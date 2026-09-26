//! Moon Mining (Alliance Auth's name for it; PRD F21).
//!
//! - Extractions come from the corporations of approved data-source
//!   characters (a Station Manager's, for moon extractions and structures).
//! - Each pop (the chunk's automatic fracture) is pinged to Members on
//!   Discord.
//! - A popped moon is Members-only for a while (default 4 hours), then on
//!   the old-moon list Blue see.
//! - Mining totals come from the corporations' mining observers.
//! - Station Managers (in game, from the data sources' corporation roles)
//!   get an extraction planner: a pop cadence turned into the duration to
//!   set at each drill.

mod planner;

use chrono::{DateTime, Duration, NaiveTime, SecondsFormat, Utc};
use serde::Deserialize;
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission,
    SubmitResult, Table, Tone, Value, badge, link, log, time,
};

use crate::planner::{Advice, Cadence, Drill};

/// Athanor and Tatara: the structures that drill moons.
const REFINERIES: [i64; 2] = [35835, 35836];
/// Popped moons stay on the old-moon list this long (roughly how long the
/// field lasts).
const OLD_FOR: Duration = Duration::hours(48);
/// A ping this late (after downtime, say) isn't worth sending.
const PING_GRACE: Duration = Duration::hours(2);
/// The planner's default: a moon a day at 19:00 EVE.
const DEFAULT_EVERY_HOURS: i64 = 24;
const DEFAULT_AT: &str = "19:00";

struct MoonMining;

impl Plugin for MoonMining {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match request.path.as_str() {
            "" => moons_page(&viewer),
            "totals" => totals_page(),
            "planner" => planner_page(&viewer),
            "settings" => settings_page(),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("settings", "settings") => save_settings(&viewer, &submission),
            ("planner", form) if form.starts_with("cadence_") => {
                save_cadence(&viewer, form, &submission)
            }
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(),
            "ledger" => ledger(),
            "roles" => roles(),
            "ping" => ping(&job),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(MoonMining);

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

/// Adds rows to a table.
fn with_rows(mut table: Table, rows: impl IntoIterator<Item = Vec<Value>>) -> Table {
    for row in rows {
        table = table.row(row);
    }
    table
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
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

struct Settings {
    fresh: Duration,
    channel: Option<String>,
    pings: bool,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT fresh_hours, ping_channel, pings FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first();
    Ok(Settings {
        fresh: Duration::hours(row.map_or(4, |r| int(r, 0))),
        channel: row
            .and_then(|r| r.get(1))
            .and_then(Db::as_text)
            .map(str::to_owned),
        pings: row
            .and_then(|r| r.get(2))
            .and_then(Db::as_bool)
            .unwrap_or(true),
    })
}

// ---- jobs ------------------------------------------------------------------

/// The host allows 100 ESI calls, and 100 queue calls, per run; stop
/// well short and pick up next run.
const ESI_BUDGET: usize = 90;
const QUEUE_BUDGET: usize = 90;
/// Observers whose ledgers are read per run, oldest first.
const OBSERVERS_PER_RUN: i64 = 20;
/// Station Manager entries not confirmed for this long are dropped (the
/// corporation's roles can't be read any more).
const MANAGERS_KEPT: &str = "2 days";

/// Calls left this run.
struct Budget(usize);

impl Budget {
    /// Takes one call if any are left.
    fn take(&mut self) -> bool {
        if self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        true
    }
}

/// Every page of an endpoint, within the budget (`None` when it runs out
/// or ESI says no, logged).
fn get_pages(
    budget: &mut Budget,
    endpoint: &str,
    subject: Subject,
    params: &[(String, String)],
    what: &str,
) -> Option<Vec<String>> {
    if !budget.take() {
        return None;
    }
    let first = match esi::get(endpoint, subject, params, Some(1)) {
        Ok(first) => first,
        Err(err) => {
            log::warn(format!("{what}: {err:?}"));
            return None;
        }
    };
    let mut bodies = vec![first.body];
    for page in 2..=first.pages {
        if !budget.take() {
            log::info(format!("{what}: out of ESI calls this run"));
            return None;
        }
        match esi::get(endpoint, subject, params, Some(page)) {
            Ok(response) => bodies.push(response.body),
            Err(err) => {
                log::warn(format!("{what}: {err:?}"));
                return None;
            }
        }
    }
    Some(bodies)
}

/// A JSON array of every page's items.
fn concat(bodies: &[String]) -> String {
    let items: Vec<serde_json::Value> = bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<Vec<serde_json::Value>>(b).ok())
        .flatten()
        .collect();
    serde_json::Value::Array(items).to_string()
}

#[derive(Deserialize)]
struct Moon {
    name: String,
    system_id: i64,
}

fn pop_key(structure_id: i64, arrival: &str) -> String {
    let stamp = parse_time(arrival).map_or(0, |t| t.timestamp());
    format!("pop:{structure_id}:{stamp}")
}

/// One data source per corporation.
fn sources_by_corporation() -> Vec<(i64, Subject)> {
    let mut seen = Vec::new();
    for source in esi::data_sources() {
        if !seen.iter().any(|(c, _)| *c == source.corporation_id) {
            seen.push((source.corporation_id, Subject::DataSource(source.id)));
        }
    }
    seen
}

/// Every 30 minutes: extractions and refineries from each corporation, a
/// ping queued for each new or moved pop, then names, all within one
/// budget.
fn sync() -> Result<(), JobError> {
    let now = Utc::now();
    let sources = sources_by_corporation();
    if sources.is_empty() {
        log::info("no data sources approved yet");
        return Ok(());
    }
    let mut budget = Budget(ESI_BUDGET);
    let mut queue = QUEUE_BUDGET;
    // Extractions and pings first: they matter most.
    for (corp, subject) in &sources {
        let Some(bodies) = get_pages(
            &mut budget,
            "corporation-mining-extractions",
            *subject,
            &[],
            &format!("extractions for corporation {corp}"),
        ) else {
            continue;
        };
        storage::transaction(&[
            Statement::new(
                "INSERT INTO extractions (structure_id, chunk_arrival, moon_id, corporation_id, extraction_start, natural_decay, seen_at) \
                 SELECT structure_id, chunk_arrival_time, moon_id, $2, extraction_start_time, natural_decay_time, now() \
                 FROM json_to_recordset($1::json) AS x(structure_id bigint, moon_id bigint, \
                      extraction_start_time timestamptz, chunk_arrival_time timestamptz, natural_decay_time timestamptz) \
                 ON CONFLICT (structure_id, chunk_arrival) DO UPDATE SET moon_id = EXCLUDED.moon_id, \
                 natural_decay = EXCLUDED.natural_decay, seen_at = now()",
                vec![Db::json(concat(&bodies)), (*corp).into()],
            ),
        ])
        .map_err(|e| retry("storing extractions", e))?;
        // Ones that were coming but the corporation no longer has:
        // rescheduled or cancelled. Their pings go too.
        let gone = storage::query(
            "DELETE FROM extractions WHERE corporation_id = $1 AND chunk_arrival > $2 \
             AND seen_at < now() - interval '1 minute' RETURNING structure_id, chunk_arrival",
            &[(*corp).into(), Db::timestamp(rfc3339(now))],
        )
        .map_err(|e| retry("removing stale extractions", e))?;
        for row in &gone.rows {
            if queue == 0 {
                break;
            }
            queue -= 1;
            let _ = jobs::cancel(&pop_key(int(row, 0), &text(row, 1)));
        }
    }
    // A ping at each coming pop not queued for its time yet.
    let due = storage::query(
        "SELECT structure_id, chunk_arrival, natural_decay FROM extractions \
         WHERE natural_decay > $1 AND queued_for IS DISTINCT FROM natural_decay \
         ORDER BY natural_decay LIMIT $2",
        &[Db::timestamp(rfc3339(now)), (queue as i64).into()],
    )
    .map_err(|e| retry("finding pops to queue", e))?;
    for row in &due.rows {
        let (structure, arrival) = (int(row, 0), text(row, 1));
        let Some(decay) = when(row, 2) else {
            continue;
        };
        let payload = serde_json::json!({ "structure_id": structure, "chunk_arrival": arrival });
        match jobs::enqueue(
            NewJob::new("ping")
                .key(pop_key(structure, &arrival))
                .payload(payload.to_string())
                .at(rfc3339(decay)),
        ) {
            Ok(()) => {
                storage::execute(
                    "UPDATE extractions SET queued_for = natural_decay WHERE structure_id = $1 AND chunk_arrival = $2",
                    &[structure.into(), Db::timestamp(arrival)],
                )
                .map_err(|e| retry("marking a queued ping", e))?;
            }
            Err(err) => {
                log::warn(format!("queuing a ping: {err:?}"));
                break;
            }
        }
    }
    // Refineries' names (a source without the role still has extractions).
    for (corp, subject) in &sources {
        let Some(bodies) = get_pages(
            &mut budget,
            "corporation-structures",
            *subject,
            &[],
            &format!("structures for corporation {corp}"),
        ) else {
            continue;
        };
        storage::transaction(&[Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, name, system_id, type_id, updated_at) \
             SELECT structure_id, $2, coalesce(name, 'Structure ' || structure_id::text), system_id, type_id, now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, name text, system_id bigint, type_id bigint) \
             WHERE type_id = ANY($3::bigint[]) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
             name = EXCLUDED.name, system_id = EXCLUDED.system_id, type_id = EXCLUDED.type_id, updated_at = now()",
            vec![
                Db::json(concat(&bodies)),
                (*corp).into(),
                format!("{{{},{}}}", REFINERIES[0], REFINERIES[1]).into(),
            ],
        )])
        .map_err(|e| retry("storing structures", e))?;
    }
    // Moon names, one call each, with what's left of the budget.
    let unnamed = storage::query(
        "SELECT DISTINCT e.moon_id, e.corporation_id FROM extractions e \
         WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = e.moon_id) LIMIT $1",
        &[(budget.0 as i64).into()],
    )
    .map_err(|e| retry("finding unnamed moons", e))?;
    let mut systems = Vec::new();
    for row in &unnamed.rows {
        let (moon_id, corp) = (int(row, 0), int(row, 1));
        let Some((_, subject)) = sources.iter().find(|(c, _)| *c == corp) else {
            continue;
        };
        if !budget.take() {
            break;
        }
        match esi::get(
            "universe-moon",
            *subject,
            &[("moon_id".to_owned(), moon_id.to_string())],
            None,
        ) {
            Ok(response) => {
                if let Ok(moon) = serde_json::from_str::<Moon>(&response.body) {
                    storage::execute(
                        "INSERT INTO names (id, name, category) VALUES ($1, $2, 'moon') ON CONFLICT (id) DO NOTHING",
                        &[moon_id.into(), moon.name.into()],
                    )
                    .map_err(|e| retry("storing a moon", e))?;
                    systems.push(moon.system_id);
                }
            }
            Err(err) => log::warn(format!("moon {moon_id}: {err:?}")),
        }
    }
    let known = storage::query(
        "SELECT DISTINCT system_id FROM structures WHERE system_id IS NOT NULL",
        &[],
    )
    .map_err(|e| retry("reading systems", e))?;
    systems.extend(known.rows.iter().map(|r| int(r, 0)));
    learn_names(&mut budget, &systems)
}

/// Names for ids we don't have yet, stored.
fn learn_names(budget: &mut Budget, ids: &[i64]) -> Result<(), JobError> {
    let mut ids = ids.to_vec();
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
            log::info("names: out of ESI calls this run");
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

/// Every 6 hours: what each pilot mined, from the oldest-read observers.
fn ledger() -> Result<(), JobError> {
    let mut budget = Budget(ESI_BUDGET);
    for (corp, subject) in sources_by_corporation() {
        let Some(bodies) = get_pages(
            &mut budget,
            "corporation-mining-observers",
            subject,
            &[],
            &format!("observers for corporation {corp}"),
        ) else {
            continue;
        };
        storage::transaction(&[Statement::new(
            "INSERT INTO observers (observer_id, corporation_id) \
             SELECT observer_id, $2 FROM json_to_recordset($1::json) AS x(observer_id bigint) \
             ON CONFLICT (observer_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id",
            vec![Db::json(concat(&bodies)), corp.into()],
        )])
        .map_err(|e| retry("storing observers", e))?;
    }
    let sources = sources_by_corporation();
    let due = storage::query(
        "SELECT observer_id, corporation_id FROM observers ORDER BY synced_at NULLS FIRST LIMIT $1",
        &[OBSERVERS_PER_RUN.into()],
    )
    .map_err(|e| retry("reading observers", e))?;
    let mut people = Vec::new();
    for row in &due.rows {
        let (observer, corp) = (int(row, 0), int(row, 1));
        let Some((_, subject)) = sources.iter().find(|(c, _)| *c == corp) else {
            continue;
        };
        let params = [("observer_id".to_owned(), observer.to_string())];
        let Some(bodies) = get_pages(
            &mut budget,
            "corporation-mining-observer",
            *subject,
            &params,
            &format!("observer {observer}"),
        ) else {
            if budget.0 == 0 {
                break;
            }
            continue;
        };
        let rows = concat(&bodies);
        storage::transaction(&[
            Statement::new(
                "INSERT INTO ledger (observer_id, character_id, type_id, day, corporation_id, quantity) \
                 SELECT $2, character_id, type_id, last_updated, recorded_corporation_id, quantity \
                 FROM json_to_recordset($1::json) AS x(character_id bigint, type_id bigint, last_updated date, \
                      recorded_corporation_id bigint, quantity bigint) \
                 ON CONFLICT (observer_id, character_id, type_id, day) DO UPDATE SET quantity = EXCLUDED.quantity",
                vec![Db::json(rows.clone()), observer.into()],
            ),
            Statement::new(
                "UPDATE observers SET synced_at = now() WHERE observer_id = $1",
                vec![observer.into()],
            ),
        ])
        .map_err(|e| retry("storing the ledger", e))?;
        if let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(&rows)
        {
            for item in items {
                people.extend(
                    ["character_id", "type_id"]
                        .iter()
                        .filter_map(|k| item[k].as_i64()),
                );
            }
        }
    }
    learn_names(&mut budget, &people)
}

/// Daily: who holds Station Manager in each data source's corporation.
fn roles() -> Result<(), JobError> {
    let mut budget = Budget(ESI_BUDGET);
    for (corp, subject) in sources_by_corporation() {
        if !budget.take() {
            break;
        }
        let body = match esi::get("corporation-roles", subject, &[], None) {
            Ok(response) => response.body,
            Err(err) => {
                log::warn(format!("roles for corporation {corp}: {err:?}"));
                continue;
            }
        };
        let Ok(members) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
            log::warn(format!("roles for corporation {corp}: unexpected answer"));
            continue;
        };
        let managers: Vec<serde_json::Value> = members
            .iter()
            .filter(|m| {
                m["roles"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|r| r == "Station_Manager"))
            })
            .map(|m| serde_json::json!({ "character_id": m["character_id"] }))
            .collect();
        // The corporation's list is replaced whole, or not at all.
        storage::transaction(&[
            Statement::new(
                "DELETE FROM station_managers WHERE corporation_id = $1",
                vec![corp.into()],
            ),
            Statement::new(
                "INSERT INTO station_managers (character_id, corporation_id, updated_at) \
                 SELECT character_id, $2, now() FROM json_to_recordset($1::json) AS x(character_id bigint) \
                 ON CONFLICT (character_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, updated_at = now()",
                vec![Db::json(serde_json::Value::Array(managers).to_string()), corp.into()],
            ),
        ])
        .map_err(|e| retry("storing station managers", e))?;
    }
    // Corporations whose roles can't be read any more lose their planner.
    storage::execute(
        &format!(
            "DELETE FROM station_managers WHERE updated_at < now() - interval '{MANAGERS_KEPT}'"
        ),
        &[],
    )
    .map_err(|e| retry("expiring station managers", e))?;
    Ok(())
}

#[derive(Deserialize)]
struct PingPayload {
    structure_id: i64,
    chunk_arrival: String,
}

/// At a pop: tell Members on Discord, once.
fn ping(job: &Job) -> Result<(), JobError> {
    let payload: PingPayload =
        serde_json::from_str(&job.payload).map_err(|e| JobError::Permanent(e.to_string()))?;
    if let Some(due) = parse_time(&job.scheduled_at)
        && Utc::now() - due > PING_GRACE
    {
        log::info("skipping a ping that's hours late");
        return Ok(());
    }
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let Some(channel) = settings.channel.filter(|_| settings.pings) else {
        return Ok(());
    };
    // Claimed first, so a retry after a sent message can't send it again.
    let rows = storage::query(
        "UPDATE extractions e SET pinged = true \
         FROM extractions x \
         LEFT JOIN names m ON m.id = x.moon_id \
         LEFT JOIN structures s ON s.structure_id = x.structure_id \
         LEFT JOIN names y ON y.id = s.system_id \
         WHERE e.structure_id = $1 AND e.chunk_arrival = $2 AND NOT e.pinged \
           AND x.structure_id = e.structure_id AND x.chunk_arrival = e.chunk_arrival \
         RETURNING coalesce(m.name, 'Moon ' || x.moon_id::text), \
                   coalesce(s.name, 'Structure ' || x.structure_id::text), \
                   coalesce(y.name, ''), x.natural_decay",
        &[
            payload.structure_id.into(),
            Db::timestamp(&payload.chunk_arrival),
        ],
    )
    .map_err(|e| retry("claiming the ping", e))?;
    let Some(row) = rows.rows.first() else {
        return Ok(());
    };
    let (moon, structure, system) = (text(row, 0), text(row, 1), text(row, 2));
    let at = when(row, 3).map_or_else(String::new, |t| t.format("%H:%M").to_string());
    let place = if system.is_empty() {
        structure.clone()
    } else {
        format!("{structure}, {system}")
    };
    let message = format!("Moon popped: {moon} ({place}) fractured at {at} EVE.");
    match discord::send(&channel, &message, Mention::State("Member".into())) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Not sent: release the claim for the retry.
            let _ = storage::execute(
                "UPDATE extractions SET pinged = false WHERE structure_id = $1 AND chunk_arrival = $2",
                &[
                    payload.structure_id.into(),
                    Db::timestamp(payload.chunk_arrival),
                ],
            );
            match err {
                discord::Error::NotAllowed(why) => {
                    log::warn(format!("ping not allowed: {why}"));
                    Err(JobError::Permanent(why))
                }
                other => Err(retry("sending the ping", other)),
            }
        }
    }
}

// ---- pages -----------------------------------------------------------------

/// A row of the moon lists: moon, place, and the pop.
struct Pop {
    moon: String,
    structure: String,
    system: String,
    arrival: DateTime<Utc>,
    decay: DateTime<Utc>,
}

fn pops(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Pop>, PageError> {
    let rows = storage::query(
        "SELECT coalesce(m.name, 'Moon ' || e.moon_id::text), coalesce(s.name, 'Structure ' || e.structure_id::text), \
                coalesce(y.name, ''), e.chunk_arrival, e.natural_decay \
         FROM extractions e \
         LEFT JOIN names m ON m.id = e.moon_id \
         LEFT JOIN structures s ON s.structure_id = e.structure_id \
         LEFT JOIN names y ON y.id = s.system_id \
         WHERE e.natural_decay > $1 AND e.natural_decay <= $2 \
         ORDER BY e.natural_decay LIMIT 500",
        &[Db::timestamp(rfc3339(from)), Db::timestamp(rfc3339(to))],
    )
    .map_err(|e| failed("reading extractions", e))?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| {
            Some(Pop {
                moon: text(r, 0),
                structure: text(r, 1),
                system: text(r, 2),
                arrival: when(r, 3)?,
                decay: when(r, 4)?,
            })
        })
        .collect())
}

fn moons_page(viewer: &Viewer) -> Result<Page, PageError> {
    let now = Utc::now();
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let old = pops(now - OLD_FOR, now - settings.fresh)?;
    let old_table = with_rows(
        Table::new(vec![
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Structure"),
            Column::numeric("Popped"),
        ])
        .title("Old moons")
        .empty("No moons popped in the last two days."),
        old.iter().rev().map(|p| {
            vec![
                p.moon.clone().into(),
                p.system.clone().into(),
                p.structure.clone().into(),
                time(rfc3339(p.decay)),
            ]
        }),
    );
    if !viewer.can("view") {
        return Ok(Page::new("Moon Mining")
            .description("Moons popped a while ago, still worth a visit")
            .table(old_table));
    }
    let fresh = pops(now - settings.fresh, now)?;
    let upcoming = pops(now, now + Duration::days(60))?;
    let ready = upcoming.iter().filter(|p| p.arrival <= now).count();
    let fresh_table = with_rows(
        Table::new(vec![
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Structure"),
            Column::numeric("Popped"),
        ])
        .title("Fresh moons")
        .empty("Nothing popped in the Members-only window."),
        fresh.iter().rev().map(|p| {
            vec![
                p.moon.clone().into(),
                p.system.clone().into(),
                p.structure.clone().into(),
                time(rfc3339(p.decay)),
            ]
        }),
    );
    let upcoming_table = with_rows(
        Table::new(vec![
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Structure"),
            Column::text("Status"),
            Column::numeric("Chunk arrives"),
            Column::numeric("Auto-fracture"),
        ])
        .title("Extractions")
        .empty("No extractions running. Is a data source approved?"),
        upcoming.iter().map(|p| {
            let status = if p.arrival <= now {
                badge("Ready", Tone::Accent)
            } else {
                badge("Extracting", Tone::Neutral)
            };
            vec![
                p.moon.clone().into(),
                p.system.clone().into(),
                p.structure.clone().into(),
                status.into(),
                time(rfc3339(p.arrival)),
                time(rfc3339(p.decay)),
            ]
        }),
    );
    let mut more = Card::new("More").field("Mining totals", link("Who mined what", "totals"));
    if !station_manager_corporations(viewer)?.is_empty() {
        more = more.field("Extraction planner", link("Plan pops", "planner"));
    }
    Ok(Page::new("Moon Mining")
        .description(format!(
            "Fresh moons are for Members for {} hours after they pop, then Blue see them too.",
            settings.fresh.num_hours()
        ))
        .stats(vec![
            Stat::new("Ready now", i64::try_from(ready).unwrap_or(i64::MAX)),
            Stat::new("Fresh", i64::try_from(fresh.len()).unwrap_or(i64::MAX)).caption(format!(
                "popped in the last {}h",
                settings.fresh.num_hours()
            )),
            Stat::new(
                "Extracting",
                i64::try_from(upcoming.len() - ready).unwrap_or(i64::MAX),
            ),
        ])
        .table(fresh_table)
        .card(more)
        .tab("Extractions", vec![Section::Table(upcoming_table)])
        .tab("Old moons", vec![Section::Table(old_table)]))
}

fn totals_page() -> Result<Page, PageError> {
    let rows = storage::query(
        "SELECT l.character_id, coalesce(n.name, 'Character ' || l.character_id::text), \
                coalesce(sum(l.quantity) FILTER (WHERE l.day >= date_trunc('month', now())::date), 0)::bigint, \
                coalesce(sum(l.quantity) FILTER (WHERE l.day >= (now() - interval '30 days')::date), 0)::bigint, \
                sum(l.quantity)::bigint \
         FROM ledger l LEFT JOIN names n ON n.id = l.character_id \
         GROUP BY l.character_id, n.name ORDER BY 4 DESC, 5 DESC LIMIT 500",
        &[],
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let ores = storage::query(
        "SELECT coalesce(n.name, 'Type ' || l.type_id::text), sum(l.quantity)::bigint \
         FROM ledger l LEFT JOIN names n ON n.id = l.type_id \
         WHERE l.day >= (now() - interval '30 days')::date \
         GROUP BY l.type_id, n.name ORDER BY 2 DESC LIMIT 100",
        &[],
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let pilots = with_rows(
        Table::new(vec![
            Column::text("Pilot"),
            Column::numeric("This month"),
            Column::numeric("Last 30 days"),
            Column::numeric("All time"),
        ])
        .title("By pilot (units)")
        .empty("Nothing mined yet, or no observers readable."),
        rows.rows.iter().map(|r| {
            vec![
                text(r, 1).into(),
                int(r, 2).into(),
                int(r, 3).into(),
                int(r, 4).into(),
            ]
        }),
    );
    let ore_table = with_rows(
        Table::new(vec![Column::text("Ore"), Column::numeric("Last 30 days")])
            .title("By ore (units)")
            .empty("Nothing mined in the last 30 days."),
        ores.rows
            .iter()
            .map(|r| vec![text(r, 0).into(), int(r, 1).into()]),
    );
    Ok(Page::new("Mining totals")
        .description("From the corporations' mining observers, refreshed every 6 hours")
        .table(pilots)
        .table(ore_table))
}

/// The corporations the viewer is a Station Manager in, in game.
fn station_manager_corporations(viewer: &Viewer) -> Result<Vec<i64>, PageError> {
    let ids: Vec<String> = viewer.characters.iter().map(|c| c.id.to_string()).collect();
    let rows = storage::query(
        "SELECT DISTINCT corporation_id FROM station_managers \
         WHERE character_id = ANY(string_to_array($1, ',')::bigint[])",
        &[ids.join(",").into()],
    )
    .map_err(|e| failed("reading station managers", e))?;
    Ok(rows.rows.iter().map(|r| int(r, 0)).collect())
}

fn cadence_for(corp: i64) -> Result<(i64, String), PageError> {
    let rows = storage::query(
        "SELECT every_hours, at_time FROM cadences WHERE corporation_id = $1",
        &[corp.into()],
    )
    .map_err(|e| failed("reading the cadence", e))?;
    Ok(rows.rows.first().map_or_else(
        || (DEFAULT_EVERY_HOURS, DEFAULT_AT.to_owned()),
        |r| (int(r, 0), text(r, 1)),
    ))
}

fn advice_row(drill: &Drill, advice: &Advice) -> Vec<Value> {
    let (status, pops, action) = match advice {
        Advice::OnSlot { pop, .. } => (
            badge("On slot", Tone::Success),
            time(rfc3339(*pop)),
            "Leave it".into(),
        ),
        Advice::OffSlot { pop, slot, off } => (
            badge("Off slot", Tone::Warning),
            time(rfc3339(*pop)),
            format!(
                "Pops {} {} its slot ({} EVE); next time, aim for the slot",
                planner::duration_text(off.abs()),
                if off.num_seconds() > 0 {
                    "after"
                } else {
                    "before"
                },
                slot.format("%d %b %H:%M")
            )
            .into(),
        ),
        Advice::Overlap { pop, with, .. } => (
            badge("Overlap", Tone::Danger),
            time(rfc3339(*pop)),
            format!("Pops in the same slot as {with}").into(),
        ),
        Advice::Start {
            arrival, duration, ..
        } => (
            badge("Idle", Tone::Accent),
            time(rfc3339(*arrival + planner::AUTO_FRACTURE)),
            format!(
                "Start now: {} (chunk arrives {} EVE)",
                planner::duration_text(*duration),
                arrival.format("%d %b %H:%M")
            )
            .into(),
        ),
        Advice::NoSlot => (
            badge("Idle", Tone::Warning),
            "".into(),
            "No free slot within 56 days: widen the cadence".into(),
        ),
    };
    vec![drill.name.clone().into(), status.into(), pops, action]
}

fn planner_page(viewer: &Viewer) -> Result<Page, PageError> {
    let corporations = station_manager_corporations(viewer)?;
    if corporations.is_empty() {
        return Ok(Page::new("Extraction planner").text(
            "The planner is for characters with the Station Manager role in a corporation Moon \
             Mining reads. Roles are checked once a day.",
        ));
    }
    let now = Utc::now();
    let mut page = Page::new("Extraction planner").description(
        "Go structure to structure and set each drill to the duration shown, so moons pop on a \
         steady cadence. Pops count the automatic fracture, three hours after the chunk arrives. \
         Tether can't start extractions: this only advises.",
    );
    for corp in corporations {
        let (every, at) = cadence_for(corp)?;
        let cadence = Cadence {
            every: Duration::hours(every),
            at: NaiveTime::parse_from_str(&at, "%H:%M")
                .unwrap_or(NaiveTime::from_hms_opt(19, 0, 0).unwrap_or_default()),
        };
        let rows = storage::query(
            "SELECT s.structure_id, s.name, \
                    (SELECT max(e.natural_decay) FROM extractions e WHERE e.structure_id = s.structure_id AND e.natural_decay > $2) \
             FROM structures s WHERE s.corporation_id = $1 ORDER BY s.name",
            &[corp.into(), Db::timestamp(rfc3339(now))],
        )
        .map_err(|e| failed("reading structures", e))?;
        let drills: Vec<Drill> = rows
            .rows
            .iter()
            .map(|r| Drill {
                structure_id: int(r, 0),
                name: text(r, 1),
                pop: when(r, 2),
            })
            .collect();
        let plan = planner::plan(&drills, cadence, now);
        let table = with_rows(
            Table::new(vec![
                Column::text("Structure"),
                Column::text("Status"),
                Column::numeric("Pops"),
                Column::text("What to do"),
            ])
            .title(format!("Drills of corporation {corp}"))
            .empty("No refineries seen for this corporation yet."),
            plan.advice.iter().map(|(d, a)| advice_row(d, a)),
        );
        let gaps = with_rows(
            Table::new(vec![Column::numeric("Slot with no pop")])
                .title("Gaps in the next two weeks")
                .empty("Every slot has a pop."),
            plan.gaps.iter().take(100).map(|g| vec![time(rfc3339(*g))]),
        );
        page = page
            .form(
                Form::new(format!("cadence_{corp}"), "Save cadence")
                    .title("Cadence")
                    .description("One pop every so many hours, lined up on a time of day (EVE).")
                    .field(
                        Field::number("every_hours", "Hours between pops")
                            .range(Some(1.0), Some(168.0), true)
                            .value(every.to_string())
                            .required(),
                    )
                    .field(
                        Field::text("at_time", "Lined up on (HH:MM EVE)", 5)
                            .value(at)
                            .required(),
                    ),
            )
            .table(table)
            .table(gaps);
    }
    Ok(page)
}

fn settings_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let mut channels: Vec<(String, String)> = vec![(String::new(), "No pings".to_owned())];
    channels.extend(
        discord::channels()
            .into_iter()
            .map(|c| (c.id, format!("#{}", c.name))),
    );
    Ok(Page::new("Moon Mining settings").form(
        Form::new("settings", "Save")
            .field(
                Field::number("fresh_hours", "Members-only hours after a pop")
                    .range(Some(1.0), Some(48.0), true)
                    .value(settings.fresh.num_hours().to_string())
                    .required(),
            )
            .field(
                Field::select("ping_channel", "Ping pops to", channels)
                    .value(settings.channel.unwrap_or_default()),
            )
            .field(Field::checkbox(
                "pings",
                "Ping Members at each pop",
                settings.pings,
            )),
    ))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let hours: i64 = submission
        .value("fresh_hours")
        .parse()
        .map_err(|_| PageError::Failed("fresh_hours wasn't a number".into()))?;
    let channel = submission.value("ping_channel");
    storage::execute(
        "UPDATE settings SET fresh_hours = $1, ping_channel = $2, pings = $3 WHERE id = 1",
        &[
            hours.into(),
            (!channel.is_empty()).then(|| channel.to_owned()).into(),
            submission.checked("pings").into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    // Admins see who changed what in the plugin's log.
    log::info(format!(
        "settings changed by {} ({}): members-only {hours}h, channel {channel:?}, pings {}",
        viewer.main.name,
        viewer.main.id,
        submission.checked("pings")
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

fn save_cadence(
    viewer: &Viewer,
    form: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let corp: i64 = form
        .trim_start_matches("cadence_")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    // Only that corporation's Station Managers set its cadence.
    if !station_manager_corporations(viewer)?.contains(&corp) {
        return Err(PageError::Forbidden);
    }
    let every: i64 = submission
        .value("every_hours")
        .parse()
        .map_err(|_| PageError::Failed("every_hours wasn't a number".into()))?;
    let at = submission.value("at_time").trim();
    if NaiveTime::parse_from_str(at, "%H:%M").is_err() || at.len() != 5 {
        return Ok(SubmitResult::Page(
            planner_page(viewer)?.text("Write the time as HH:MM, e.g. 19:00."),
        ));
    }
    storage::execute(
        "INSERT INTO cadences (corporation_id, every_hours, at_time) VALUES ($1, $2, $3) \
         ON CONFLICT (corporation_id) DO UPDATE SET every_hours = EXCLUDED.every_hours, at_time = EXCLUDED.at_time",
        &[corp.into(), every.into(), at.into()],
    )
    .map_err(|e| failed("saving the cadence", e))?;
    log::info(format!(
        "cadence for corporation {corp} set to every {every}h at {at} by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("planner".into()))
}
