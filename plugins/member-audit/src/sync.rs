//! Reading characters from ESI, each section on its own clock.
//!
//! A run may make 100 ESI calls (90 are used, the rest kept back). Each
//! character's sections (location, wallet, skills, mail, assets and so on)
//! fall due at their own intervals, set near ESI's cache times: location
//! and wallet every half hour, the journal, contracts and industry hourly,
//! assets every two hours, contacts and blueprints every six, corporation
//! history and roles daily (roles only when the Settings turn them on, as
//! aa-memberaudit's `MEMBERAUDIT_FEATURE_ROLES_ENABLED`). A run takes the most overdue first (a new
//! character's whole sheet before anything else), and while work is left
//! it queues a follow-up run a minute later. So a large alliance is read
//! steadily rather than all at once, and nothing is read more often than
//! ESI would answer anew.
//!
//! "Update now" (`update_character`) reads one character's every section
//! at once, except those read in the last few minutes.

use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tether_plugin_sdk::esi::{self, Error as EsiError, Subject};
use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};

use crate::{clip, int, opt_int, plain_text, retry, rfc3339, text};

/// The host allows 100 ESI calls per run; stop short.
const ESI_BUDGET: usize = 90;
/// Calls kept back each run for names.
const NAME_RESERVE: usize = 8;
/// The follow-up run while sections are still due.
pub(crate) const MORE: &str = "sync_more";
/// One character's "Update now".
pub(crate) const UPDATE: &str = "update_character";
/// Longest journal description, contract title or mail subject kept.
const MAX_SHORT: usize = 200;
/// Longest mail body kept, in characters.
const MAX_BODY: usize = 20_000;
/// Mail bodies read per character per run (one call each).
const BODIES_PER_RUN: usize = 15;
/// Contract items and killmails read per character per run.
const DETAILS_PER_RUN: usize = 5;
/// Pages read of a paged endpoint, at most.
const MAX_PAGES: u32 = 20;
/// Sections read within this many minutes aren't read again by Update now.
const FRESH_MINUTES: i64 = 5;

/// A part of the character sheet, read on its own clock.
pub(crate) struct Section {
    pub name: &'static str,
    /// Minutes between reads.
    pub every: i64,
    /// ESI calls it takes at least.
    pub calls: usize,
}

const fn section(name: &'static str, every: i64, calls: usize) -> Section {
    Section { name, every, calls }
}

/// In order of importance (a new character gets these first).
pub(crate) const SECTIONS: &[Section] = &[
    section("skills", 60, 2),
    section("location", 30, 2),
    section("wallet", 30, 1),
    section("public", 1440, 1),
    section("clones", 60, 2),
    section("assets", 120, 1),
    section("journal", 60, 1),
    section("mail", 30, 1),
    section("mail_meta", 360, 2),
    section("transactions", 60, 1),
    section("contracts", 60, 1),
    section("orders", 60, 1),
    section("industry", 60, 1),
    section("history", 1440, 1),
    section("attributes", 1440, 1),
    section("roles", 1440, 1),
    section("titles", 1440, 1),
    section("contacts", 360, 1),
    section("standings", 360, 1),
    section("loyalty", 360, 1),
    section("planets", 360, 1),
    section("blueprints", 360, 1),
    section("killmails", 360, 1),
    section("mining", 360, 1),
];

fn section_named(name: &str) -> Option<&'static Section> {
    SECTIONS.iter().find(|s| s.name == name)
}

/// Why a read stopped.
enum Stop {
    /// This section failed (its answer unreadable, or storing it): recorded,
    /// and the run goes on.
    Section(String),
    /// ESI answered this status for this section: as `Section`, except
    /// that readers keeping a thing's answer for good (a mail's body, a
    /// contract's items, a place's name) take only a final one as such.
    Esi(u16, String),
    /// The character's token or registration: its other sections wait.
    Character(String),
    /// Out of calls: the run ends.
    Run,
    /// ESI (or the host) unavailable: the run ends, and this section goes
    /// to the back of the queue, so one that always fails can't hold up
    /// the rest.
    Unavailable,
}

impl From<EsiError> for Stop {
    fn from(err: EsiError) -> Self {
        match err {
            EsiError::Token | EsiError::NotRegistered => Stop::Character(esi::describe(&err)),
            EsiError::Unavailable => Stop::Unavailable,
            // Over the host's limit after all (an endpoint that cost two).
            EsiError::Invalid(why) if why.contains("ESI calls") => Stop::Run,
            EsiError::Status(code) => Stop::Esi(code, esi::describe(&err)),
            other => Stop::Section(esi::describe(&other)),
        }
    }
}

impl Stop {
    /// ESI says it's gone for good (404, 410: deleted, or kept no longer).
    /// Anything else, a server error or a refusal to slow down included,
    /// may pass.
    fn gone(&self) -> bool {
        matches!(self, Stop::Esi(404 | 410, _))
    }

    /// ESI's answer is final for what was asked: a client error (403: not
    /// this character's to see; 404: gone), not one that passes (420 or
    /// 429: slow down; 5xx: ESI's trouble).
    fn refused(&self) -> bool {
        matches!(self, Stop::Esi(code, _) if (400..500).contains(code) && !matches!(code, 420 | 429))
    }
}

/// A run's state: calls left, and ids met that need names.
struct Run {
    settings: crate::settings::Settings,
    calls: usize,
    ids: Vec<i64>,
    /// Structures met, and a character who may see each.
    structures: Vec<(i64, i64)>,
}

impl Run {
    fn get(
        &mut self,
        endpoint: &str,
        character: i64,
        params: &[(&str, String)],
        page: Option<u32>,
    ) -> Result<esi::Response, Stop> {
        if self.calls == 0 {
            return Err(Stop::Run);
        }
        self.calls -= 1;
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        Ok(esi::get(
            endpoint,
            Subject::Character(character),
            &params,
            page,
        )?)
    }

    fn json<T: for<'de> Deserialize<'de>>(
        &mut self,
        endpoint: &str,
        character: i64,
        params: &[(&str, String)],
    ) -> Result<T, Stop> {
        let body = self.get(endpoint, character, params, None)?.body;
        serde_json::from_str(&body).map_err(|e| Stop::Section(format!("{endpoint}: {e}")))
    }

    /// Every page's items, and whether that was all of them.
    fn pages(&mut self, endpoint: &str, character: i64) -> Result<(Vec<Json>, bool), Stop> {
        let first = self.get(endpoint, character, &[], Some(1))?;
        let mut whole = first.pages <= MAX_PAGES;
        let mut items = array(&first.body, &mut whole);
        for page in 2..=first.pages.min(MAX_PAGES) {
            let body = self.get(endpoint, character, &[], Some(page))?.body;
            items.extend(array(&body, &mut whole));
        }
        Ok((items, whole))
    }

    fn location(&mut self, id: Option<i64>, character: i64) {
        match id {
            Some(id) if is_structure(id) => self.structures.push((id, character)),
            Some(id) if id > 0 => self.ids.push(id),
            _ => {}
        }
    }
}

fn array(body: &str, whole: &mut bool) -> Vec<Json> {
    match serde_json::from_str::<Json>(body) {
        Ok(Json::Array(items)) => items,
        _ => {
            *whole = false;
            Vec::new()
        }
    }
}

/// Upwell structures' ids (names can't name them).
fn is_structure(id: i64) -> bool {
    id >= 1_000_000_000_000
}

fn store(statements: &[Statement]) -> Result<(), Stop> {
    storage::transaction(statements)
        .map(|_| ())
        .map_err(|e| Stop::Section(format!("storing: {e:?}")))
}

fn stmt(sql: &str, params: Vec<Db>) -> Statement {
    Statement::new(sql, params)
}

fn rows(items: Vec<Json>) -> Db {
    Db::json(Json::Array(items).to_string())
}

/// Rows per insert: the host takes 1 MiB of parameters per call.
const CHUNK: usize = 1000;

/// Inserts `items` with `sql` (`$1` the rows as JSON, `$2` the character)
/// in pieces the host takes, the first in one transaction with `before`
/// (the delete of what they replace).
fn insert_all(before: Vec<Statement>, sql: &str, items: &[Json], id: i64) -> Result<(), Stop> {
    let mut chunks = items.chunks(CHUNK);
    let mut first = before;
    if let Some(chunk) = chunks.next() {
        first.push(stmt(sql, vec![rows(chunk.to_vec()), id.into()]));
    }
    store(&first)?;
    for chunk in chunks {
        store(&[stmt(sql, vec![rows(chunk.to_vec()), id.into()])])?;
    }
    Ok(())
}

fn i(v: &Json) -> Option<i64> {
    v.as_i64()
}

/// A scheduled or follow-up run (`only` none), or one character's Update
/// now.
pub(crate) fn run(only: Option<i64>) -> Result<(), JobError> {
    let settings = crate::settings::get().map_err(|e| retry("reading settings", e))?;
    if only.is_none() {
        refresh_characters(&settings)?;
    }
    let mut run = Run {
        settings,
        calls: ESI_BUDGET - NAME_RESERVE,
        ids: Vec::new(),
        structures: Vec::new(),
    };
    let due = due(only, settings.roles)?;
    let mut skipped: Vec<i64> = Vec::new();
    let mut left = false;
    for (character, name) in &due {
        if skipped.contains(character) {
            continue;
        }
        let Some(section) = section_named(name) else {
            continue;
        };
        if run.calls < section.calls {
            left = true;
            continue;
        }
        match read(&mut run, *character, section.name) {
            Ok(()) => record(*character, section.name, None),
            Err(Stop::Section(why) | Stop::Esi(_, why)) => {
                log::warn(format!("character {character}, {}: {why}", section.name));
                record(*character, section.name, Some(&why));
            }
            Err(Stop::Character(why)) => {
                log::warn(format!("character {character}: {why}"));
                // Its turn is used: its due sections wait their interval.
                for (c, n) in &due {
                    if c == character {
                        record(*c, n, Some(&why));
                    }
                }
                skipped.push(*character);
            }
            Err(Stop::Run) => {
                left = true;
                break;
            }
            Err(Stop::Unavailable) => {
                log::warn(format!(
                    "character {character}, {}: ESI unavailable; the rest waits for the next run",
                    section.name
                ));
                record(*character, section.name, Some("ESI was unavailable"));
                left = true;
                break;
            }
        }
    }
    run.calls += NAME_RESERVE;
    name_places(&mut run)?;
    learn_names(&mut run)?;
    if only.is_none() && run.calls >= 30 {
        learn_skill_groups(&mut run)?;
    }
    if let Some(character) = only {
        let _ = storage::execute(
            "UPDATE characters SET update_done_at = now() WHERE character_id = $1",
            &[character.into()],
        );
    } else if left {
        // More is due than a run may read: carry on shortly (queuing
        // again under the key replaces a waiting one).
        let at = rfc3339(Utc::now() + chrono::Duration::minutes(1));
        jobs::enqueue(NewJob::new(MORE).key(MORE).at(at))
            .map_err(|e| retry("queuing the next run", e))?;
    }
    Ok(())
}

/// The host's list of characters (registered with the app's scopes, by
/// pilots holding one of its permissions): new ones added, those gone
/// forgotten with all their data. History older than the Settings keep
/// goes, and shares past the sharing timeout end.
fn refresh_characters(settings: &crate::settings::Settings) -> Result<(), JobError> {
    let characters = esi::characters();
    let list: Vec<Json> = characters
        .iter()
        .map(|c| {
            json!({
                "character_id": c.id, "name": c.name,
                "corporation_id": c.corporation_id, "alliance_id": c.alliance_id,
            })
        })
        .collect();
    // When this list was taken: whoever it doesn't hold is forgotten below.
    let began = storage::query("SELECT now()", &[])
        .map_err(|e| retry("reading the time", e))?
        .rows
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .ok_or_else(|| JobError::Retry("reading the time: no answer".to_owned()))?;
    // In pieces the host takes (a large alliance's list runs past the
    // parameters one call may carry), the first with the clean-up below.
    let upsert = |chunk: &[Json]| {
        stmt(
            "INSERT INTO characters (character_id, name, corporation_id, alliance_id, seen_at) \
             SELECT character_id, name, corporation_id, alliance_id, now() \
             FROM json_to_recordset($1::json) AS x(character_id bigint, name text, corporation_id bigint, alliance_id bigint) \
             ON CONFLICT (character_id) DO UPDATE SET name = EXCLUDED.name, \
             corporation_id = EXCLUDED.corporation_id, alliance_id = EXCLUDED.alliance_id, seen_at = now()",
            vec![rows(chunk.to_vec())],
        )
    };
    let mut chunks = list.chunks(CHUNK);
    let mut first: Vec<Statement> = chunks.next().map(upsert).into_iter().collect();
    first.extend([
        // aa-memberaudit's MEMBERAUDIT_DATA_RETENTION_LIMIT: mail,
        // contracts and wallet history.
        stmt(
            "DELETE FROM journal WHERE at < now() - make_interval(days => $1::int)",
            vec![settings.retention_days.into()],
        ),
        stmt(
            "DELETE FROM transactions WHERE at < now() - make_interval(days => $1::int)",
            vec![settings.retention_days.into()],
        ),
        stmt(
            "DELETE FROM mails WHERE at < now() - make_interval(days => $1::int)",
            vec![settings.retention_days.into()],
        ),
        stmt(
            "DELETE FROM contracts WHERE issued < now() - make_interval(days => $1::int)",
            vec![settings.retention_days.into()],
        ),
        // MEMBERAUDIT_SHARING_TIMEOUT (0: until unshared).
        stmt(
            "UPDATE characters SET is_shared = false, shared_at = NULL, shared_by_main = NULL \
             WHERE is_shared AND $1::int > 0 AND shared_at < now() - make_interval(mins => $1::int)",
            vec![settings.sharing_timeout_minutes.into()],
        ),
        // Roles off: none kept, even from a run that began while they were
        // on.
        stmt(
            "DELETE FROM roles WHERE NOT (SELECT roles_enabled FROM settings WHERE id = 1)",
            vec![],
        ),
        stmt(
            "DELETE FROM mining WHERE day < current_date - 90",
            vec![],
        ),
    ]);
    storage::transaction(&first).map_err(|e| retry("storing characters", e))?;
    for chunk in chunks {
        storage::transaction(&[upsert(chunk)]).map_err(|e| retry("storing characters", e))?;
    }
    // Not in the list any more (left, sold, the account holds none of the
    // app's permissions any more, consent withdrawn): forgotten at once,
    // with all its data, once the whole list is stored. An empty list may
    // be the host having trouble, so on that alone nothing is forgotten
    // for a day; empty for longer, it's real (the last registered pilot
    // left, or the app's scopes went), and everything goes.
    let forget = if characters.is_empty() {
        stmt(
            "DELETE FROM characters WHERE seen_at < now() - interval '1 day'",
            vec![],
        )
    } else {
        stmt(
            "DELETE FROM characters WHERE seen_at < $1::timestamptz",
            vec![began],
        )
    };
    storage::transaction(&[
        forget,
        stmt(
            "DELETE FROM update_asks WHERE at < now() - interval '1 hour'",
            vec![],
        ),
    ])
    .map_err(|e| retry("forgetting characters", e))?;
    Ok(())
}

/// Sections due, most urgent first: a character never read at all goes
/// first, whole; then whatever is most overdue. Roles only when `roles`.
fn due(only: Option<i64>, roles: bool) -> Result<Vec<(i64, String)>, JobError> {
    let sections: Vec<Json> = SECTIONS
        .iter()
        .enumerate()
        .filter(|(_, s)| roles || s.name != "roles")
        .map(|(rank, s)| json!({ "name": s.name, "every": s.every, "rank": rank }))
        .collect();
    let rows = match only {
        Some(character) => storage::query(
            "SELECT c.character_id, s.name FROM characters c \
             CROSS JOIN json_to_recordset($1::json) AS s(name text, every int, rank int) \
             LEFT JOIN section_syncs y ON y.character_id = c.character_id AND y.section = s.name \
             WHERE c.character_id = $2 \
               AND (y.synced_at IS NULL OR y.synced_at < now() - make_interval(mins => $3::int)) \
             ORDER BY s.rank",
            &[rows(sections), character.into(), FRESH_MINUTES.into()],
        ),
        None => storage::query(
            "SELECT c.character_id, s.name FROM characters c \
             CROSS JOIN json_to_recordset($1::json) AS s(name text, every int, rank int) \
             LEFT JOIN section_syncs y ON y.character_id = c.character_id AND y.section = s.name \
             WHERE c.seen_at > now() - interval '1 hour' \
               AND (y.synced_at IS NULL OR y.synced_at < now() - make_interval(mins => s.every)) \
             ORDER BY c.synced_at IS NOT NULL, \
                      CASE WHEN c.synced_at IS NULL THEN c.character_id END, \
                      y.synced_at + make_interval(mins => s.every) NULLS FIRST, s.rank \
             LIMIT 400",
            &[rows(sections)],
        ),
    }
    .map_err(|e| retry("choosing what to read", e))?;
    Ok(rows.rows.iter().map(|r| (int(r, 0), text(r, 1))).collect())
}

fn record(character: i64, section: &str, error: Option<&str>) {
    let result = storage::transaction(&[
        stmt(
            "INSERT INTO section_syncs (character_id, section, synced_at, ok, error) \
             VALUES ($1, $2, now(), $3, $4) \
             ON CONFLICT (character_id, section) DO UPDATE SET synced_at = now(), ok = EXCLUDED.ok, \
             error = EXCLUDED.error",
            vec![
                character.into(),
                section.into(),
                error.is_none().into(),
                error.map(|e| clip(e, MAX_SHORT)).into(),
            ],
        ),
        // A character counts as read once anything of it was.
        stmt(
            "UPDATE characters SET synced_at = now() WHERE character_id = $1",
            vec![character.into()],
        ),
    ]);
    if let Err(err) = result {
        log::warn(format!("recording a read: {err:?}"));
    }
}

fn read(run: &mut Run, id: i64, section: &str) -> Result<(), Stop> {
    match section {
        "skills" => skills(run, id),
        "location" => location(run, id),
        "wallet" => wallet(run, id),
        "public" => public(run, id),
        "clones" => clones(run, id),
        "assets" => assets(run, id),
        "journal" => journal(run, id),
        "mail" => mail(run, id),
        "mail_meta" => mail_meta(run, id),
        "transactions" => transactions(run, id),
        "contracts" => contracts(run, id),
        "orders" => orders(run, id),
        "industry" => industry(run, id),
        "history" => history(run, id),
        "attributes" => attributes(run, id),
        "roles" => roles(run, id),
        "titles" => titles(run, id),
        "contacts" => contacts(run, id),
        "standings" => standings(run, id),
        "loyalty" => loyalty(run, id),
        "planets" => planets(run, id),
        "blueprints" => blueprints(run, id),
        "killmails" => killmails(run, id),
        "mining" => mining(run, id),
        other => Err(Stop::Section(format!("no section {other}"))),
    }
}

// ---- sections ------------------------------------------------------------

#[derive(Deserialize)]
struct Skills {
    skills: Vec<Json>,
    total_sp: i64,
    #[serde(default)]
    unallocated_sp: Option<i64>,
}

fn skills(run: &mut Run, id: i64) -> Result<(), Stop> {
    let skills: Skills = run.json("character-skills", id, &[])?;
    let queue: Vec<Json> = run.json("character-skillqueue", id, &[])?;
    run.ids
        .extend(skills.skills.iter().filter_map(|s| i(&s["skill_id"])));
    run.ids
        .extend(queue.iter().filter_map(|s| i(&s["skill_id"])));
    store(&[
        stmt(
            "UPDATE characters SET total_sp = $2, unallocated_sp = $3, skills_at = now() \
             WHERE character_id = $1",
            vec![
                id.into(),
                skills.total_sp.into(),
                skills.unallocated_sp.into(),
            ],
        ),
        stmt(
            "DELETE FROM skills WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO skills (character_id, skill_id, active_level, trained_level, sp) \
             SELECT $2, skill_id, active_skill_level, trained_skill_level, skillpoints_in_skill \
             FROM json_to_recordset($1::json) AS x(skill_id bigint, active_skill_level int, \
                  trained_skill_level int, skillpoints_in_skill bigint)",
            vec![rows(skills.skills), id.into()],
        ),
        stmt("DELETE FROM queue WHERE character_id = $1", vec![id.into()]),
        stmt(
            "INSERT INTO queue (character_id, position, skill_id, level, finish, start, \
                                level_start_sp, level_end_sp, training_start_sp) \
             SELECT $2, queue_position, skill_id, finished_level, finish_date, start_date, \
                    level_start_sp, level_end_sp, training_start_sp \
             FROM json_to_recordset($1::json) AS x(queue_position int, skill_id bigint, finished_level int, \
                  finish_date timestamptz, start_date timestamptz, level_start_sp bigint, \
                  level_end_sp bigint, training_start_sp bigint) \
             ON CONFLICT DO NOTHING",
            vec![rows(queue), id.into()],
        ),
    ])
}

#[derive(Deserialize)]
struct Location {
    solar_system_id: i64,
    #[serde(default)]
    station_id: Option<i64>,
    #[serde(default)]
    structure_id: Option<i64>,
}

#[derive(Deserialize)]
struct Ship {
    ship_type_id: i64,
    ship_name: String,
}

fn location(run: &mut Run, id: i64) -> Result<(), Stop> {
    let location: Location = run.json("character-location", id, &[])?;
    let ship: Ship = run.json("character-ship", id, &[])?;
    run.ids.push(location.solar_system_id);
    run.ids.push(ship.ship_type_id);
    let place = location.station_id.or(location.structure_id);
    run.location(place, id);
    let kind = if location.station_id.is_some() {
        "station"
    } else if location.structure_id.is_some() {
        "structure"
    } else {
        "space"
    };
    store(&[stmt(
        "UPDATE characters SET system_id = $2, location_id = $3, location_type = $4, \
         ship_type_id = $5, ship_name = $6 WHERE character_id = $1",
        vec![
            id.into(),
            location.solar_system_id.into(),
            place.into(),
            kind.into(),
            ship.ship_type_id.into(),
            clip(&ship.ship_name, MAX_SHORT).into(),
        ],
    )])
}

fn wallet(run: &mut Run, id: i64) -> Result<(), Stop> {
    let balance: f64 = run.json("character-wallet", id, &[])?;
    store(&[stmt(
        "UPDATE characters SET wallet = $2 WHERE character_id = $1",
        vec![id.into(), balance.into()],
    )])
}

fn public(run: &mut Run, id: i64) -> Result<(), Stop> {
    let sheet: Json = run.json("character-public", id, &[("character_id", id.to_string())])?;
    let faction = i(&sheet["faction_id"]);
    run.ids.extend(faction);
    let bio = sheet["description"]
        .as_str()
        .map(plain_text)
        .map(|b| clip(&b, 2000));
    store(&[stmt(
        "UPDATE characters SET birthday = $2::timestamptz, security_status = $3, faction_id = $4, bio = $5 \
         WHERE character_id = $1",
        vec![
            id.into(),
            sheet["birthday"].as_str().map(str::to_owned).into(),
            sheet["security_status"].as_f64().into(),
            faction.into(),
            bio.into(),
        ],
    )])
}

fn clones(run: &mut Run, id: i64) -> Result<(), Stop> {
    let clones: Json = run.json("character-clones", id, &[])?;
    let implants: Vec<i64> = run.json("character-implants", id, &[])?;
    let jump_clones: Vec<Json> = clones["jump_clones"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|c| {
            let implants: Vec<i64> = c["implants"]
                .as_array()
                .map(|a| a.iter().filter_map(i).collect())
                .unwrap_or_default();
            run.ids.extend(implants.iter().copied());
            run.location(i(&c["location_id"]), id);
            json!({
                "jump_clone_id": c["jump_clone_id"], "location_id": c["location_id"],
                "implants": implants,
            })
        })
        .collect();
    run.ids.extend(implants.iter().copied());
    let home = i(&clones["home_location"]["location_id"]);
    run.location(home, id);
    let implant_rows: Vec<Json> = implants.iter().map(|t| json!({ "type_id": t })).collect();
    store(&[
        stmt(
            "UPDATE characters SET home_location_id = $2, last_clone_jump = $3::timestamptz, \
             last_station_change = $4::timestamptz WHERE character_id = $1",
            vec![
                id.into(),
                home.into(),
                clones["last_clone_jump_date"]
                    .as_str()
                    .map(str::to_owned)
                    .into(),
                clones["last_station_change_date"]
                    .as_str()
                    .map(str::to_owned)
                    .into(),
            ],
        ),
        stmt(
            "DELETE FROM clones WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO clones (character_id, jump_clone_id, location_id, implants) \
             SELECT $2, jump_clone_id, location_id, implants \
             FROM json_to_recordset($1::json) AS x(jump_clone_id bigint, location_id bigint, implants jsonb)",
            vec![rows(jump_clones), id.into()],
        ),
        stmt(
            "DELETE FROM implants WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO implants (character_id, type_id) \
             SELECT $2, type_id FROM json_to_recordset($1::json) AS x(type_id bigint) ON CONFLICT DO NOTHING",
            vec![rows(implant_rows), id.into()],
        ),
    ])
}

fn assets(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, whole) = run.pages("character-assets", id)?;
    for a in &items {
        run.ids.extend(i(&a["type_id"]));
        match a["location_type"].as_str() {
            Some("station") | Some("solar_system") => run.ids.extend(i(&a["location_id"])),
            _ if a["location_flag"] == "Hangar" => run.location(i(&a["location_id"]), id),
            _ => {}
        }
    }
    let items: Vec<Json> = items
        .iter()
        .map(|a| {
            json!({
                "item_id": a["item_id"], "type_id": a["type_id"], "quantity": a["quantity"],
                "location_id": a["location_id"], "location_flag": a["location_flag"],
                "location_type": a["location_type"],
            })
        })
        .collect();
    insert_all(
        vec![
            stmt(
                "UPDATE characters SET assets_at = NULL WHERE character_id = $1",
                vec![id.into()],
            ),
            stmt(
                "DELETE FROM assets WHERE character_id = $1",
                vec![id.into()],
            ),
        ],
        "INSERT INTO assets (character_id, item_id, type_id, quantity, location_id, location_flag, location_type) \
         SELECT $2, item_id, type_id, quantity, location_id, location_flag, location_type \
         FROM json_to_recordset($1::json) AS x(item_id bigint, type_id bigint, quantity bigint, \
              location_id bigint, location_flag text, location_type text) \
         ON CONFLICT DO NOTHING",
        &items,
        id,
    )?;
    // The Secure Groups asset filter answers only for complete assets.
    if whole {
        store(&[stmt(
            "UPDATE characters SET assets_at = now() WHERE character_id = $1",
            vec![id.into()],
        )])
    } else {
        // What was read stays, and the sheet says it's only part.
        Err(Stop::Section(format!(
            "only part of the assets was read, so this list is incomplete (more than \
             {MAX_PAGES} pages of them, or a page ESI sent garbled)"
        )))
    }
}

/// The wallet journal: ESI's last 30 days, newest first, in pages, read
/// down to the first page with nothing new and stored a page at a time (a
/// busy trader's pages all at once don't fit a run's memory). Below its
/// newest entry what's stored has no gaps, unless `journal_gap` says it
/// may (marked with the first page a read stores, cleared once a read gets
/// down to what's stored, past what's kept or to ESI's last page): then
/// reads go down every page, as aa-memberaudit reads them, and the sheet
/// says the journal is partial until one gets through.
fn journal(run: &mut Run, id: i64) -> Result<(), Stop> {
    // Older than the Settings keep (aa-memberaudit's
    // MEMBERAUDIT_DATA_RETENTION_LIMIT) is never new: it goes at the next
    // clean-up.
    let cutoff = Utc::now() - chrono::Duration::days(run.settings.retention_days);
    let reading = |e| Stop::Section(format!("reading the journal: {e:?}"));
    let gap = storage::query(
        "SELECT journal_gap FROM characters WHERE character_id = $1",
        &[id.into()],
    )
    .map_err(reading)?
    .rows
    .first()
    .is_some_and(|r| crate::boolean(r, 0));
    let mut marked = gap;
    let first = run.get("character-wallet-journal", id, &[], Some(1))?;
    let pages = first.pages.clamp(1, MAX_PAGES);
    let mut body = first.body;
    let mut page = 1;
    let whole = loop {
        let mut readable = true;
        let items = array(&std::mem::take(&mut body), &mut readable);
        if !readable {
            break false;
        }
        let date = |j: &Json| j["date"].as_str().and_then(crate::parse_time);
        let kept: Vec<i64> = items
            .iter()
            .filter(|j| date(j).is_some_and(|at| at >= cutoff))
            .filter_map(|j| i(&j["id"]))
            .collect();
        // Past what's kept: older pages hold nothing new.
        let past = items.iter().any(|j| date(j).is_some_and(|at| at < cutoff));
        let known = if kept.is_empty() {
            0
        } else {
            storage::query(
                "SELECT count(*) FROM journal WHERE character_id = $1 \
                 AND id = ANY(string_to_array($2, ',')::bigint[])",
                &[id.into(), crate::id_list(&kept).into()],
            )
            .map_err(reading)?
            .rows
            .first()
            .map_or(0, |r| int(r, 0))
        };
        let new = usize::try_from(known).unwrap_or(0) < kept.len();
        // This page ends the read whole: no gap to mark.
        let last = past || (page >= pages && first.pages <= MAX_PAGES);
        if new {
            let mut mark = Vec::new();
            if !marked && !last {
                mark.push(stmt(
                    "UPDATE characters SET journal_gap = true WHERE character_id = $1",
                    vec![id.into()],
                ));
                marked = true;
            }
            store_journal(run, id, items, mark)?;
        }
        if (!new && !gap) || past {
            break true;
        }
        if page >= pages {
            break first.pages <= MAX_PAGES;
        }
        page += 1;
        body = run
            .get("character-wallet-journal", id, &[], Some(page))?
            .body;
    };
    if !whole {
        return Err(Stop::Section(format!(
            "only part of the journal was read, so older entries may be missing (more than \
             {MAX_PAGES} pages of them, or a page ESI sent garbled)"
        )));
    }
    if marked {
        store(&[stmt(
            "UPDATE characters SET journal_gap = false WHERE character_id = $1",
            vec![id.into()],
        )])?;
    }
    Ok(())
}

/// Stores a page of journal entries, the first piece with `before`.
fn store_journal(
    run: &mut Run,
    id: i64,
    items: Vec<Json>,
    before: Vec<Statement>,
) -> Result<(), Stop> {
    let entries: Vec<Json> = items
        .into_iter()
        .map(|j| {
            run.ids.extend(i(&j["first_party_id"]));
            run.ids.extend(i(&j["second_party_id"]));
            run.ids.extend(i(&j["tax_receiver_id"]));
            json!({
                "id": j["id"], "date": j["date"], "ref_type": j["ref_type"],
                "amount": j["amount"], "balance": j["balance"],
                "description": clip(j["description"].as_str().unwrap_or_default(), MAX_SHORT),
                "first_party_id": j["first_party_id"], "second_party_id": j["second_party_id"],
                "context_id": j["context_id"], "context_id_type": j["context_id_type"],
                "tax": j["tax"], "tax_receiver_id": j["tax_receiver_id"],
                "reason": clip(j["reason"].as_str().unwrap_or_default(), MAX_SHORT),
            })
        })
        .collect();
    // A trader's parties repeat: each named once.
    run.ids.sort_unstable();
    run.ids.dedup();
    insert_all(
        before,
        "INSERT INTO journal (character_id, id, at, ref_type, amount, balance, description, \
                              first_party_id, second_party_id, context_id, context_id_type, tax, \
                              tax_receiver_id, reason) \
         SELECT $2, id, date, ref_type, amount, balance, coalesce(description, ''), first_party_id, \
                second_party_id, context_id, context_id_type, tax, tax_receiver_id, \
                coalesce(reason, '') \
         FROM json_to_recordset($1::json) AS x(id bigint, date timestamptz, ref_type text, \
              amount double precision, balance double precision, description text, \
              first_party_id bigint, second_party_id bigint, context_id bigint, \
              context_id_type text, tax double precision, tax_receiver_id bigint, reason text) \
         ON CONFLICT (character_id, id) DO NOTHING",
        &entries,
        id,
    )
}

fn transactions(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-wallet-transactions", id, &[])?;
    for t in &items {
        run.ids.extend(i(&t["type_id"]));
        run.ids.extend(i(&t["client_id"]));
        run.location(i(&t["location_id"]), id);
    }
    insert_all(
        vec![],
        "INSERT INTO transactions (character_id, id, at, type_id, quantity, unit_price, client_id, \
                                   location_id, is_buy, is_personal) \
         SELECT $2, transaction_id, date, type_id, quantity, unit_price, client_id, location_id, is_buy, is_personal \
         FROM json_to_recordset($1::json) AS x(transaction_id bigint, date timestamptz, type_id bigint, \
              quantity bigint, unit_price double precision, client_id bigint, location_id bigint, \
              is_buy boolean, is_personal boolean) \
         ON CONFLICT (character_id, id) DO NOTHING",
        &items,
        id,
    )
}

fn contracts(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, _) = run.pages("character-contracts", id)?;
    let items: Vec<Json> = items
        .into_iter()
        .map(|c| {
            for key in [
                "issuer_id",
                "issuer_corporation_id",
                "assignee_id",
                "acceptor_id",
            ] {
                run.ids.extend(i(&c[key]).filter(|id| *id > 0));
            }
            for key in ["start_location_id", "end_location_id"] {
                run.location(i(&c[key]), id);
            }
            let mut c = c;
            if let Some(title) = c["title"].as_str() {
                c["title"] = Json::String(clip(title, MAX_SHORT));
            }
            c
        })
        .collect();
    insert_all(
        vec![],
        "INSERT INTO contracts (character_id, contract_id, kind, status, availability, issuer_id, \
             assignee_id, acceptor_id, issued, expires, completed, title, price, reward, collateral, \
             volume, start_location_id, end_location_id, accepted, issuer_corporation_id, \
             days_to_complete, buyout) \
         SELECT $2, contract_id, type, status, availability, issuer_id, assignee_id, acceptor_id, \
             date_issued, date_expired, date_completed, coalesce(title, ''), price, reward, collateral, \
             volume, start_location_id, end_location_id, date_accepted, issuer_corporation_id, \
             days_to_complete, buyout \
         FROM json_to_recordset($1::json) AS x(contract_id bigint, type text, status text, \
             availability text, issuer_id bigint, assignee_id bigint, acceptor_id bigint, \
             date_issued timestamptz, date_expired timestamptz, date_completed timestamptz, \
             title text, price double precision, reward double precision, \
             collateral double precision, volume double precision, start_location_id bigint, \
             end_location_id bigint, date_accepted timestamptz, issuer_corporation_id bigint, \
             days_to_complete integer, buyout double precision) \
         ON CONFLICT (character_id, contract_id) DO UPDATE SET status = EXCLUDED.status, \
             acceptor_id = EXCLUDED.acceptor_id, completed = EXCLUDED.completed, \
             accepted = EXCLUDED.accepted",
        &items,
        id,
    )?;
    // Items of item exchanges and auctions, a few new ones a run (those
    // ESI failed to give last time after the others).
    let pending = storage::query(
        "SELECT contract_id FROM contracts WHERE character_id = $1 AND NOT items_read \
         AND kind IN ('item_exchange', 'auction') ORDER BY items_tried_at NULLS FIRST, issued DESC LIMIT $2",
        &[id.into(), (DETAILS_PER_RUN as i64).into()],
    )
    .map_err(|e| Stop::Section(format!("reading contracts: {e:?}")))?;
    for row in &pending.rows {
        let contract = int(row, 0);
        let items = match run.json::<Vec<Json>>(
            "character-contract-items",
            id,
            &[("contract_id", contract.to_string())],
        ) {
            Ok(items) => items,
            // ESI keeps items only a while: gone is gone.
            Err(stop) if stop.gone() => Vec::new(),
            // A passing failure: asked again on a later read.
            Err(Stop::Section(why) | Stop::Esi(_, why)) => {
                log::warn(format!(
                    "character {id}, contract {contract}'s items: {why}"
                ));
                store(&[stmt(
                    "UPDATE contracts SET items_tried_at = now() \
                     WHERE character_id = $1 AND contract_id = $2",
                    vec![id.into(), contract.into()],
                )])?;
                continue;
            }
            Err(other) => return Err(other),
        };
        run.ids
            .extend(items.iter().filter_map(|x| i(&x["type_id"])));
        store(&[
            stmt(
                "INSERT INTO contract_items (character_id, contract_id, record_id, type_id, quantity, \
                     is_included, is_singleton, raw_quantity) \
                 SELECT $2, $3, record_id, type_id, quantity, is_included, \
                     coalesce(is_singleton, false), raw_quantity \
                 FROM json_to_recordset($1::json) AS x(record_id bigint, type_id bigint, quantity bigint, \
                     is_included boolean, is_singleton boolean, raw_quantity bigint) \
                 ON CONFLICT DO NOTHING",
                vec![rows(items), id.into(), contract.into()],
            ),
            stmt(
                "UPDATE contracts SET items_read = true WHERE character_id = $1 AND contract_id = $2",
                vec![id.into(), contract.into()],
            ),
        ])?;
    }
    Ok(())
}

fn orders(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-orders", id, &[])?;
    for o in &items {
        run.ids.extend(i(&o["type_id"]));
        run.location(i(&o["location_id"]), id);
    }
    store(&[
        stmt(
            "DELETE FROM orders WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO orders (character_id, order_id, type_id, is_buy, price, volume_total, \
                                 volume_remain, location_id, issued, duration, is_corporation) \
             SELECT $2, order_id, type_id, coalesce(is_buy_order, false), price, volume_total, \
                    volume_remain, location_id, issued, duration, is_corporation \
             FROM json_to_recordset($1::json) AS x(order_id bigint, type_id bigint, is_buy_order boolean, \
                  price double precision, volume_total bigint, volume_remain bigint, location_id bigint, \
                  issued timestamptz, duration int, is_corporation boolean) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn industry(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-industry-jobs", id, &[])?;
    for j in &items {
        run.ids.extend(i(&j["blueprint_type_id"]));
        run.ids.extend(i(&j["product_type_id"]));
        run.location(i(&j["facility_id"]), id);
    }
    store(&[
        stmt(
            "DELETE FROM industry_jobs WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO industry_jobs (character_id, job_id, activity_id, blueprint_type_id, \
                 product_type_id, runs, status, facility_id, starts, ends, cost) \
             SELECT $2, job_id, activity_id, blueprint_type_id, product_type_id, runs, status, \
                 facility_id, start_date, end_date, cost \
             FROM json_to_recordset($1::json) AS x(job_id bigint, activity_id int, blueprint_type_id bigint, \
                  product_type_id bigint, runs int, status text, facility_id bigint, \
                  start_date timestamptz, end_date timestamptz, cost double precision) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn history(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json(
        "character-corporation-history",
        id,
        &[("character_id", id.to_string())],
    )?;
    run.ids
        .extend(items.iter().filter_map(|h| i(&h["corporation_id"])));
    store(&[
        stmt(
            "DELETE FROM corporation_history WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO corporation_history (character_id, record_id, corporation_id, start_date, is_deleted) \
             SELECT $2, record_id, corporation_id, start_date, coalesce(is_deleted, false) \
             FROM json_to_recordset($1::json) AS x(record_id bigint, corporation_id bigint, \
                  start_date timestamptz, is_deleted boolean) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn attributes(run: &mut Run, id: i64) -> Result<(), Stop> {
    let a: Json = run.json("character-attributes", id, &[])?;
    store(&[stmt(
        "INSERT INTO attributes (character_id, charisma, intelligence, memory, perception, willpower, \
             bonus_remaps, last_remap, remap_cooldown) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::timestamptz, $9::timestamptz) \
         ON CONFLICT (character_id) DO UPDATE SET charisma = EXCLUDED.charisma, \
             intelligence = EXCLUDED.intelligence, memory = EXCLUDED.memory, \
             perception = EXCLUDED.perception, willpower = EXCLUDED.willpower, \
             bonus_remaps = EXCLUDED.bonus_remaps, last_remap = EXCLUDED.last_remap, \
             remap_cooldown = EXCLUDED.remap_cooldown",
        vec![
            id.into(),
            i(&a["charisma"]).unwrap_or_default().into(),
            i(&a["intelligence"]).unwrap_or_default().into(),
            i(&a["memory"]).unwrap_or_default().into(),
            i(&a["perception"]).unwrap_or_default().into(),
            i(&a["willpower"]).unwrap_or_default().into(),
            i(&a["bonus_remaps"]).into(),
            a["last_remap_date"].as_str().map(str::to_owned).into(),
            a["accrued_remap_cooldown_date"]
                .as_str()
                .map(str::to_owned)
                .into(),
        ],
    )])
}

fn roles(run: &mut Run, id: i64) -> Result<(), Stop> {
    let r: Json = run.json("character-roles", id, &[])?;
    let mut items = Vec::new();
    for scope in ["roles", "roles_at_hq", "roles_at_base", "roles_at_other"] {
        for role in r[scope].as_array().into_iter().flatten() {
            if let Some(role) = role.as_str() {
                items.push(json!({ "scope": scope, "role": clip(role, 100) }));
            }
        }
    }
    store(&[
        stmt("DELETE FROM roles WHERE character_id = $1", vec![id.into()]),
        stmt(
            "INSERT INTO roles (character_id, scope, role) \
             SELECT $2, scope, role FROM json_to_recordset($1::json) AS x(scope text, role text) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn titles(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-titles", id, &[])?;
    let items: Vec<Json> = items
        .iter()
        .filter_map(|t| {
            Some(json!({
                "title_id": i(&t["title_id"])?,
                "name": clip(&plain_text(t["name"].as_str().unwrap_or_default()), MAX_SHORT),
            }))
        })
        .collect();
    store(&[
        stmt(
            "DELETE FROM titles WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO titles (character_id, title_id, name) \
             SELECT $2, title_id, name FROM json_to_recordset($1::json) AS x(title_id bigint, name text) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn contacts(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, _) = run.pages("character-contacts", id)?;
    run.ids
        .extend(items.iter().filter_map(|c| i(&c["contact_id"])));
    insert_all(
        vec![stmt(
            "DELETE FROM contacts WHERE character_id = $1",
            vec![id.into()],
        )],
        "INSERT INTO contacts (character_id, contact_id, contact_type, standing, is_watched, is_blocked) \
             SELECT $2, contact_id, contact_type, standing, coalesce(is_watched, false), coalesce(is_blocked, false) \
             FROM json_to_recordset($1::json) AS x(contact_id bigint, contact_type text, \
                  standing double precision, is_watched boolean, is_blocked boolean) \
             ON CONFLICT DO NOTHING",
        &items,
        id,
    )
}

fn standings(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-standings", id, &[])?;
    // Agents aren't named by `names`; corporations and factions are.
    run.ids.extend(
        items
            .iter()
            .filter(|s| s["from_type"] != "agent")
            .filter_map(|s| i(&s["from_id"])),
    );
    store(&[
        stmt(
            "DELETE FROM standings WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO standings (character_id, from_id, from_type, standing) \
             SELECT $2, from_id, from_type, standing FROM json_to_recordset($1::json) \
             AS x(from_id bigint, from_type text, standing double precision) ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn loyalty(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-loyalty-points", id, &[])?;
    run.ids
        .extend(items.iter().filter_map(|l| i(&l["corporation_id"])));
    store(&[
        stmt(
            "DELETE FROM loyalty WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO loyalty (character_id, corporation_id, points) \
             SELECT $2, corporation_id, loyalty_points FROM json_to_recordset($1::json) \
             AS x(corporation_id bigint, loyalty_points bigint) ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn planets(run: &mut Run, id: i64) -> Result<(), Stop> {
    let items: Vec<Json> = run.json("character-planets", id, &[])?;
    run.ids
        .extend(items.iter().filter_map(|p| i(&p["solar_system_id"])));
    store(&[
        stmt(
            "DELETE FROM planets WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO planets (character_id, planet_id, solar_system_id, planet_type, upgrade_level, pins, last_update) \
             SELECT $2, planet_id, solar_system_id, planet_type, upgrade_level, num_pins, last_update \
             FROM json_to_recordset($1::json) AS x(planet_id bigint, solar_system_id bigint, \
                  planet_type text, upgrade_level int, num_pins int, last_update timestamptz) \
             ON CONFLICT DO NOTHING",
            vec![rows(items), id.into()],
        ),
    ])
}

fn blueprints(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, whole) = run.pages("character-blueprints", id)?;
    if !whole {
        return Err(Stop::Section("more blueprints than may be read".to_owned()));
    }
    for b in &items {
        run.ids.extend(i(&b["type_id"]));
    }
    insert_all(
        vec![stmt(
            "DELETE FROM blueprints WHERE character_id = $1",
            vec![id.into()],
        )],
        "INSERT INTO blueprints (character_id, item_id, type_id, location_id, location_flag, \
                 quantity, runs, material_efficiency, time_efficiency) \
             SELECT $2, item_id, type_id, location_id, location_flag, quantity, runs, \
                 material_efficiency, time_efficiency \
             FROM json_to_recordset($1::json) AS x(item_id bigint, type_id bigint, location_id bigint, \
                  location_flag text, quantity int, runs int, material_efficiency int, time_efficiency int) \
             ON CONFLICT DO NOTHING",
        &items,
        id,
    )
}

fn killmails(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, _) = run.pages("character-killmails", id)?;
    insert_all(
        vec![],
        "INSERT INTO killmails (character_id, killmail_id, hash) \
         SELECT $2, killmail_id, killmail_hash FROM json_to_recordset($1::json) \
         AS x(killmail_id bigint, killmail_hash text) ON CONFLICT DO NOTHING",
        &items,
        id,
    )?;
    let pending = storage::query(
        "SELECT killmail_id, hash FROM killmails WHERE character_id = $1 AND at IS NULL \
         ORDER BY killmail_id DESC LIMIT $2",
        &[id.into(), (DETAILS_PER_RUN as i64 * 2).into()],
    )
    .map_err(|e| Stop::Section(format!("reading killmails: {e:?}")))?;
    for row in &pending.rows {
        let (killmail, hash) = (int(row, 0), text(row, 1));
        let detail: Json = match run.json(
            "killmail-detail",
            id,
            &[
                ("killmail_id", killmail.to_string()),
                ("killmail_hash", hash),
            ],
        ) {
            Ok(detail) => detail,
            Err(Stop::Section(why) | Stop::Esi(_, why)) => {
                log::warn(format!("killmail {killmail}: {why}"));
                continue;
            }
            Err(other) => return Err(other),
        };
        let victim = &detail["victim"];
        for key in [
            "character_id",
            "corporation_id",
            "alliance_id",
            "ship_type_id",
        ] {
            run.ids.extend(i(&victim[key]));
        }
        run.ids.extend(i(&detail["solar_system_id"]));
        store(&[stmt(
            "UPDATE killmails SET at = $3::timestamptz, solar_system_id = $4, victim_id = $5, \
                 victim_corporation_id = $6, victim_alliance_id = $7, ship_type_id = $8, attackers = $9 \
             WHERE character_id = $1 AND killmail_id = $2",
            vec![
                id.into(),
                killmail.into(),
                detail["killmail_time"].as_str().map(str::to_owned).into(),
                i(&detail["solar_system_id"]).into(),
                i(&victim["character_id"]).into(),
                i(&victim["corporation_id"]).into(),
                i(&victim["alliance_id"]).into(),
                i(&victim["ship_type_id"]).into(),
                detail["attackers"]
                    .as_array()
                    .map(|a| i64::try_from(a.len()).unwrap_or_default())
                    .into(),
            ],
        )])?;
    }
    Ok(())
}

fn mining(run: &mut Run, id: i64) -> Result<(), Stop> {
    let (items, _) = run.pages("character-mining", id)?;
    for m in &items {
        run.ids.extend(i(&m["type_id"]));
        run.ids.extend(i(&m["solar_system_id"]));
    }
    insert_all(
        vec![],
        "INSERT INTO mining (character_id, day, type_id, solar_system_id, quantity) \
         SELECT $2, date, type_id, solar_system_id, quantity FROM json_to_recordset($1::json) \
         AS x(date date, type_id bigint, solar_system_id bigint, quantity bigint) \
         ON CONFLICT (character_id, day, type_id, solar_system_id) DO UPDATE SET quantity = EXCLUDED.quantity",
        &items,
        id,
    )
}

// ---- mail ------------------------------------------------------------------

/// Mail headers ESI gives a call: the newest, or those before
/// `last_mail_id`.
const MAIL_PAGE: usize = 50;
/// Pages of mail headers read per character per read, at most, all told:
/// one read always fits a run, and the rest waits for the next.
const MAIL_PAGES: usize = 20;

/// What a walk down the mail headers reads.
#[derive(Clone, Copy)]
enum Walk {
    /// Down to mail stored already: new mail, or a gap below `mail_gap`
    /// (where a walk cut short got to).
    Gap,
    /// Older mail than any stored (`mail_older` while there may be more).
    Older,
}

/// Mail, as aa-memberaudit pages it: headers newest first, 50 a call.
/// First a gap an earlier read left, then new mail down to the newest
/// stored, then older mail until the Settings' mails kept per character
/// are stored, ESI has no more, or the rest is older than the Settings
/// keep. At most MAIL_PAGES pages a read, each stored as it's read with
/// where the read got to: a read cut short keeps what it read, and the
/// next goes on from there. Then bodies, a few a read.
fn mail(run: &mut Run, id: i64) -> Result<(), Stop> {
    let lists = storage::query(
        "SELECT mailing_list_id FROM mailing_lists WHERE character_id = $1",
        &[id.into()],
    )
    .map(|r| r.rows.iter().map(|r| int(r, 0)).collect::<Vec<_>>())
    .unwrap_or_default();
    let cutoff = Utc::now() - chrono::Duration::days(run.settings.retention_days);
    let reading = |e| Stop::Section(format!("reading mail: {e:?}"));
    let stored = storage::query(
        "SELECT (SELECT max(mail_id) FROM mails WHERE character_id = c.character_id), c.mail_gap, \
                (SELECT max(mail_id) FROM mails WHERE character_id = c.character_id \
                   AND mail_id < c.mail_gap) \
         FROM characters c WHERE c.character_id = $1",
        &[id.into()],
    )
    .map_err(reading)?;
    let row = stored.rows.first();
    let newest = row.and_then(|r| opt_int(r, 0));
    let gap = row.and_then(|r| opt_int(r, 1));
    let below_gap = row.and_then(|r| opt_int(r, 2));
    let mut walk = Walker {
        id,
        lists: &lists,
        cutoff,
        pages: MAIL_PAGES,
    };
    let mut done = match gap {
        Some(gap) => walk.down(run, Some(gap), below_gap, Walk::Gap)?,
        None => true,
    };
    if done {
        done = match newest {
            Some(newest) => walk.down(run, None, Some(newest), Walk::Gap)?,
            // A first read: as far as is kept.
            None => walk.down(run, None, None, Walk::Older)?,
        };
    }
    if done {
        let kept = storage::query(
            "SELECT (SELECT min(mail_id) FROM mails WHERE character_id = c.character_id), \
                    (SELECT count(*) FROM mails WHERE character_id = c.character_id), c.mail_older \
             FROM characters c WHERE c.character_id = $1",
            &[id.into()],
        )
        .map_err(reading)?;
        if let Some(row) = kept.rows.first()
            && let Some(oldest) = opt_int(row, 0)
            && crate::boolean(row, 2)
            && int(row, 1) < run.settings.max_mails
        {
            walk.down(run, Some(oldest), None, Walk::Older)?;
        }
    }
    // Bodies: each read once, newest first, a few a run (those ESI failed
    // to give last time after the others).
    let pending = storage::query(
        "SELECT mail_id FROM mails WHERE character_id = $1 AND body IS NULL \
         ORDER BY body_tried_at NULLS FIRST, at DESC LIMIT $2",
        &[id.into(), (BODIES_PER_RUN as i64).into()],
    )
    .map_err(|e| Stop::Section(format!("reading mail: {e:?}")))?;
    for row in &pending.rows {
        if run.calls <= 1 {
            break;
        }
        let mail_id = int(row, 0);
        let body = match run.json::<Json>(
            "character-mail-body",
            id,
            &[("mail_id", mail_id.to_string())],
        ) {
            Ok(mail) => clip(
                &plain_text(mail["body"].as_str().unwrap_or_default()),
                MAX_BODY,
            ),
            // Deleted since: kept as a header without a body.
            Err(stop) if stop.gone() => String::new(),
            // A passing failure: asked again on a later read.
            Err(Stop::Section(why) | Stop::Esi(_, why)) => {
                log::warn(format!("character {id}, mail {mail_id}'s body: {why}"));
                store(&[stmt(
                    "UPDATE mails SET body_tried_at = now() WHERE character_id = $1 AND mail_id = $2",
                    vec![id.into(), mail_id.into()],
                )])?;
                continue;
            }
            Err(other) => return Err(other),
        };
        store(&[stmt(
            "UPDATE mails SET body = $3 WHERE character_id = $1 AND mail_id = $2",
            vec![id.into(), mail_id.into(), body.into()],
        )])?;
    }
    Ok(())
}

/// A walk down one character's mail headers, a page at a time.
struct Walker<'a> {
    id: i64,
    /// The character's mailing lists (not named as characters).
    lists: &'a [i64],
    /// The Settings keep: older mail isn't kept.
    cutoff: chrono::DateTime<Utc>,
    /// Pages left for this read.
    pages: usize,
}

impl Walker<'_> {
    /// Reads mail before `from` (the newest, with none) down to `to`
    /// (mail stored already; with none, as far as ESI and the Settings
    /// keep). Each page is stored with where the walk got to. Whether it
    /// got there, rather than being cut short.
    fn down(
        &mut self,
        run: &mut Run,
        from: Option<i64>,
        to: Option<i64>,
        walk: Walk,
    ) -> Result<bool, Stop> {
        let id = self.id;
        let mut before = from;
        while self.pages > 0 {
            self.pages -= 1;
            let page = mail_page(run, id, before)?;
            let lowest = page.iter().filter_map(|m| i(&m["mail_id"])).min();
            // ESI has no more, or the rest is older than the Settings keep.
            let bottom = page.len() < MAIL_PAGE || reaches(&page, self.cutoff) || lowest.is_none();
            let reached = matches!((lowest, to), (Some(lowest), Some(to)) if lowest <= to);
            let mark = match walk {
                Walk::Gap if bottom => stmt(
                    "UPDATE characters SET mail_gap = NULL, mail_older = false WHERE character_id = $1",
                    vec![id.into()],
                ),
                Walk::Gap if reached => stmt(
                    "UPDATE characters SET mail_gap = NULL WHERE character_id = $1",
                    vec![id.into()],
                ),
                Walk::Gap => stmt(
                    "UPDATE characters SET mail_gap = $2 WHERE character_id = $1",
                    vec![id.into(), lowest.into()],
                ),
                Walk::Older => stmt(
                    "UPDATE characters SET mail_older = $2 WHERE character_id = $1",
                    vec![id.into(), (!bottom).into()],
                ),
            };
            store_mail(run, id, self.lists, page, mark)?;
            if bottom || reached {
                return Ok(true);
            }
            // As many from here up as are kept: what's below would go at
            // once (and is read back if the Settings keep more).
            let above = storage::query(
                "SELECT count(*) FROM mails WHERE character_id = $1 AND mail_id >= $2",
                &[id.into(), lowest.into()],
            )
            .map_err(|e| Stop::Section(format!("reading mail: {e:?}")))?;
            if above.rows.first().map_or(0, |r| int(r, 0)) >= run.settings.max_mails {
                store(&[stmt(
                    "UPDATE characters SET mail_gap = NULL, mail_older = true WHERE character_id = $1",
                    vec![id.into()],
                )])?;
                return Ok(true);
            }
            before = lowest;
        }
        Ok(false)
    }
}

/// A page of mail headers: the newest, or those before `before`.
fn mail_page(run: &mut Run, id: i64, before: Option<i64>) -> Result<Vec<Json>, Stop> {
    let params: Vec<(&str, String)> = before
        .map(|m| ("last_mail_id", m.to_string()))
        .into_iter()
        .collect();
    run.json("character-mail", id, &params)
}

/// Whether a page of headers goes back past `cutoff` (what's older isn't
/// kept).
fn reaches(page: &[Json], cutoff: chrono::DateTime<Utc>) -> bool {
    page.iter()
        .filter_map(|m| m["timestamp"].as_str().and_then(crate::parse_time))
        .any(|at| at < cutoff)
}

/// Stores a page of mail headers, keeps the Settings' newest, and marks
/// where the read got to (`mark`), all at once.
fn store_mail(
    run: &mut Run,
    id: i64,
    lists: &[i64],
    headers: Vec<Json>,
    mark: Statement,
) -> Result<(), Stop> {
    let items: Vec<Json> = headers
        .into_iter()
        .filter_map(|m| {
            let from = i(&m["from"])?;
            if !lists.contains(&from) {
                run.ids.push(from);
            }
            let recipients: Vec<Json> = m["recipients"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .inspect(|r| {
                    if r["recipient_type"] != "mailing_list" {
                        run.ids.extend(i(&r["recipient_id"]));
                    }
                })
                .collect();
            Some(json!({
                "mail_id": m["mail_id"], "at": m["timestamp"], "from_id": from,
                "subject": clip(m["subject"].as_str().unwrap_or_default(), MAX_SHORT),
                "is_read": m["is_read"].as_bool().unwrap_or(false),
                "labels": m["labels"].as_array().cloned().unwrap_or_default(),
                "recipients": recipients,
            }))
        })
        .collect();
    // A page (50 headers) fits one statement.
    store(&[
        stmt(
            "INSERT INTO mails (character_id, mail_id, at, from_id, subject, is_read, labels, recipients) \
             SELECT $2, mail_id, at, from_id, subject, is_read, labels, recipients \
             FROM json_to_recordset($1::json) AS x(mail_id bigint, at timestamptz, from_id bigint, \
                  subject text, is_read boolean, labels jsonb, recipients jsonb) \
             WHERE at IS NOT NULL \
             ON CONFLICT (character_id, mail_id) DO UPDATE SET is_read = EXCLUDED.is_read, labels = EXCLUDED.labels",
            vec![rows(items), id.into()],
        ),
        // aa-memberaudit's MEMBERAUDIT_MAX_MAILS: the newest are kept.
        stmt(
            "DELETE FROM mails WHERE character_id = $1 AND mail_id NOT IN ( \
               SELECT mail_id FROM mails WHERE character_id = $1 ORDER BY at DESC, mail_id DESC LIMIT $2)",
            vec![id.into(), run.settings.max_mails.into()],
        ),
        mark,
    ])
}

fn mail_meta(run: &mut Run, id: i64) -> Result<(), Stop> {
    let labels: Json = run.json("character-mail-labels", id, &[])?;
    let lists: Vec<Json> = run.json("character-mailing-lists", id, &[])?;
    let labels: Vec<Json> = labels["labels"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|l| {
            json!({
                "label_id": l["label_id"], "name": clip(l["name"].as_str().unwrap_or_default(), 100),
                "unread": l["unread_count"].as_i64().unwrap_or(0),
            })
        })
        .collect();
    let lists: Vec<Json> = lists
        .into_iter()
        .map(|l| json!({ "mailing_list_id": l["mailing_list_id"], "name": clip(l["name"].as_str().unwrap_or_default(), 100) }))
        .collect();
    store(&[
        stmt(
            "DELETE FROM mail_labels WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO mail_labels (character_id, label_id, name, unread) \
             SELECT $2, label_id, name, unread FROM json_to_recordset($1::json) \
             AS x(label_id bigint, name text, unread bigint) WHERE label_id IS NOT NULL ON CONFLICT DO NOTHING",
            vec![rows(labels), id.into()],
        ),
        stmt(
            "DELETE FROM mailing_lists WHERE character_id = $1",
            vec![id.into()],
        ),
        stmt(
            "INSERT INTO mailing_lists (character_id, mailing_list_id, name) \
             SELECT $2, mailing_list_id, name FROM json_to_recordset($1::json) \
             AS x(mailing_list_id bigint, name text) WHERE mailing_list_id IS NOT NULL ON CONFLICT DO NOTHING",
            vec![rows(lists), id.into()],
        ),
    ])
}

// ---- names -----------------------------------------------------------------

/// Structures (by a character who may dock there) and planets: `names`
/// can't name them. A few a run; one that can't be named (ESI refused it,
/// or the character can't ask) isn't asked again for a week, while ESI's
/// passing trouble (5xx, 420) is asked again next run.
fn name_places(run: &mut Run) -> Result<(), JobError> {
    let mut structures = std::mem::take(&mut run.structures);
    structures.sort_unstable();
    structures.dedup_by_key(|(id, _)| *id);
    // And places stored earlier, with a character who has something there.
    let earlier = storage::query(
        "SELECT DISTINCT ON (p.id) p.id, p.character_id FROM ( \
           SELECT location_id AS id, character_id FROM characters WHERE location_type = 'structure' \
           UNION ALL SELECT location_id, character_id FROM clones \
           UNION ALL SELECT home_location_id, character_id FROM characters \
           UNION ALL SELECT location_id, character_id FROM assets WHERE location_flag = 'Hangar' \
         ) p WHERE p.id >= 1000000000000 \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = p.id) \
           AND NOT EXISTS (SELECT 1 FROM unnamed u WHERE u.id = p.id AND u.tried_at > now() - interval '7 days') \
         LIMIT 20",
        &[],
    )
    .map_err(|e| retry("finding unnamed structures", e))?;
    structures.extend(earlier.rows.iter().map(|r| (int(r, 0), int(r, 1))));
    // One row a planet, with any character colonising it: several
    // characters may share a planet.
    let planets = storage::query(
        "SELECT DISTINCT ON (p.planet_id) p.planet_id, p.character_id FROM planets p \
         WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = p.planet_id) \
           AND NOT EXISTS (SELECT 1 FROM unnamed u WHERE u.id = p.planet_id AND u.tried_at > now() - interval '7 days') \
         ORDER BY p.planet_id LIMIT 10",
        &[],
    )
    .map_err(|e| retry("finding unnamed planets", e))?;
    // Asked, and named or not to be asked again for a week.
    let mut tried = Vec::new();
    let mut named = Vec::new();
    for (structure, character) in structures.into_iter().take(10) {
        if run.calls <= NAME_RESERVE / 2 {
            break;
        }
        if tried.contains(&structure) {
            continue;
        }
        match run.json::<Json>(
            "universe-structure",
            character,
            &[("structure_id", structure.to_string())],
        ) {
            Ok(s) => {
                run.ids.extend(i(&s["solar_system_id"]));
                named.push(json!({ "id": structure, "name": s["name"], "category": "structure" }));
            }
            Err(Stop::Run | Stop::Unavailable) => break,
            Err(stop @ Stop::Esi(..)) if !stop.refused() => continue,
            Err(_) => {}
        }
        tried.push(structure);
    }
    for row in &planets.rows {
        if run.calls <= NAME_RESERVE / 2 {
            break;
        }
        let (planet, character) = (int(row, 0), int(row, 1));
        if tried.contains(&planet) {
            continue;
        }
        match run.json::<Json>(
            "universe-planet",
            character,
            &[("planet_id", planet.to_string())],
        ) {
            Ok(p) => named.push(json!({ "id": planet, "name": p["name"], "category": "planet" })),
            Err(Stop::Run | Stop::Unavailable) => break,
            Err(stop @ Stop::Esi(..)) if !stop.refused() => continue,
            Err(_) => {}
        }
        tried.push(planet);
    }
    let tried: Vec<Json> = tried.iter().map(|id| json!({ "id": id })).collect();
    // Each id once: an upsert can't touch a row twice.
    storage::transaction(&[
        stmt(
            "INSERT INTO names (id, name, category) \
             SELECT DISTINCT ON (id) id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
             WHERE name IS NOT NULL ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            vec![rows(named)],
        ),
        stmt(
            "INSERT INTO unnamed (id, tried_at) SELECT DISTINCT id, now() FROM json_to_recordset($1::json) AS x(id bigint) \
             WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = x.id) \
             ON CONFLICT (id) DO UPDATE SET tried_at = now()",
            vec![rows(tried)],
        ),
    ])
    .map_err(|e| retry("storing place names", e))?;
    Ok(())
}

/// Names for ids met this run, and any stored earlier that are still
/// unnamed. ESI refuses a whole batch for one id it doesn't know, so a
/// refused batch is halved until the culprit is found, and that id isn't
/// asked again for a week.
fn learn_names(run: &mut Run) -> Result<(), JobError> {
    let unnamed = storage::query(
        "SELECT id FROM ( \
           SELECT skill_id AS id FROM skills UNION SELECT skill_id FROM queue \
           UNION SELECT type_id FROM assets \
           UNION SELECT location_id FROM assets WHERE location_type IN ('station', 'solar_system') \
           UNION SELECT type_id FROM implants UNION SELECT system_id FROM characters \
           UNION SELECT ship_type_id FROM characters UNION SELECT corporation_id FROM characters \
           UNION SELECT alliance_id FROM characters \
         ) i WHERE id IS NOT NULL AND id > 0 AND id < 1000000000000 \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = i.id) LIMIT 3000",
        &[],
    )
    .map_err(|e| retry("finding unnamed ids", e))?;
    let mut ids = std::mem::take(&mut run.ids);
    ids.extend(unnamed.rows.iter().map(|r| int(r, 0)));
    ids.retain(|id| *id > 0 && !is_structure(*id));
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let skip = storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[]) \
         UNION SELECT id FROM unnamed WHERE id = ANY(string_to_array($1, ',')::bigint[]) \
           AND tried_at > now() - interval '7 days'",
        &[crate::id_list(&ids).into()],
    )
    .map_err(|e| retry("reading names", e))?;
    let skip: std::collections::BTreeSet<i64> = skip.rows.iter().map(|r| int(r, 0)).collect();
    let missing: Vec<i64> = ids.into_iter().filter(|id| !skip.contains(id)).collect();
    let mut named = Vec::new();
    let mut bad = Vec::new();
    let mut batches: Vec<Vec<i64>> = missing.chunks(1000).map(<[i64]>::to_vec).collect();
    while let Some(batch) = batches.pop() {
        if run.calls == 0 {
            break;
        }
        run.calls -= 1;
        match esi::names(&batch) {
            Ok(found) => named.extend(found),
            Err(EsiError::Status(404)) if batch.len() > 1 => {
                let (a, b) = batch.split_at(batch.len() / 2);
                batches.push(a.to_vec());
                batches.push(b.to_vec());
            }
            Err(EsiError::Status(404)) => bad.extend(batch),
            Err(err) => {
                log::warn(format!("names: {err:?}"));
                break;
            }
        }
    }
    let named: Vec<Json> = named
        .into_iter()
        .map(|n| json!({ "id": n.id, "name": n.name, "category": n.category }))
        .collect();
    let bad: Vec<Json> = bad.iter().map(|id| json!({ "id": id })).collect();
    storage::transaction(&[
        stmt(
            "INSERT INTO names (id, name, category) \
             SELECT id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            vec![rows(named)],
        ),
        stmt(
            "INSERT INTO unnamed (id, tried_at) SELECT id, now() FROM json_to_recordset($1::json) AS x(id bigint) \
             ON CONFLICT (id) DO UPDATE SET tried_at = now()",
            vec![rows(bad)],
        ),
    ])
    .map_err(|e| retry("storing names", e))?;
    Ok(())
}

/// Which group each skill is in: the skills category's groups (public),
/// read when there are none, then weekly.
fn learn_skill_groups(run: &mut Run) -> Result<(), JobError> {
    let stale = storage::query(
        "SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM skill_groups WHERE read_at > now() - interval '7 days')",
        &[],
    )
    .map_err(|e| retry("reading skill groups", e))?;
    if stale.rows.is_empty() {
        return Ok(());
    }
    // Any character's call will do: public data.
    let subject = match storage::query("SELECT character_id FROM characters LIMIT 1", &[]) {
        Ok(rows) => match rows.rows.first() {
            Some(row) => int(row, 0),
            None => return Ok(()),
        },
        Err(err) => return Err(retry("reading characters", err)),
    };
    let category: Json = match run.json(
        "universe-category",
        subject,
        &[("category_id", "16".to_owned())],
    ) {
        Ok(category) => category,
        Err(_) => return Ok(()),
    };
    let groups: Vec<i64> = category["groups"]
        .as_array()
        .map(|g| g.iter().filter_map(i).collect())
        .unwrap_or_default();
    if run.calls < groups.len() {
        return Ok(());
    }
    let mut group_rows = Vec::new();
    let mut type_rows = Vec::new();
    for group in groups {
        let Ok(g) = run.json::<Json>(
            "universe-group",
            subject,
            &[("group_id", group.to_string())],
        ) else {
            return Ok(());
        };
        group_rows.push(json!({ "group_id": group, "name": g["name"] }));
        for t in g["types"].as_array().into_iter().flatten().filter_map(i) {
            type_rows.push(json!({ "type_id": t, "group_id": group }));
        }
    }
    storage::transaction(&[
        stmt("DELETE FROM skill_groups", vec![]),
        stmt(
            "INSERT INTO skill_groups (group_id, name) SELECT group_id, name \
             FROM json_to_recordset($1::json) AS x(group_id bigint, name text) WHERE name IS NOT NULL",
            vec![rows(group_rows)],
        ),
        stmt(
            "INSERT INTO skill_types (type_id, group_id) SELECT type_id, group_id \
             FROM json_to_recordset($1::json) AS x(type_id bigint, group_id bigint) \
             WHERE group_id IN (SELECT group_id FROM skill_groups) ON CONFLICT DO NOTHING",
            vec![rows(type_rows)],
        ),
    ])
    .map_err(|e| retry("storing skill groups", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_read_by_something() {
        for s in SECTIONS {
            assert!(s.every >= 30 && s.calls >= 1, "{}", s.name);
            assert_eq!(section_named(s.name).map(|x| x.name), Some(s.name));
        }
        let mut names: Vec<&str> = SECTIONS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SECTIONS.len());
    }

    #[test]
    fn a_character_reads_well_within_the_budget_per_hour() {
        // Calls one character costs an hour at least, with every section
        // on its clock: the budget reads a few hundred characters an hour.
        let per_hour: f64 = SECTIONS
            .iter()
            .map(|s| s.calls as f64 * 60.0 / s.every as f64)
            .sum();
        assert!(per_hour < 25.0, "{per_hour}");
    }

    #[test]
    fn only_a_final_answer_settles_a_thing() {
        let esi = |code| Stop::from(EsiError::Status(code));
        assert!(esi(404).gone() && esi(410).gone());
        for code in [403, 420, 429, 500, 502, 503, 504] {
            assert!(!esi(code).gone(), "{code}");
        }
        assert!(esi(403).refused() && esi(404).refused());
        for code in [420, 429, 500, 503, 504] {
            assert!(!esi(code).refused(), "{code}");
        }
        assert!(!Stop::Section("unreadable".to_owned()).gone());
    }

    #[test]
    fn structures_are_told_apart() {
        assert!(is_structure(1_035_466_617_946));
        assert!(!is_structure(60_003_760));
    }
}
