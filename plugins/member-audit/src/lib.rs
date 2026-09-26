//! Member Audit (Alliance Auth's name for it; PRD F20).
//!
//! - **My Characters**: the viewer's characters with combined totals
//!   (wallets, skill points, queues ending soon), for multiboxers too.
//! - **Character Sheet**: skills, skill queue, assets, wallet journal,
//!   clones and implants, location and ship.
//! - **Character Finder** (officers): every member character.
//! - **Skill Sets**: named skill lists (a doctrine), with who can fly them.
//!
//! Data comes from every Member character registered with the plugin's
//! user scopes (installing it makes Member require them, as AA's Member
//! Audit compliance does). A sync every 15 minutes refreshes the oldest
//! few characters within the host's ESI call budget.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission,
    SubmitResult, Table, Tone, Value, badge, isk, link, log, time,
};

/// The host allows 100 ESI calls per run; stop short.
const ESI_BUDGET: usize = 90;
/// Calls one character's sync takes at least.
const CALLS_PER_CHARACTER: usize = 9;
/// Calls kept back each run for names.
const NAME_RESERVE: usize = 9;
/// Wallet journal kept, as ESI's own window is 30 days.
const JOURNAL_DAYS: &str = "30 days";
/// Longest journal description kept.
const MAX_DESCRIPTION: usize = 200;
/// Skill sets and the skills in one, so the Skill Sets page stays within
/// the host's page limits.
const MAX_SETS: i64 = 30;
const MAX_SKILLS_PER_SET: usize = 50;
/// A queue ending sooner than this is flagged.
const QUEUE_WARNING: Duration = Duration::hours(24);

struct MemberAudit;

impl Plugin for MemberAudit {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if let Some(id) = path.strip_prefix("character/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return character_page(&viewer, id);
        }
        match path {
            "" => my_characters(&viewer),
            "finder" => finder(&request),
            "skill-sets" => skill_sets_page(&viewer, None),
            "reports" => reports(),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        if submission.request.path != "skill-sets" || !viewer.can("manage") {
            return Err(PageError::Forbidden);
        }
        match submission.form.as_str() {
            "add_set" => add_set(&viewer, &submission),
            "delete_set" => delete_set(&viewer, submission.value("set")),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(MemberAudit);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn float(row: &[Db], i: usize) -> f64 {
    row.get(i).and_then(Db::as_float).unwrap_or_default()
}

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i).and_then(Db::as_text).and_then(parse_time)
}

fn with_rows(mut table: Table, rows: impl IntoIterator<Item = Vec<Value>>) -> Table {
    for row in rows {
        table = table.row(row);
    }
    table
}

fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

/// Calls left this run.
struct Budget(usize);

impl Budget {
    fn take(&mut self) -> bool {
        if self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        true
    }
}

/// Why a character's sync stopped: its token (skip it) or the budget
/// (stop the run).
enum Stop {
    Character(String),
    Budget,
}

fn call(
    budget: &mut Budget,
    endpoint: &str,
    character: i64,
    page: Option<u32>,
) -> Result<esi::Response, Stop> {
    if !budget.take() {
        return Err(Stop::Budget);
    }
    esi::get(endpoint, Subject::Character(character), &[], page)
        .map_err(|e| Stop::Character(format!("{endpoint}: {e:?}")))
}

fn all_pages(budget: &mut Budget, endpoint: &str, character: i64) -> Result<Vec<String>, Stop> {
    let first = call(budget, endpoint, character, Some(1))?;
    let mut bodies = vec![first.body];
    for page in 2..=first.pages.min(20) {
        bodies.push(call(budget, endpoint, character, Some(page))?.body);
    }
    Ok(bodies)
}

fn concat(bodies: &[String]) -> String {
    let items: Vec<serde_json::Value> = bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<Vec<serde_json::Value>>(b).ok())
        .flatten()
        .collect();
    serde_json::Value::Array(items).to_string()
}

// ---- sync ------------------------------------------------------------------

#[derive(Deserialize)]
struct Skills {
    skills: Vec<serde_json::Value>,
    total_sp: i64,
    #[serde(default)]
    unallocated_sp: Option<i64>,
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

#[derive(Deserialize)]
struct Clones {
    #[serde(default)]
    jump_clones: Vec<JumpClone>,
}

#[derive(Deserialize)]
struct JumpClone {
    jump_clone_id: i64,
    location_id: i64,
    #[serde(default)]
    implants: Vec<i64>,
}

/// Every 15 minutes: the character list, then the oldest few characters.
fn sync() -> Result<(), JobError> {
    let characters = esi::characters();
    let list: Vec<serde_json::Value> = characters
        .iter()
        .map(|c| {
            serde_json::json!({
                "character_id": c.id, "name": c.name,
                "corporation_id": c.corporation_id, "alliance_id": c.alliance_id,
            })
        })
        .collect();
    storage::transaction(&[
        Statement::new(
            "INSERT INTO characters (character_id, name, corporation_id, alliance_id, seen_at) \
             SELECT character_id, name, corporation_id, alliance_id, now() \
             FROM json_to_recordset($1::json) AS x(character_id bigint, name text, corporation_id bigint, alliance_id bigint) \
             ON CONFLICT (character_id) DO UPDATE SET name = EXCLUDED.name, \
             corporation_id = EXCLUDED.corporation_id, alliance_id = EXCLUDED.alliance_id, seen_at = now()",
            vec![Db::json(serde_json::Value::Array(list).to_string())],
        ),
        Statement::new(
            format!("DELETE FROM journal WHERE at < now() - interval '{JOURNAL_DAYS}'"),
            vec![],
        ),
    ])
    .map_err(|e| retry("storing characters", e))?;
    // Not in the list any more (left, sold, no longer a Member, consent
    // withdrawn): forgotten at once, with all its data. An empty list may
    // be the host having trouble, so it forgets nothing.
    if !characters.is_empty() {
        storage::execute(
            "DELETE FROM characters WHERE seen_at < now() - interval '1 minute'",
            &[],
        )
        .map_err(|e| retry("forgetting characters", e))?;
    }
    let due = storage::query(
        "SELECT character_id FROM characters WHERE seen_at > now() - interval '1 hour' \
         ORDER BY synced_at NULLS FIRST LIMIT $1",
        &[(((ESI_BUDGET - NAME_RESERVE) / CALLS_PER_CHARACTER) as i64).into()],
    )
    .map_err(|e| retry("choosing characters", e))?;
    let mut budget = Budget(ESI_BUDGET);
    let mut ids = Vec::new();
    for row in &due.rows {
        let id = int(row, 0);
        match sync_character(&mut budget, id, &mut ids) {
            Ok(()) => {}
            Err(Stop::Character(why)) => {
                log::warn(format!("character {id}: {why}"));
                // Its turn is used: others go first next time.
                let _ = storage::execute(
                    "UPDATE characters SET synced_at = now() WHERE character_id = $1",
                    &[id.into()],
                );
            }
            Err(Stop::Budget) => break,
        }
    }
    // And anything stored earlier that's still unnamed.
    let unnamed = storage::query(
        "SELECT id FROM ( \
           SELECT skill_id AS id FROM skills UNION SELECT skill_id FROM queue \
           UNION SELECT type_id FROM assets UNION SELECT location_id FROM assets \
           UNION SELECT type_id FROM implants UNION SELECT location_id FROM clones \
           UNION SELECT system_id FROM characters UNION SELECT location_id FROM characters \
           UNION SELECT ship_type_id FROM characters UNION SELECT corporation_id FROM characters \
         ) i WHERE id IS NOT NULL AND id > 0 AND id < 1000000000000 \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = i.id) LIMIT 3000",
        &[],
    )
    .map_err(|e| retry("finding unnamed ids", e))?;
    ids.extend(unnamed.rows.iter().map(|r| int(r, 0)));
    learn_names(&mut budget, &ids)
}

fn sync_character(budget: &mut Budget, id: i64, ids: &mut Vec<i64>) -> Result<(), Stop> {
    let store = |statements: &[Statement]| {
        storage::transaction(statements).map_err(|e| Stop::Character(format!("storing: {e:?}")))
    };
    let skills: Skills = serde_json::from_str(&call(budget, "character-skills", id, None)?.body)
        .map_err(|e| Stop::Character(format!("skills: {e}")))?;
    ids.extend(skills.skills.iter().filter_map(|s| s["skill_id"].as_i64()));
    let queue = call(budget, "character-skillqueue", id, None)?.body;
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&queue) {
        ids.extend(items.iter().filter_map(|s| s["skill_id"].as_i64()));
    }
    let wallet: f64 = call(budget, "character-wallet", id, None)?
        .body
        .trim()
        .parse()
        .unwrap_or_default();
    let location: Location =
        serde_json::from_str(&call(budget, "character-location", id, None)?.body)
            .map_err(|e| Stop::Character(format!("location: {e}")))?;
    let ship: Ship = serde_json::from_str(&call(budget, "character-ship", id, None)?.body)
        .map_err(|e| Stop::Character(format!("ship: {e}")))?;
    let clones: Clones = serde_json::from_str(&call(budget, "character-clones", id, None)?.body)
        .map_err(|e| Stop::Character(format!("clones: {e}")))?;
    let implants = call(budget, "character-implants", id, None)?.body;
    let journal = call(budget, "character-wallet-journal", id, Some(1))?.body;
    // Only what's stored, and a short description, so a busy journal fits.
    let journal: Vec<serde_json::Value> = serde_json::from_str::<Vec<serde_json::Value>>(&journal)
        .unwrap_or_default()
        .into_iter()
        .map(|j| {
            let description: String = j["description"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(MAX_DESCRIPTION)
                .collect();
            serde_json::json!({
                "id": j["id"], "date": j["date"], "ref_type": j["ref_type"],
                "amount": j["amount"], "balance": j["balance"], "description": description,
            })
        })
        .collect();
    let assets = all_pages(budget, "character-assets", id)?;

    ids.push(location.solar_system_id);
    ids.extend(location.station_id);
    ids.push(ship.ship_type_id);
    let clone_rows: Vec<serde_json::Value> = clones
        .jump_clones
        .iter()
        .map(|c| {
            ids.push(c.location_id);
            ids.extend(c.implants.iter().copied());
            serde_json::json!({
                "jump_clone_id": c.jump_clone_id,
                "location_id": c.location_id,
                "implants": c.implants,
            })
        })
        .collect();
    let implant_ids: Vec<i64> = serde_json::from_str(&implants).unwrap_or_default();
    ids.extend(implant_ids.iter().copied());
    let implant_rows: Vec<serde_json::Value> = implant_ids
        .iter()
        .map(|t| serde_json::json!({ "type_id": t }))
        .collect();

    let id_param: Db = id.into();
    store(&[
        Statement::new(
            "UPDATE characters SET synced_at = now(), total_sp = $2, unallocated_sp = $3, wallet = $4, \
             system_id = $5, location_id = $6, ship_type_id = $7, ship_name = $8 WHERE character_id = $1",
            vec![
                id_param.clone(),
                skills.total_sp.into(),
                skills.unallocated_sp.into(),
                wallet.into(),
                location.solar_system_id.into(),
                location.station_id.or(location.structure_id).into(),
                ship.ship_type_id.into(),
                ship.ship_name.into(),
            ],
        ),
        Statement::new(
            "DELETE FROM skills WHERE character_id = $1",
            vec![id_param.clone()],
        ),
        Statement::new(
            "INSERT INTO skills (character_id, skill_id, active_level, trained_level, sp) \
             SELECT $2, skill_id, active_skill_level, trained_skill_level, skillpoints_in_skill \
             FROM json_to_recordset($1::json) AS x(skill_id bigint, active_skill_level int, \
                  trained_skill_level int, skillpoints_in_skill bigint)",
            vec![
                Db::json(serde_json::Value::Array(skills.skills).to_string()),
                id_param.clone(),
            ],
        ),
        Statement::new(
            "DELETE FROM queue WHERE character_id = $1",
            vec![id_param.clone()],
        ),
        Statement::new(
            "INSERT INTO queue (character_id, position, skill_id, level, finish) \
             SELECT $2, queue_position, skill_id, finished_level, finish_date \
             FROM json_to_recordset($1::json) AS x(queue_position int, skill_id bigint, finished_level int, \
                  finish_date timestamptz) \
             ON CONFLICT DO NOTHING",
            vec![Db::json(queue), id_param.clone()],
        ),
        Statement::new(
            "DELETE FROM clones WHERE character_id = $1",
            vec![id_param.clone()],
        ),
        Statement::new(
            "INSERT INTO clones (character_id, jump_clone_id, location_id, implants) \
             SELECT $2, jump_clone_id, location_id, implants \
             FROM json_to_recordset($1::json) AS x(jump_clone_id bigint, location_id bigint, implants jsonb)",
            vec![
                Db::json(serde_json::Value::Array(clone_rows).to_string()),
                id_param.clone(),
            ],
        ),
        Statement::new(
            "DELETE FROM implants WHERE character_id = $1",
            vec![id_param.clone()],
        ),
        Statement::new(
            "INSERT INTO implants (character_id, type_id) \
             SELECT $2, type_id FROM json_to_recordset($1::json) AS x(type_id bigint) ON CONFLICT DO NOTHING",
            vec![
                Db::json(serde_json::Value::Array(implant_rows).to_string()),
                id_param.clone(),
            ],
        ),
        Statement::new(
            "DELETE FROM assets WHERE character_id = $1",
            vec![id_param.clone()],
        ),
    ])?;
    store(&[Statement::new(
        "INSERT INTO journal (character_id, id, at, ref_type, amount, balance, description) \
         SELECT $2, id, date, ref_type, amount, balance, coalesce(description, '') \
         FROM json_to_recordset($1::json) AS x(id bigint, date timestamptz, ref_type text, \
              amount double precision, balance double precision, description text) \
         ON CONFLICT (character_id, id) DO NOTHING",
        vec![
            Db::json(serde_json::Value::Array(journal).to_string()),
            id_param.clone(),
        ],
    )])?;
    // Assets page by page: a big hangar is more than one call may carry.
    for body in &assets {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(body)
        {
            ids.extend(items.iter().filter_map(|a| a["type_id"].as_i64()));
            ids.extend(
                items
                    .iter()
                    .filter(|a| a["location_type"] != "item")
                    .filter_map(|a| a["location_id"].as_i64()),
            );
        }
        store(&[Statement::new(
            "INSERT INTO assets (character_id, item_id, type_id, quantity, location_id, location_flag) \
             SELECT $2, item_id, type_id, quantity, location_id, location_flag \
             FROM json_to_recordset($1::json) AS x(item_id bigint, type_id bigint, quantity bigint, \
                  location_id bigint, location_flag text) \
             ON CONFLICT DO NOTHING",
            vec![
                Db::json(concat(std::slice::from_ref(body))),
                id_param.clone(),
            ],
        )])?;
    }
    Ok(())
}

/// Names for ids we don't have yet (structures, whose names ESI keeps
/// private, stay unnamed).
fn learn_names(budget: &mut Budget, ids: &[i64]) -> Result<(), JobError> {
    let mut ids: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| *id > 0 && *id < 1_000_000_000_000)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = ids.iter().map(i64::to_string).collect();
    let known = storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[list.join(",").into()],
    )
    .map_err(|e| retry("reading names", e))?;
    let known: Vec<i64> = known.rows.iter().map(|r| int(r, 0)).collect();
    let missing: Vec<i64> = ids.into_iter().filter(|id| !known.contains(id)).collect();
    for chunk in missing.chunks(1000) {
        if !budget.take() {
            return Ok(());
        }
        let named = match esi::names(chunk) {
            Ok(named) => named,
            Err(err) => {
                log::warn(format!("names: {err:?}"));
                return Ok(());
            }
        };
        let rows: Vec<serde_json::Value> = named
            .into_iter()
            .map(|n| serde_json::json!({ "id": n.id, "name": n.name, "category": n.category }))
            .collect();
        storage::transaction(&[Statement::new(
            "INSERT INTO names (id, name, category) \
             SELECT id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            vec![Db::json(serde_json::Value::Array(rows).to_string())],
        )])
        .map_err(|e| retry("storing names", e))?;
    }
    Ok(())
}

// ---- pages -----------------------------------------------------------------

/// A name for an id: stored, or the id.
const NAME: &str = "coalesce((SELECT name FROM names WHERE id = {}), {}::text)";

fn name_of(column: &str) -> String {
    NAME.replacen("{}", column, 2)
}

fn character_row(row: &[Db]) -> Vec<Value> {
    let synced = when(row, 7);
    vec![
        link(text(row, 1), format!("character/{}", int(row, 0))).into(),
        text(row, 2).into(),
        int(row, 3).into(),
        isk(float(row, 4)),
        text(row, 5).into(),
        text(row, 6).into(),
        synced.map_or_else(
            || badge("Not yet", Tone::Neutral).into(),
            |t| time(rfc3339(t)),
        ),
    ]
}

fn character_columns() -> Vec<Column> {
    vec![
        Column::text("Character"),
        Column::text("Corporation"),
        Column::numeric("Skill points"),
        Column::numeric("Wallet"),
        Column::text("Location"),
        Column::text("Ship"),
        Column::numeric("Synced"),
    ]
}

fn character_select(filter: &str) -> String {
    format!(
        "SELECT c.character_id, c.name, {corp}, coalesce(c.total_sp, 0), coalesce(c.wallet, 0), \
                {system}, coalesce({ship}, ''), c.synced_at \
         FROM characters c {filter}",
        corp = name_of("c.corporation_id"),
        system = name_of("c.system_id"),
        ship = name_of("c.ship_type_id"),
    )
}

fn ids_param(viewer: &Viewer) -> Db {
    let ids: Vec<String> = viewer.characters.iter().map(|c| c.id.to_string()).collect();
    ids.join(",").into()
}

fn my_characters(viewer: &Viewer) -> Result<Page, PageError> {
    let rows = query(
        &character_select(
            "WHERE c.character_id = ANY(string_to_array($1, ',')::bigint[]) ORDER BY c.name",
        ),
        &[ids_param(viewer)],
    )?;
    let (sp, wallet) = rows
        .iter()
        .fold((0i64, 0f64), |(sp, w), r| (sp + int(r, 3), w + float(r, 4)));
    let registered = rows.len();
    let queues = query(
        "SELECT c.name, max(q.finish), count(q.position), c.character_id FROM characters c \
         LEFT JOIN queue q ON q.character_id = c.character_id \
         WHERE c.character_id = ANY(string_to_array($1, ',')::bigint[]) \
         GROUP BY c.character_id, c.name ORDER BY max(q.finish) NULLS FIRST",
        &[ids_param(viewer)],
    )?;
    let now = Utc::now();
    let ending = queues
        .iter()
        .filter(|r| when(r, 1).is_none_or(|end| end - now < QUEUE_WARNING))
        .count();
    let queue_table = with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Queue"),
            Column::numeric("Skills queued"),
            Column::numeric("Ends"),
        ])
        .title("Skill queues")
        .empty("No characters synced yet."),
        queues.iter().map(|r| {
            let end = when(r, 1);
            let state = match end {
                None => badge("Empty", Tone::Danger),
                Some(end) if end - now < QUEUE_WARNING => badge("Ends soon", Tone::Warning),
                Some(_) => badge("Training", Tone::Success),
            };
            vec![
                link(text(r, 0), format!("character/{}", int(r, 3))).into(),
                state.into(),
                int(r, 2).into(),
                end.map_or_else(|| "".into(), |t| time(rfc3339(t))),
            ]
        }),
    );
    let mut page = Page::new("My Characters")
        .description("Your characters, and all of them together")
        .stats(vec![
            Stat::new(
                "Characters",
                i64::try_from(registered).unwrap_or(i64::MAX),
            )
            .caption(format!("of {} on your account", viewer.characters.len())),
            Stat::new("Wallets", isk(wallet)),
            Stat::new("Skill points", sp),
            Stat::new("Queues ending", i64::try_from(ending).unwrap_or(i64::MAX))
                .caption("within a day, or empty"),
        ])
        .table(with_rows(
            Table::new(character_columns())
                .title("Characters")
                .empty("None synced yet: register your characters with Member Audit's scopes, then wait for the next sync."),
            rows.iter().map(|r| character_row(r)),
        ))
        .table(queue_table);
    let mut more = Card::new("More").field("Skill Sets", link("What you can fly", "skill-sets"));
    if viewer.can("finder") {
        more = more
            .field("Character Finder", link("Every member character", "finder"))
            .field("Reports", link("Skill Sets across members", "reports"));
    }
    page = page.card(more);
    Ok(page)
}

fn character_page(viewer: &Viewer, id: i64) -> Result<Page, PageError> {
    // Their own characters, or anyone's with the Character Finder.
    if !viewer.characters.iter().any(|c| c.id == id) && !viewer.can("finder") {
        return Err(PageError::NotFound);
    }
    let rows = query(
        &format!(
            "SELECT c.character_id, c.name, {corp}, coalesce(c.total_sp, 0), coalesce(c.wallet, 0), \
                    {system}, coalesce({ship}, ''), c.synced_at, coalesce(c.unallocated_sp, 0), \
                    coalesce(c.ship_name, ''), {place} \
             FROM characters c WHERE c.character_id = $1",
            corp = name_of("c.corporation_id"),
            system = name_of("c.system_id"),
            ship = name_of("c.ship_type_id"),
            place = name_of("c.location_id"),
        ),
        &[id.into()],
    )?;
    let Some(c) = rows.first() else {
        return Err(PageError::NotFound);
    };
    let overview = Card::new("Overview")
        .field("Corporation", text(c, 2))
        .field("Skill points", int(c, 3))
        .field("Unallocated", int(c, 8))
        .field("Wallet", isk(float(c, 4)))
        .field("System", text(c, 5))
        .field("Docked at", text(c, 10))
        .field("Ship", format!("{} ({})", text(c, 9), text(c, 6)))
        .field(
            "Synced",
            when(c, 7).map_or_else(|| "not yet".into(), |t| time(rfc3339(t))),
        );
    let skills = query(
        &format!(
            "SELECT {skill}, trained_level, active_level, sp FROM skills s WHERE character_id = $1 \
             ORDER BY 1 LIMIT 500",
            skill = name_of("s.skill_id")
        ),
        &[id.into()],
    )?;
    let queue = query(
        &format!(
            "SELECT {skill}, level, finish FROM queue q WHERE character_id = $1 ORDER BY position",
            skill = name_of("q.skill_id")
        ),
        &[id.into()],
    )?;
    let assets = query(
        &format!(
            "SELECT {place}, {item}, sum(quantity)::bigint FROM assets a WHERE character_id = $1 \
             GROUP BY a.location_id, a.type_id ORDER BY 1, 2 LIMIT 500",
            place = name_of("a.location_id"),
            item = name_of("a.type_id")
        ),
        &[id.into()],
    )?;
    let journal = query(
        "SELECT at, ref_type, coalesce(amount, 0), coalesce(balance, 0), description FROM journal \
         WHERE character_id = $1 ORDER BY at DESC LIMIT 200",
        &[id.into()],
    )?;
    let clones = query(
        &format!(
            "SELECT {place}, (SELECT string_agg({implant}, ', ') FROM jsonb_array_elements_text(cl.implants) AS i(t)) \
             FROM clones cl WHERE character_id = $1",
            place = name_of("cl.location_id"),
            implant = name_of("i.t::bigint")
        ),
        &[id.into()],
    )?;
    let implants = query(
        &format!(
            "SELECT {implant} FROM implants m WHERE character_id = $1 ORDER BY 1",
            implant = name_of("m.type_id")
        ),
        &[id.into()],
    )?;
    Ok(Page::new(text(c, 1))
        .description("Character Sheet")
        .card(overview)
        .tab(
            "Skills",
            vec![Section::Table(with_rows(
                Table::new(vec![
                    Column::text("Skill"),
                    Column::numeric("Trained"),
                    Column::numeric("Active"),
                    Column::numeric("Skill points"),
                ])
                .empty("No skills synced."),
                skills.iter().map(|r| {
                    vec![
                        text(r, 0).into(),
                        int(r, 1).into(),
                        int(r, 2).into(),
                        int(r, 3).into(),
                    ]
                }),
            ))],
        )
        .tab(
            "Skill Queue",
            vec![Section::Table(with_rows(
                Table::new(vec![
                    Column::text("Skill"),
                    Column::numeric("Level"),
                    Column::numeric("Finishes"),
                ])
                .empty("The queue is empty."),
                queue.iter().map(|r| {
                    vec![
                        text(r, 0).into(),
                        int(r, 1).into(),
                        when(r, 2).map_or_else(|| "paused".into(), |t| time(rfc3339(t))),
                    ]
                }),
            ))],
        )
        .tab(
            "Assets",
            vec![Section::Table(with_rows(
                Table::new(vec![
                    Column::text("Location"),
                    Column::text("Item"),
                    Column::numeric("Quantity"),
                ])
                .empty("No assets synced."),
                assets
                    .iter()
                    .map(|r| vec![text(r, 0).into(), text(r, 1).into(), int(r, 2).into()]),
            ))],
        )
        .tab(
            "Wallet",
            vec![Section::Table(with_rows(
                Table::new(vec![
                    Column::numeric("Date"),
                    Column::text("Type"),
                    Column::numeric("Amount"),
                    Column::numeric("Balance"),
                    Column::text("Description"),
                ])
                .empty("No journal entries synced."),
                journal.iter().map(|r| {
                    vec![
                        when(r, 0).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                        text(r, 1).into(),
                        isk(float(r, 2)),
                        isk(float(r, 3)),
                        text(r, 4).into(),
                    ]
                }),
            ))],
        )
        .tab(
            "Clones",
            vec![
                Section::Table(with_rows(
                    Table::new(vec![Column::text("Jump clone"), Column::text("Implants")])
                        .empty("No jump clones."),
                    clones
                        .iter()
                        .map(|r| vec![text(r, 0).into(), text(r, 1).into()]),
                )),
                Section::Table(with_rows(
                    Table::new(vec![Column::text("Active implants")]).empty("No implants."),
                    implants.iter().map(|r| vec![text(r, 0).into()]),
                )),
            ],
        ))
}

fn finder(request: &Request) -> Result<Page, PageError> {
    let q = request
        .query
        .iter()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.trim().to_lowercase())
        .unwrap_or_default();
    let rows = if q.is_empty() {
        query(&character_select("ORDER BY c.name LIMIT 500"), &[])?
    } else {
        query(
            &character_select(
                "WHERE lower(c.name) LIKE '%' || $1 || '%' ORDER BY c.name LIMIT 500",
            ),
            &[q.into()],
        )?
    };
    let count = query("SELECT count(*) FROM characters", &[])?;
    Ok(Page::new("Character Finder")
        .description(
            "Every Member character registered with Member Audit's scopes. Add ?q=name to search.",
        )
        .stats(vec![Stat::new(
            "Characters",
            count.first().map_or(0, |r| int(r, 0)),
        )])
        .table(with_rows(
            Table::new(character_columns()).empty("No characters match."),
            rows.iter().map(|r| character_row(r)),
        )))
}

// ---- skill sets --------------------------------------------------------------

struct SkillSet {
    id: i64,
    name: String,
    skills: Vec<(i64, String, i64)>,
}

fn skill_sets() -> Result<Vec<SkillSet>, PageError> {
    let sets = query("SELECT id, name FROM skill_sets ORDER BY name", &[])?;
    let mut out = Vec::new();
    for s in &sets {
        let id = int(s, 0);
        let skills = query(
            &format!(
                "SELECT skill_id, {skill}, level FROM skill_set_skills k WHERE set_id = $1 ORDER BY 2",
                skill = name_of("k.skill_id")
            ),
            &[id.into()],
        )?;
        out.push(SkillSet {
            id,
            name: text(s, 1),
            skills: skills
                .iter()
                .map(|r| (int(r, 0), text(r, 1), int(r, 2)))
                .collect(),
        });
    }
    Ok(out)
}

/// Characters (of `filter`) meeting every skill of the set.
fn able(set: &SkillSet, only: Option<&Db>) -> Result<Vec<(i64, String)>, PageError> {
    let (sql, params) = match only {
        Some(ids) => (
            "SELECT c.character_id, c.name FROM characters c \
             WHERE c.character_id = ANY(string_to_array($2, ',')::bigint[]) AND NOT EXISTS ( \
               SELECT 1 FROM skill_set_skills k WHERE k.set_id = $1 AND NOT EXISTS ( \
                 SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
                   AND s.skill_id = k.skill_id AND s.active_level >= k.level)) \
             ORDER BY c.name LIMIT 500",
            vec![set.id.into(), ids.clone()],
        ),
        None => (
            "SELECT c.character_id, c.name FROM characters c WHERE NOT EXISTS ( \
               SELECT 1 FROM skill_set_skills k WHERE k.set_id = $1 AND NOT EXISTS ( \
                 SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
                   AND s.skill_id = k.skill_id AND s.active_level >= k.level)) \
             ORDER BY c.name LIMIT 500",
            vec![set.id.into()],
        ),
    };
    Ok(query(sql, &params)?
        .iter()
        .map(|r| (int(r, 0), text(r, 1)))
        .collect())
}

fn skill_sets_page(viewer: &Viewer, note: Option<&str>) -> Result<Page, PageError> {
    let sets = skill_sets()?;
    let mine = ids_param(viewer);
    let mut page = Page::new("Skill Sets").description(
        "Named lists of skills, such as a doctrine, and which of your characters can use them",
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    let mut rows = Vec::new();
    for set in &sets {
        let able = able(set, Some(&mine))?;
        let skills: Vec<String> = set
            .skills
            .iter()
            .map(|(_, name, level)| format!("{name} {level}"))
            .collect();
        let mut listed = skills.join(", ");
        if listed.chars().count() > 1500 {
            listed = listed.chars().take(1500).collect::<String>() + "…";
        }
        rows.push(vec![
            set.name.clone().into(),
            listed.into(),
            if able.is_empty() {
                badge("None of yours", Tone::Neutral).into()
            } else {
                able.iter()
                    .map(|(_, n)| n.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
                    .into()
            },
        ]);
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Skill set"),
            Column::text("Skills"),
            Column::text("Your characters who can"),
        ])
        .empty("No skill sets yet."),
        rows,
    ));
    if viewer.can("manage") {
        page = page.form(
            Form::new("add_set", "Add skill set")
                .title("New skill set")
                .description("One skill per line with its level, as `Caldari Battleship 4`. Only skills some member has trained are known.")
                .field(Field::text("name", "Name", 100).required())
                .field(Field::textarea("skills", "Skills", 5000).required()),
        );
        if !sets.is_empty() {
            let choices: Vec<(String, String)> = sets
                .iter()
                .take(100)
                .map(|set| (set.id.to_string(), set.name.clone()))
                .collect();
            page = page.form(
                Form::new("delete_set", "Delete skill set")
                    .field(Field::select("set", "Skill set", choices).required())
                    .field(Field::checkbox("confirm", "Yes, delete it", false).required()),
            );
        }
    }
    Ok(page)
}

/// `Skill Name 4` lines, matched to known skills.
fn parse_skills(text: &str) -> Result<Vec<(i64, i64)>, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() > MAX_SKILLS_PER_SET {
        return Err(format!(
            "A skill set has at most {MAX_SKILLS_PER_SET} skills."
        ));
    }
    let mut out = Vec::new();
    for line in lines {
        let line: String = line.chars().take(100).collect();
        let line = line.as_str();
        let (name, level) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("\"{line}\" needs a level after the skill"))?;
        let level: i64 = level
            .trim()
            .parse()
            .ok()
            .filter(|l| (1..=5).contains(l))
            .ok_or_else(|| format!("\"{line}\": the level is 1 to 5"))?;
        let found = storage::query(
            "SELECT id FROM names WHERE lower(name) = lower($1) \
             AND id IN (SELECT DISTINCT skill_id FROM skills) LIMIT 1",
            &[name.trim().into()],
        )
        .map_err(|e| format!("reading skills: {e:?}"))?;
        let id =
            found.rows.first().map(|r| int(r, 0)).ok_or_else(|| {
                format!("\"{}\" isn't a skill any member has trained", name.trim())
            })?;
        out.push((id, level));
    }
    if out.is_empty() {
        return Err("List at least one skill.".to_owned());
    }
    Ok(out)
}

fn add_set(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let name = submission.value("name").trim().to_owned();
    let skills = match parse_skills(submission.value("skills")) {
        Ok(skills) => skills,
        Err(why) => return Ok(SubmitResult::Page(skill_sets_page(viewer, Some(&why))?)),
    };
    let count = storage::query("SELECT count(*) FROM skill_sets", &[])
        .map_err(|e| failed("counting skill sets", e))?;
    if count.rows.first().map_or(0, |r| int(r, 0)) >= MAX_SETS {
        return Ok(SubmitResult::Page(skill_sets_page(
            viewer,
            Some(&format!(
                "There can be at most {MAX_SETS} skill sets: delete one first."
            )),
        )?));
    }
    let rows: Vec<serde_json::Value> = skills
        .iter()
        .map(|(skill, level)| serde_json::json!({ "skill_id": skill, "level": level }))
        .collect();
    // The set and its skills together, or neither.
    let added = storage::query(
        "WITH s AS (INSERT INTO skill_sets (name) VALUES ($1) ON CONFLICT (name) DO NOTHING RETURNING id), \
              k AS (INSERT INTO skill_set_skills (set_id, skill_id, level) \
                    SELECT s.id, x.skill_id, max(x.level) FROM s, \
                           json_to_recordset($2::json) AS x(skill_id bigint, level int) \
                    GROUP BY s.id, x.skill_id RETURNING set_id) \
         SELECT id FROM s",
        &[name.clone().into(), Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map_err(|e| failed("saving the skill set", e))?;
    if added.rows.is_empty() {
        return Ok(SubmitResult::Page(skill_sets_page(
            viewer,
            Some("A skill set with that name already exists."),
        )?));
    }
    log::info(format!(
        "skill set {name:?} added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

fn delete_set(viewer: &Viewer, set: &str) -> Result<SubmitResult, PageError> {
    let id: i64 = set.parse().map_err(|_| PageError::NotFound)?;
    storage::execute("DELETE FROM skill_sets WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the skill set", e))?;
    log::info(format!(
        "skill set {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("skill-sets".into()))
}

fn reports() -> Result<Page, PageError> {
    let sets = skill_sets()?;
    let mut page =
        Page::new("Reports").description("Skill Sets: which member characters can use each");
    let mut summary = Vec::new();
    let mut tabs = Vec::new();
    for set in sets.iter().take(10) {
        let able = able(set, None)?;
        summary.push(vec![
            set.name.clone().into(),
            i64::try_from(able.len()).unwrap_or(i64::MAX).into(),
        ]);
        tabs.push((
            set.name.clone(),
            with_rows(
                Table::new(vec![Column::text("Character")]).empty("Nobody yet."),
                able.iter()
                    .map(|(id, n)| vec![link(n.clone(), format!("character/{id}")).into()]),
            ),
        ));
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Skill set"),
            Column::numeric("Characters"),
        ])
        .title("Skill Sets")
        .empty("No skill sets yet."),
        summary,
    ));
    for (name, table) in tabs {
        page = page.tab(name, vec![Section::Table(table)]);
    }
    Ok(page)
}
