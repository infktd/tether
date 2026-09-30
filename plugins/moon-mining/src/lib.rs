//! Moon Mining (Alliance Auth's name for it; PRD F21).
//!
//! - Extractions come from the corporations of the app's owners
//!   (data-source characters: a Station Manager's, for moon extractions and
//!   structures), added by holders of `add_refinery_owner`.
//! - Optional, and off unless a manager turns them on (aa-moonmining has
//!   neither): each pop (the chunk's automatic fracture) pinged to Members
//!   on Discord, once a channel is picked; and a Members-only window after
//!   each pop, after which the moon goes on an old-moon list for those
//!   with `basic_access` alone (Blue). Without the window, as
//!   aa-moonmining, extractions are for `extractions_access` and everyone
//!   else opens Moons.
//! - Mining totals come from the corporations' mining observers.
//! - An extraction planner for every refinery of the corporations read,
//!   idle ones first: each corporation's pop cadence, which its Station
//!   Managers (in game, from the data sources' corporation roles) set,
//!   turned into the duration to set at each drill.
//! - Moons (aa-moonmining's): owned moons, every moon, and the moons one
//!   uploaded, from pasted moon surveys, each with its ores and value.
//! - Values come from CCP's ore prices (ESI's `/markets/prices/`, read
//!   daily); see `value` for how.
//! - Reports: moons' potential income, members' mining, uploads and ore
//!   prices.

mod extraction;
mod moons;
mod planner;
mod reports;
mod survey;
mod value;

use chrono::{DateTime, Duration, NaiveTime, SecondsFormat, Utc};
use serde::Deserialize;
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Lane, LaneItem, Page, PageError, Plugin, Request, Section, Stat,
    Submission, SubmitResult, Table, Timeline, Tone, Value, badge, character, countdown, isk,
    item_type, link, log, time,
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
        let segments: Vec<&str> = request.path.split('/').collect();
        let page = match segments.as_slice() {
            [""] => extractions_page(&viewer),
            ["moons"] => moons::moons_page(&viewer, &moons::Filter::default()),
            ["moon", id] => moons::moon_page(&viewer, id.parse().map_err(|_| PageError::NotFound)?),
            ["upload"] => moons::upload_page(&viewer, None),
            ["extraction", structure, at] => extraction::page(
                &viewer,
                structure.parse().map_err(|_| PageError::NotFound)?,
                at.parse().map_err(|_| PageError::NotFound)?,
            ),
            ["reports"] => reports::page(&viewer),
            ["totals"] => totals_page(),
            ["planner"] => planner_page(&viewer),
            ["settings"] => settings_page(),
            _ => Err(PageError::NotFound),
        }?;
        with_links(page, &viewer)
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("settings", "settings") => save_settings(&viewer, &submission),
            ("planner", form) if form.starts_with("cadence_") => {
                save_cadence(&viewer, form, &submission)
            }
            ("moons", "filter") => {
                let filter = moons::Filter::from(&submission);
                Ok(SubmitResult::Page(with_links(
                    moons::moons_page(&viewer, &filter)?,
                    &viewer,
                )?))
            }
            ("upload", "survey") => moons::upload(&viewer, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(),
            "ledger" => ledger(),
            "roles" => roles(),
            "prices" => prices(),
            "places" => {
                let mut budget = Budget(ESI_BUDGET);
                places(&mut budget, &sources_by_corporation())?;
                // A big survey upload: keep naming its moons until done.
                let left =
                    storage::query("SELECT 1 FROM moons WHERE checked_at IS NULL LIMIT 1", &[])
                        .map_err(|e| retry("finding moons to check", e))?;
                if !left.rows.is_empty() {
                    jobs::enqueue(
                        NewJob::new("places")
                            .key("places")
                            .at(rfc3339(Utc::now() + PLACES_AGAIN)),
                    )
                    .map_err(|e| retry("queuing more moons", e))?;
                }
                Ok(())
            }
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

fn float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
}

/// "Jita (0.9)": a system with its security, as the game rounds it.
fn system_label(name: &str, security: Option<f64>) -> String {
    match security {
        Some(sec) if !name.is_empty() => format!("{name} ({:.1})", security_shown(sec)),
        _ => name.to_owned(),
    }
}

/// The game shows 0.0 < sec < 0.05 as 0.1, and rounds the rest.
fn security_shown(sec: f64) -> f64 {
    if sec > 0.0 && sec < 0.05 {
        0.1
    } else {
        (sec * 10.0).round() / 10.0
    }
}

/// An ISK cell: the amount, or nothing when it isn't known.
fn isk_or_blank(amount: Option<f64>) -> Value {
    amount.map_or_else(|| "".into(), |a| isk(value::finite(a)))
}

/// A number of units or m³ as a count.
fn count(n: f64) -> Value {
    Value::Number(if n.is_finite() { n.round() as i64 } else { 0 })
}

struct Settings {
    fresh: Duration,
    channel: Option<String>,
    pings: bool,
    /// aa-moonmining's volume per day and days per month.
    rates: value::Rates,
    /// aa-moonmining's hours until a completed extraction is stale (Past).
    stale: Duration,
    /// Old moons listed beside the fresh ones on Extractions.
    old_shown: usize,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT fresh_hours, ping_channel, pings, volume_per_day, days_per_month, stale_hours, \
                old_moons_shown \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first();
    let defaults = value::Rates::default();
    Ok(Settings {
        rates: value::Rates {
            per_day: row.and_then(|r| float(r, 3)).unwrap_or(defaults.per_day),
            days_per_month: row
                .and_then(|r| float(r, 4))
                .unwrap_or(defaults.days_per_month),
        },
        stale: Duration::hours(row.map_or(12, |r| int(r, 5))),
        old_shown: row.map_or(5, |r| usize::try_from(int(r, 6)).unwrap_or(5)),
        fresh: Duration::hours(row.map_or(0, |r| int(r, 0))),
        channel: row
            .and_then(|r| r.get(1))
            .and_then(Db::as_text)
            .map(str::to_owned),
        pings: row
            .and_then(|r| r.get(2))
            .and_then(Db::as_bool)
            .unwrap_or(false),
    })
}

impl Settings {
    /// Whether the Members-only window (and the old-moon list) is on.
    fn window(&self) -> bool {
        self.fresh > Duration::zero()
    }
}

/// The settings values are worked out with, for pages.
fn rates() -> Result<value::Rates, PageError> {
    Ok(settings().map_err(|e| failed("reading settings", e))?.rates)
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
        log::info("no owners added yet");
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
                 natural_decay = EXCLUDED.natural_decay, seen_at = now(), cancelled_at = NULL",
                vec![Db::json(concat(&bodies)), (*corp).into()],
            ),
            Statement::new(
                "INSERT INTO moons (moon_id) SELECT DISTINCT moon_id FROM extractions ON CONFLICT DO NOTHING",
                vec![],
            ),
        ])
        .map_err(|e| retry("storing extractions", e))?;
        // Ones that were coming but the corporation no longer has:
        // cancelled (a restart shows as a new one). Kept for the Past tab;
        // their pings go.
        let gone = storage::query(
            "UPDATE extractions SET cancelled_at = now() WHERE corporation_id = $1 AND chunk_arrival > $2 \
             AND cancelled_at IS NULL AND seen_at < now() - interval '1 minute' \
             RETURNING structure_id, chunk_arrival",
            &[(*corp).into(), Db::timestamp(rfc3339(now))],
        )
        .map_err(|e| retry("marking cancelled extractions", e))?;
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
         WHERE natural_decay > $1 AND cancelled_at IS NULL AND queued_for IS DISTINCT FROM natural_decay \
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
            // A refinery without a Moon Drill (one for reprocessing, say)
            // isn't a drill: the planner leaves it out.
            "INSERT INTO structures (structure_id, corporation_id, name, system_id, type_id, drill, updated_at) \
             SELECT structure_id, $2, coalesce(name, 'Structure ' || structure_id::text), system_id, type_id, \
                    EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(services, '[]')) s \
                            WHERE s ->> 'name' = 'Moon Drilling'), now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, name text, system_id bigint, \
                  type_id bigint, services jsonb) \
             WHERE type_id = ANY($3::bigint[]) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
             name = EXCLUDED.name, system_id = EXCLUDED.system_id, type_id = EXCLUDED.type_id, \
             drill = EXCLUDED.drill, updated_at = now()",
            vec![
                Db::json(concat(&bodies)),
                (*corp).into(),
                format!("{{{},{}}}", REFINERIES[0], REFINERIES[1]).into(),
            ],
        )])
        .map_err(|e| retry("storing structures", e))?;
    }
    places(&mut budget, &sources)
}

/// Moons checked with ESI at most per run (one call each), well under the
/// host's ESI error throttle (30 in 5 minutes), and unknown moons (ESI's
/// 404s) a run stops after.
const MOONS_PER_RUN: i64 = 40;
const UNKNOWN_MOONS_PER_RUN: usize = 3;
/// How soon `places` runs again while moons are left to name.
const PLACES_AGAIN: Duration = Duration::minutes(2);
/// Systems looked up per run (two calls each).
const SYSTEMS_PER_RUN: i64 = 15;

#[derive(Deserialize)]
struct SystemInfo {
    security_status: f64,
    constellation_id: i64,
    region_id: i64,
}

/// Names and places, with what's left of the budget: each moon's name and
/// system from ESI (a surveyed moon ESI doesn't know is dropped), systems'
/// security, constellation and region, then the names of everything
/// shown.
fn places(budget: &mut Budget, sources: &[(i64, Subject)]) -> Result<(), JobError> {
    let unchecked = storage::query(
        // Moons drilled first: they're on the Extractions and the planner.
        "SELECT m.moon_id FROM moons m WHERE m.checked_at IS NULL \
         ORDER BY EXISTS (SELECT 1 FROM extractions e WHERE e.moon_id = m.moon_id) DESC, m.moon_id \
         LIMIT $1",
        &[MOONS_PER_RUN.into()],
    )
    .map_err(|e| retry("finding moons to check", e))?;
    let mut unknown = 0;
    for row in &unchecked.rows {
        if unknown >= UNKNOWN_MOONS_PER_RUN {
            break;
        }
        let moon_id = int(row, 0);
        if !budget.take() {
            break;
        }
        // Public: any subject.
        match esi::get(
            "universe-moon",
            Subject::Character(0),
            &[("moon_id".to_owned(), moon_id.to_string())],
            None,
        ) {
            Ok(response) => {
                let Ok(moon) = serde_json::from_str::<Moon>(&response.body) else {
                    log::warn(format!("moon {moon_id}: unexpected answer"));
                    continue;
                };
                storage::transaction(&[
                    Statement::new(
                        "INSERT INTO names (id, name, category) VALUES ($1, $2, 'moon') \
                         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                        vec![moon_id.into(), moon.name.into()],
                    ),
                    Statement::new(
                        "UPDATE moons SET system_id = $2, checked_at = now() WHERE moon_id = $1",
                        vec![moon_id.into(), moon.system_id.into()],
                    ),
                ])
                .map_err(|e| retry("storing a moon", e))?;
            }
            Err(esi::Error::Status(404)) => {
                unknown += 1;
                // Not a moon: a survey with a made-up id.
                log::warn(format!(
                    "moon {moon_id}: ESI doesn't know it; its survey is dropped"
                ));
                storage::transaction(&[
                    Statement::new(
                        "DELETE FROM surveys WHERE moon_id = $1",
                        vec![moon_id.into()],
                    ),
                    Statement::new(
                        "DELETE FROM moons m WHERE moon_id = $1 \
                         AND NOT EXISTS (SELECT 1 FROM extractions e WHERE e.moon_id = m.moon_id)",
                        vec![moon_id.into()],
                    ),
                ])
                .map_err(|e| retry("dropping an unknown moon", e))?;
            }
            Err(err) => log::warn(format!("moon {moon_id}: {err:?}")),
        }
    }
    // Security and place: public data, read through any data source
    // (`names` doesn't say where a system is).
    if let Some((_, subject)) = sources.first() {
        let missing = storage::query(
            "SELECT DISTINCT s.system_id FROM (SELECT system_id FROM moons UNION SELECT system_id FROM structures) s \
             WHERE s.system_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM systems y WHERE y.system_id = s.system_id) \
             ORDER BY s.system_id LIMIT $1",
            &[SYSTEMS_PER_RUN.into()],
        )
        .map_err(|e| retry("finding systems", e))?;
        for row in &missing.rows {
            let system = int(row, 0);
            // Two ESI requests behind one call.
            if !(budget.take() && budget.take()) {
                break;
            }
            match esi::get(
                "universe-system",
                *subject,
                &[("system_id".to_owned(), system.to_string())],
                None,
            ) {
                Ok(response) => {
                    let Ok(s) = serde_json::from_str::<SystemInfo>(&response.body) else {
                        log::warn(format!("system {system}: unexpected answer"));
                        continue;
                    };
                    storage::execute(
                        "INSERT INTO systems (system_id, security, constellation_id, region_id) \
                         VALUES ($1, $2, $3, $4) ON CONFLICT (system_id) DO UPDATE SET \
                         security = EXCLUDED.security, constellation_id = EXCLUDED.constellation_id, \
                         region_id = EXCLUDED.region_id",
                        &[
                            system.into(),
                            s.security_status.into(),
                            s.constellation_id.into(),
                            s.region_id.into(),
                        ],
                    )
                    .map_err(|e| retry("storing a system", e))?;
                }
                Err(err) => log::warn(format!("system {system}: {err:?}")),
            }
        }
    }
    let shown = storage::query(
        "SELECT DISTINCT id FROM ( \
             SELECT system_id AS id FROM moons UNION SELECT system_id FROM structures \
             UNION SELECT constellation_id FROM systems UNION SELECT region_id FROM systems \
             UNION SELECT corporation_id FROM extractions UNION SELECT type_id FROM ore_types) x \
             WHERE id IS NOT NULL AND id > 0",
        &[],
    )
    .map_err(|e| retry("reading ids to name", e))?;
    let ids: Vec<i64> = shown.rows.iter().map(|r| int(r, 0)).collect();
    learn_names(budget, &ids)
}

/// Moon ores' item groups and their rarity class.
const ORE_GROUPS: [(i64, i64); 5] = [(1884, 4), (1920, 8), (1921, 16), (1922, 32), (1923, 64)];

#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    types: Vec<i64>,
}

#[derive(Deserialize)]
struct Price {
    type_id: i64,
    average_price: Option<f64>,
    adjusted_price: Option<f64>,
}

/// Daily: moon ores and their rarity (ESI's item groups), then CCP's
/// prices of the ores Moon Mining values: moon ores, surveyed ores and
/// whatever the ledgers show was mined.
fn prices() -> Result<(), JobError> {
    let mut budget = Budget(ESI_BUDGET);
    let mut ores: Vec<serde_json::Value> = Vec::new();
    for (group, rarity) in ORE_GROUPS {
        if !budget.take() {
            break;
        }
        match esi::get(
            "universe-group",
            Subject::Character(0),
            &[("group_id".to_owned(), group.to_string())],
            None,
        ) {
            Ok(response) => match serde_json::from_str::<Group>(&response.body) {
                Ok(g) => ores.extend(
                    g.types
                        .into_iter()
                        .map(|t| serde_json::json!({ "type_id": t, "rarity": rarity })),
                ),
                Err(_) => log::warn(format!("item group {group}: unexpected answer")),
            },
            Err(err) => log::warn(format!("item group {group}: {err:?}")),
        }
    }
    if !ores.is_empty() {
        storage::execute(
            "INSERT INTO ore_types (type_id, rarity) \
             SELECT type_id, rarity FROM json_to_recordset($1::json) AS x(type_id bigint, rarity integer) \
             ON CONFLICT (type_id) DO UPDATE SET rarity = EXCLUDED.rarity",
            &[Db::json(serde_json::Value::Array(ores).to_string())],
        )
        .map_err(|e| retry("storing ore types", e))?;
    }
    if !budget.take() {
        return Ok(());
    }
    let body = match esi::get("markets-prices", Subject::Character(0), &[], None) {
        Ok(response) => response.body,
        Err(err) => return Err(retry("reading market prices", err)),
    };
    let all: Vec<Price> = serde_json::from_str(&body)
        .map_err(|e| JobError::Retry(format!("market prices: unexpected answer: {e}")))?;
    // Ids from ESI only (moon ores, and what the ledgers say was mined):
    // surveys hold only moon ores, checked at upload.
    let wanted = storage::query(
        "SELECT type_id FROM ore_types UNION SELECT DISTINCT type_id FROM ledger",
        &[],
    )
    .map_err(|e| retry("reading ore types", e))?;
    let wanted: std::collections::BTreeSet<i64> = wanted.rows.iter().map(|r| int(r, 0)).collect();
    let rows: Vec<serde_json::Value> = all
        .into_iter()
        .filter(|p| wanted.contains(&p.type_id))
        .map(|p| {
            serde_json::json!({
                "type_id": p.type_id,
                "average_price": p.average_price.filter(|v| v.is_finite()),
                "adjusted_price": p.adjusted_price.filter(|v| v.is_finite()),
            })
        })
        .collect();
    let stored = rows.len();
    storage::execute(
        "INSERT INTO prices (type_id, average_price, adjusted_price, updated_at) \
         SELECT type_id, average_price, adjusted_price, now() \
         FROM json_to_recordset($1::json) AS x(type_id bigint, average_price float8, adjusted_price float8) \
         ON CONFLICT (type_id) DO UPDATE SET average_price = EXCLUDED.average_price, \
         adjusted_price = EXCLUDED.adjusted_price, updated_at = now()",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map_err(|e| retry("storing prices", e))?;
    log::info(format!("prices of {stored} ore types updated"));
    let types: Vec<i64> = wanted.into_iter().collect();
    learn_names(&mut budget, &types)
}

/// Refused name look-ups a run tolerates.
const NAME_REFUSALS: usize = 4;

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
    // ESI refuses a whole batch for one id it doesn't know: halve a
    // refused batch to name the rest, but give up after a few refusals
    // (each is an ESI error).
    let mut todo: Vec<Vec<i64>> = missing.chunks(1000).map(<[i64]>::to_vec).collect();
    let mut refused = 0;
    while let Some(chunk) = todo.pop() {
        if !budget.take() {
            log::info("names: out of ESI calls this run");
            return Ok(());
        }
        let named = match esi::names(&chunk) {
            Ok(named) => named,
            Err(esi::Error::Status(404)) if chunk.len() > 1 && refused < NAME_REFUSALS => {
                refused += 1;
                let (a, b) = chunk.split_at(chunk.len() / 2);
                todo.push(a.to_vec());
                todo.push(b.to_vec());
                continue;
            }
            Err(err) => {
                log::warn(format!("names of {} ids: {err:?}", chunk.len()));
                refused += 1;
                if refused >= NAME_REFUSALS {
                    return Ok(());
                }
                continue;
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
                // ESI gives a row per corporation a pilot mined for that
                // day (and pages can repeat one): one ledger row each of
                // pilot, ore and day, under the corporation with the most.
                "INSERT INTO ledger (observer_id, character_id, type_id, day, corporation_id, quantity) \
                 SELECT $2, character_id, type_id, last_updated, \
                        (array_agg(recorded_corporation_id ORDER BY quantity DESC))[1], sum(quantity)::bigint \
                 FROM (SELECT DISTINCT character_id, type_id, last_updated, recorded_corporation_id, quantity \
                       FROM json_to_recordset($1::json) AS x(character_id bigint, type_id bigint, \
                            last_updated date, recorded_corporation_id bigint, quantity bigint)) x \
                 GROUP BY character_id, type_id, last_updated \
                 ON CONFLICT (observer_id, character_id, type_id, day) DO UPDATE SET \
                     quantity = EXCLUDED.quantity, corporation_id = EXCLUDED.corporation_id",
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
                    ["character_id", "type_id", "recorded_corporation_id"]
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

/// A player-chosen name made safe for Discord's markdown: no links,
/// code spans, emphasis, spoilers or strikethrough from its characters.
fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(
            c,
            '[' | ']' | '(' | ')' | '`' | '\\' | '*' | '_' | '~' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Text cut to `max` characters, marked where it was cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// At a pop: tell Members on Discord, once, as a card.
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
         LEFT JOIN names c ON c.id = x.corporation_id \
         LEFT JOIN structures s ON s.structure_id = x.structure_id \
         LEFT JOIN names y ON y.id = s.system_id \
         WHERE e.structure_id = $1 AND e.chunk_arrival = $2 AND NOT e.pinged AND e.cancelled_at IS NULL \
           AND x.structure_id = e.structure_id AND x.chunk_arrival = e.chunk_arrival \
         RETURNING coalesce(m.name, 'Moon ' || x.moon_id::text), \
                   coalesce(s.name, 'Structure ' || x.structure_id::text), \
                   coalesce(y.name, ''), x.natural_decay, x.corporation_id, c.name, s.type_id, \
                   (SELECT string_agg(coalesce(o.name, 'Type ' || p.type_id::text) || ' ' \
                            || round(p.amount * 100)::text || '%', ', ' ORDER BY p.amount DESC) \
                    FROM survey_products p LEFT JOIN names o ON o.id = p.type_id \
                    WHERE p.moon_id = x.moon_id)",
        &[
            payload.structure_id.into(),
            Db::timestamp(&payload.chunk_arrival),
        ],
    )
    .map_err(|e| retry("claiming the ping", e))?;
    let Some(row) = rows.rows.first() else {
        return Ok(());
    };
    let (moon, structure, system) = (
        escape(&text(row, 0)),
        escape(&text(row, 1)),
        escape(&text(row, 2)),
    );
    let popped = when(row, 3);
    let at = popped.map_or_else(String::new, |t| t.format("%H:%M").to_string());
    let mut card = discord::Embed::new(clip(&format!("Moon popped: {}", text(row, 0)), 256))
        .description(format!(
            "The chunk at {structure} fractured at {at} EVE. The ore is in space now and can \
             be mined."
        ))
        .color(0x2e_cc71)
        .field("Structure", clip(&structure, 1024))
        .field("Moon", clip(&moon, 1024))
        .footer("Moon Mining");
    if !system.is_empty() {
        card = card.field("System", clip(&system, 1024));
    }
    let ores = text(row, 7);
    if !ores.is_empty() {
        card = card.wide_field("Ore (survey)", clip(&escape(&ores), 1024));
    }
    let (corporation, corporation_name) = (int(row, 4), text(row, 5));
    if !corporation_name.is_empty() {
        card = card.author(
            clip(&corporation_name, 256),
            (corporation > 0).then_some(discord::Image::Corporation(corporation)),
        );
    }
    if let Some(type_id) = row.get(6).and_then(Db::as_integer).filter(|t| *t > 0) {
        card = card.thumbnail(discord::Image::TypeRender(type_id));
    }
    if let Some(t) = popped {
        card = card.timestamp(t.to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    match discord::send_embed(&channel, &card, Mention::State("Member".into())) {
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

/// The app's pages beside the title, as aa-moonmining's navbar: those the
/// viewer may open, and Upload moon
/// surveys as the button. Someone without `extractions_access` gets the
/// old-moon list in the Extractions' place.
fn with_links(page: Page, viewer: &Viewer) -> Result<Page, PageError> {
    let mut links: Vec<(&str, &str)> = Vec::new();
    if viewer.can("extractions_access") {
        links.push(("Extractions", ""));
    }
    // As aa-moonmining's navbar: Moons for everyone who opens the app.
    links.push(("Moons", "moons"));
    if viewer.can("reports_access") {
        links.push(("Reports", "reports"));
    }
    if viewer.can("extractions_access") {
        links.push(("Mining totals", "totals"));
        links.push(("Planner", "planner"));
    }
    if viewer.can("manage") {
        links.push(("Settings", "settings"));
    }
    let upload = viewer.can("upload_moon_scan");
    // The old-moon list (with the Members-only window on), for someone who
    // sees it but not the extractions.
    let window = settings()
        .map_err(|e| failed("reading settings", e))?
        .window();
    let mut page = if viewer.can("extractions_access") || !window {
        page
    } else {
        page.link("Old moons", "")
    };
    for (label, path) in links {
        page = page.link(label, path);
    }
    if upload {
        page = page.button("Upload moon surveys", "upload");
    }
    Ok(page)
}

/// A refinery: its type's icon and its name, when the type is known.
fn refinery(name: &str, type_id: i64) -> Value {
    if type_id > 0 {
        item_type(type_id, name).into()
    } else {
        name.into()
    }
}

/// The ledger rows that belong to an extraction (`e`): mined at its
/// refinery from the day the chunk arrived until two days after it
/// fractured, when the field is gone.
const LEDGER_WINDOW: &str = "l.observer_id = e.structure_id \
     AND l.day >= (e.chunk_arrival AT TIME ZONE 'UTC')::date \
     AND l.day <= ((e.natural_decay + interval '2 days') AT TIME ZONE 'UTC')::date";

/// An ore's unit price: CCP's average, else its adjusted price.
const PRICE: &str = "coalesce(pr.average_price, pr.adjusted_price, 0)";

/// One extraction, with its moon, place and value.
struct Pop {
    structure_id: i64,
    moon_id: i64,
    moon: String,
    structure: String,
    structure_type: i64,
    system: String,
    start: DateTime<Utc>,
    arrival: DateTime<Utc>,
    decay: DateTime<Utc>,
    cancelled: Option<DateTime<Utc>>,
    /// Σ share × unit price of the moon's survey (none without one).
    worth: Option<f64>,
    /// What the ledger says was mined from it, in ISK (none without
    /// ledger rows).
    mined: Option<f64>,
    /// The settings its value is worked out with.
    rates: value::Rates,
}

impl Pop {
    /// The chunk's estimated value, from the moon's survey.
    fn value(&self) -> Option<f64> {
        self.worth.map(|w| value::chunk(self.volume(), w))
    }

    /// The chunk's estimated volume, m³.
    fn volume(&self) -> f64 {
        self.rates.chunk_volume(self.start, self.arrival)
    }

    /// Its details page.
    fn details(&self) -> Value {
        link(
            "Details",
            format!(
                "extraction/{}/{}",
                self.structure_id,
                self.arrival.timestamp()
            ),
        )
        .into()
    }

    fn status(&self, now: DateTime<Utc>) -> Value {
        if self.cancelled.is_some() {
            badge("Cancelled", Tone::Neutral)
        } else if self.decay <= now {
            badge("Completed", Tone::Neutral)
        } else if self.arrival <= now {
            badge("Ready", Tone::Accent)
        } else {
            badge("Extracting", Tone::Neutral)
        }
        .into()
    }
}

/// Which extractions to read. Each is fixed SQL (never data).
#[derive(Clone, Copy)]
enum Which {
    /// Not cancelled, fracturing between two instants.
    Decaying,
    /// Cancelled, or ready more than 12 hours ago (AA's Past).
    Past,
    /// Every one at a moon.
    AtMoon,
    /// One, by refinery and chunk arrival.
    One,
}

fn extractions(which: Which, params: &[Db]) -> Result<Vec<Pop>, PageError> {
    let rates = rates()?;
    let filter = match which {
        Which::Decaying => {
            "e.cancelled_at IS NULL AND e.natural_decay > $1 AND e.natural_decay <= $2 \
             ORDER BY e.natural_decay LIMIT 500"
        }
        Which::Past => {
            "(e.cancelled_at IS NOT NULL OR e.chunk_arrival < $1) ORDER BY e.chunk_arrival DESC LIMIT 200"
        }
        Which::AtMoon => "e.moon_id = $1 ORDER BY e.chunk_arrival DESC LIMIT 20",
        Which::One => "e.structure_id = $1 AND e.chunk_arrival = $2",
    };
    let rows = storage::query(
        &format!(
            "SELECT e.structure_id, e.moon_id, coalesce(m.name, 'Moon ' || e.moon_id::text), \
                    coalesce(s.name, 'Structure ' || e.structure_id::text), coalesce(s.type_id, 0), \
                    coalesce(y.name, ''), e.extraction_start, e.chunk_arrival, e.natural_decay, \
                    e.cancelled_at, v.worth, mined.isk, mined.n, sy.security \
             FROM extractions e \
             LEFT JOIN names m ON m.id = e.moon_id \
             LEFT JOIN structures s ON s.structure_id = e.structure_id \
             LEFT JOIN names y ON y.id = s.system_id \
             LEFT JOIN systems sy ON sy.system_id = s.system_id \
             LEFT JOIN LATERAL (SELECT sum(p.amount * {PRICE})::float8 AS worth \
                  FROM survey_products p LEFT JOIN prices pr ON pr.type_id = p.type_id \
                  WHERE p.moon_id = e.moon_id) v ON true \
             LEFT JOIN LATERAL (SELECT count(*) AS n, sum(l.quantity * {PRICE})::float8 AS isk \
                  FROM ledger l LEFT JOIN prices pr ON pr.type_id = l.type_id \
                  WHERE {LEDGER_WINDOW}) mined ON true \
             WHERE {filter}"
        ),
        params,
    )
    .map_err(|e| failed("reading extractions", e))?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| {
            Some(Pop {
                structure_id: int(r, 0),
                moon_id: int(r, 1),
                moon: text(r, 2),
                structure: text(r, 3),
                structure_type: int(r, 4),
                system: system_label(&text(r, 5), float(r, 13)),
                start: when(r, 6)?,
                arrival: when(r, 7)?,
                decay: when(r, 8)?,
                cancelled: when(r, 9),
                worth: float(r, 10),
                mined: (int(r, 12) > 0).then(|| float(r, 11).unwrap_or_default()),
                rates,
            })
        })
        .collect())
}

fn pops(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Pop>, PageError> {
    extractions(
        Which::Decaying,
        &[Db::timestamp(rfc3339(from)), Db::timestamp(rfc3339(to))],
    )
}

/// The pops tables, newest first, at most `limit`: moon (its name says
/// the system), structure and, with `times`, when it popped. Three
/// columns, so Tether draws fresh and old moons side by side.
fn popped_table(title: &str, empty: &str, pops: &[Pop], limit: usize, times: bool) -> Table {
    let mut columns = vec![Column::text("Moon"), Column::text("Structure")];
    if times {
        columns.push(Column::numeric("Popped"));
    }
    with_rows(
        Table::new(columns).title(title).empty(empty),
        pops.iter().rev().take(limit).map(|p| {
            let mut row = vec![
                p.moon.clone().into(),
                refinery(&p.structure, p.structure_type),
            ];
            if times {
                row.push(time(rfc3339(p.decay)));
            }
            row
        }),
    )
}

/// aa-moonmining's Extractions (Upcoming and Past): the app's main page,
/// for `extractions_access`. Others open Moons, as aa-moonmining's index;
/// with the Members-only window on, they get the old-moon list instead,
/// and the page adds the fresh and old moons.
fn extractions_page(viewer: &Viewer) -> Result<Page, PageError> {
    let now = Utc::now();
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let window = settings.window();
    let old = pops(now - OLD_FOR, now - settings.fresh)?;
    let no_old = "No moons popped in the last two days.";
    if !viewer.can("extractions_access") {
        if !window {
            return moons::moons_page(viewer, &moons::Filter::default());
        }
        // Blue's whole page: which moons are open to them, as many as
        // Members' list shows, and never when they popped (that would map
        // out the pop schedule).
        return Ok(Page::new("Moon Mining")
            .description("Moons popped a while ago, still worth a visit")
            .table(popped_table(
                "Old moons",
                no_old,
                &old,
                settings.old_shown,
                false,
            )));
    }
    let fresh = pops(now - settings.fresh, now)?;
    let upcoming = pops(now, now + Duration::days(60))?;
    let past = extractions(Which::Past, &[Db::timestamp(rfc3339(now - settings.stale))])?;
    let ready = upcoming.iter().filter(|p| p.arrival <= now).count();
    let coming_value: f64 = upcoming.iter().filter_map(Pop::value).sum();
    let fresh_table = popped_table(
        "Fresh moons",
        "Nothing popped in the Members-only window.",
        &fresh,
        usize::MAX,
        true,
    );
    let upcoming_table = with_rows(
        Table::new(vec![
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Structure"),
            Column::text("Status"),
            Column::numeric("Chunk arrives"),
            Column::numeric("Auto-fracture"),
            Column::numeric("Value (est.)"),
            Column::numeric("Mined"),
            Column::numeric(""),
        ])
        .title("Extractions")
        .empty("No extractions running. Has an owner been added?"),
        upcoming.iter().map(|p| {
            vec![
                p.moon.clone().into(),
                p.system.clone().into(),
                refinery(&p.structure, p.structure_type),
                p.status(now),
                countdown(rfc3339(p.arrival)),
                time(rfc3339(p.decay)),
                isk_or_blank(p.value()),
                isk_or_blank(p.mined),
                p.details(),
            ]
        }),
    );
    let past_table = with_rows(
        Table::new(vec![
            Column::text("Moon"),
            Column::text("System"),
            Column::text("Structure"),
            Column::text("Status"),
            Column::numeric("Chunk arrived"),
            Column::numeric("Value (est.)"),
            Column::numeric("Mined"),
            Column::numeric(""),
        ])
        .title("Past extractions")
        .empty("No past extractions yet."),
        past.iter().map(|p| {
            vec![
                p.moon.clone().into(),
                p.system.clone().into(),
                refinery(&p.structure, p.structure_type),
                p.status(now),
                time(rfc3339(p.arrival)),
                isk_or_blank(p.value()),
                isk_or_blank(p.mined),
                p.details(),
            ]
        }),
    );
    let mut stats = vec![Stat::new(
        "Ready now",
        i64::try_from(ready).unwrap_or(i64::MAX),
    )];
    if window {
        stats.push(
            Stat::new("Fresh", i64::try_from(fresh.len()).unwrap_or(i64::MAX)).caption(format!(
                "popped in the last {}h",
                settings.fresh.num_hours()
            )),
        );
    }
    stats.extend([
        Stat::new(
            "Extracting",
            i64::try_from(upcoming.len() - ready).unwrap_or(i64::MAX),
        ),
        Stat::new("Coming (est.)", isk(value::finite(coming_value)))
            .caption("chunks of surveyed moons"),
    ]);
    let page = Page::new("Moon Mining")
        .description(if window {
            format!(
                "Fresh moons are for Members for {} hours after they pop, then Blue see them too. \
                 Values are estimates from moon surveys and CCP's ore prices.",
                settings.fresh.num_hours()
            )
        } else {
            "Values are estimates from moon surveys and CCP's ore prices.".to_owned()
        })
        .stats(stats);
    if !window {
        return Ok(page
            .tab("Extractions", vec![Section::Table(upcoming_table)])
            .tab("Past", vec![Section::Table(past_table)]));
    }
    // Fresh and old moons side by side, the newest old ones only.
    let mut page = page.table(fresh_table);
    if settings.old_shown > 0 {
        page = page.table(popped_table(
            "Old moons",
            no_old,
            &old,
            settings.old_shown,
            true,
        ));
    }
    Ok(page
        .tab("Extractions", vec![Section::Table(upcoming_table)])
        .tab("Past", vec![Section::Table(past_table)]))
}

fn totals_page() -> Result<Page, PageError> {
    let rows = storage::query(
        &format!(
            "SELECT l.character_id, coalesce(n.name, 'Character ' || l.character_id::text), \
                    coalesce(sum(l.quantity) FILTER (WHERE l.day >= date_trunc('month', now())::date), 0)::bigint, \
                    coalesce(sum(l.quantity) FILTER (WHERE l.day >= (now() - interval '30 days')::date), 0)::bigint, \
                    sum(l.quantity)::bigint, \
                    coalesce(sum(l.quantity * {PRICE}) FILTER (WHERE l.day >= (now() - interval '30 days')::date), 0)::float8 \
             FROM ledger l LEFT JOIN names n ON n.id = l.character_id \
             LEFT JOIN prices pr ON pr.type_id = l.type_id \
             GROUP BY l.character_id, n.name ORDER BY 4 DESC, 5 DESC LIMIT 500"
        ),
        &[],
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let ores = storage::query(
        &format!(
            "SELECT coalesce(n.name, 'Type ' || l.type_id::text), sum(l.quantity)::bigint, l.type_id, \
                    sum(l.quantity * {PRICE})::float8 \
             FROM ledger l LEFT JOIN names n ON n.id = l.type_id \
             LEFT JOIN prices pr ON pr.type_id = l.type_id \
             WHERE l.day >= (now() - interval '30 days')::date \
             GROUP BY l.type_id, n.name ORDER BY 2 DESC LIMIT 100"
        ),
        &[],
    )
    .map_err(|e| failed("reading the ledger", e))?;
    let pilots = with_rows(
        Table::new(vec![
            Column::text("Pilot"),
            Column::numeric("This month"),
            Column::numeric("Last 30 days"),
            Column::numeric("All time"),
            Column::numeric("Value, last 30 days"),
        ])
        .title("By pilot (units)")
        .empty("Nothing mined yet, or no observers readable."),
        rows.rows.iter().map(|r| {
            vec![
                character(int(r, 0), text(r, 1)).into(),
                int(r, 2).into(),
                int(r, 3).into(),
                int(r, 4).into(),
                isk(value::finite(float(r, 5).unwrap_or_default())),
            ]
        }),
    );
    let ore_table = with_rows(
        Table::new(vec![
            Column::text("Ore"),
            Column::numeric("Last 30 days"),
            Column::numeric("Value"),
        ])
        .title("By ore (units)")
        .empty("Nothing mined in the last 30 days."),
        ores.rows.iter().map(|r| {
            vec![
                item_type(int(r, 2), text(r, 0)).into(),
                int(r, 1).into(),
                isk(value::finite(float(r, 3).unwrap_or_default())),
            ]
        }),
    );
    Ok(Page::new("Mining totals")
        .description(
            "From the corporations' mining observers, refreshed every 6 hours; values at CCP's \
             average ore prices",
        )
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

/// A drill's status badge, when it pops, and what to do there.
fn advice_parts(advice: &Advice) -> (Value, Value, Value) {
    match advice {
        Advice::OnSlot { pop, .. } => (
            badge("On slot", Tone::Success).into(),
            time(rfc3339(*pop)),
            "Leave it".into(),
        ),
        Advice::OffSlot { pop, slot, off } => (
            badge("Off slot", Tone::Warning).into(),
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
            badge("Overlap", Tone::Danger).into(),
            time(rfc3339(*pop)),
            format!("Pops in the same slot as {with}").into(),
        ),
        Advice::Start {
            arrival, duration, ..
        } => (
            badge("Idle", Tone::Accent).into(),
            time(rfc3339(*arrival + planner::AUTO_FRACTURE)),
            format!(
                "Start now: {} (chunk arrives {} EVE)",
                planner::duration_text(*duration),
                arrival.format("%d %b %H:%M")
            )
            .into(),
        ),
        Advice::NoSlot => (
            badge("Idle", Tone::Warning).into(),
            "".into(),
            "No free slot within 56 days: widen the cadence".into(),
        ),
    }
}

/// The plan on a timeline: a lane per drill with its pop (or the start
/// the planner proposes, dashed), and the cadence's slots shaded.
fn plan_timeline(
    advice: &[(Drill, Advice)],
    cadence: Cadence,
    now: DateTime<Utc>,
    title: String,
) -> Timeline {
    let from = now - Duration::hours(6);
    let to = now + planner::GAP_HORIZON;
    let mut timeline = Timeline::new(rfc3339(from), rfc3339(to)).title(title);
    let mut slot = cadence.first_from(from);
    let mut windows = 0;
    while slot <= to && windows < 60 {
        timeline = timeline.window(
            rfc3339(slot - planner::ON_SLOT),
            rfc3339(slot + planner::ON_SLOT),
        );
        slot += cadence.every.max(Duration::hours(1));
        windows += 1;
    }
    for (drill, a) in advice.iter().take(20) {
        let lane = Lane::new(drill.name.clone());
        let lane = match a {
            Advice::OnSlot { pop, .. } => {
                lane.item(LaneItem::new("Pops", rfc3339(*pop)).tone(Tone::Success))
            }
            Advice::OffSlot { pop, .. } => {
                lane.item(LaneItem::new("Pops off slot", rfc3339(*pop)).tone(Tone::Warning))
            }
            Advice::Overlap { pop, with, .. } => lane
                .item(LaneItem::new(format!("Pops with {with}"), rfc3339(*pop)).tone(Tone::Danger)),
            Advice::Start {
                arrival, duration, ..
            } => lane.item(
                LaneItem::new(
                    format!("Start now: {}", planner::duration_text(*duration)),
                    rfc3339(now),
                )
                .until(rfc3339(*arrival + planner::AUTO_FRACTURE))
                .planned(),
            ),
            Advice::NoSlot => lane.caption("Idle: no free slot"),
        };
        timeline = timeline.lane(lane);
    }
    timeline
}

/// One of a corporation's refineries, as the planner lists it.
struct Refinery {
    drill: Drill,
    system: String,
    /// The moon of its latest extraction: ESI doesn't say which moon an
    /// idle refinery sits on.
    moon: String,
    /// Its last pop, for an idle one.
    last_pop: Option<DateTime<Utc>>,
}

fn refineries(corp: i64, now: DateTime<Utc>) -> Result<Vec<Refinery>, PageError> {
    let rows = storage::query(
        "SELECT s.structure_id, s.name, \
                (SELECT max(e.natural_decay) FROM extractions e WHERE e.structure_id = s.structure_id \
                 AND e.natural_decay > $2 AND e.cancelled_at IS NULL), \
                coalesce(y.name, ''), \
                (SELECT coalesce(m.name, 'Moon ' || e.moon_id::text) FROM extractions e \
                 LEFT JOIN names m ON m.id = e.moon_id WHERE e.structure_id = s.structure_id \
                 ORDER BY e.chunk_arrival DESC LIMIT 1), \
                (SELECT max(e.natural_decay) FROM extractions e WHERE e.structure_id = s.structure_id \
                 AND e.natural_decay <= $2 AND e.cancelled_at IS NULL) \
         FROM structures s LEFT JOIN names y ON y.id = s.system_id \
         WHERE s.corporation_id = $1 AND s.drill IS NOT FALSE ORDER BY s.name",
        &[corp.into(), Db::timestamp(rfc3339(now))],
    )
    .map_err(|e| failed("reading structures", e))?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Refinery {
            drill: Drill {
                structure_id: int(r, 0),
                name: text(r, 1),
                pop: when(r, 2),
            },
            system: text(r, 3),
            moon: text(r, 4),
            last_pop: when(r, 5),
        })
        .collect())
}

/// Every refinery of every corporation Moon Mining reads, planned against
/// its corporation's cadence, the idle ones first. Station Managers set
/// their corporation's cadence.
fn planner_page(viewer: &Viewer) -> Result<Page, PageError> {
    let managed = station_manager_corporations(viewer)?;
    let corporations = storage::query(
        "SELECT c.corporation_id, coalesce(n.name, 'Corporation ' || c.corporation_id::text) \
         FROM (SELECT DISTINCT corporation_id FROM structures WHERE drill IS NOT FALSE) c \
         LEFT JOIN names n ON n.id = c.corporation_id ORDER BY 2",
        &[],
    )
    .map_err(|e| failed("reading corporations", e))?;
    let mut page = Page::new("Extraction planner").description(
        "Go structure to structure and set each idle drill to the duration shown, so moons pop \
         on a steady cadence. Pops count the automatic fracture, three hours after the chunk \
         arrives. Tether can't start extractions: this only advises.",
    );
    if corporations.rows.is_empty() {
        return Ok(page.text(
            "No refineries yet: the planner lists the Athanors and Tataras of the corporations \
             Moon Mining reads.",
        ));
    }
    let now = Utc::now();
    let mut idle: Vec<Vec<Value>> = Vec::new();
    let mut plans = Vec::new();
    for row in &corporations.rows {
        let (corp, corp_name) = (int(row, 0), text(row, 1));
        let (every, at) = cadence_for(corp)?;
        let cadence = Cadence {
            every: Duration::hours(every),
            at: NaiveTime::parse_from_str(&at, "%H:%M")
                .unwrap_or(NaiveTime::from_hms_opt(19, 0, 0).unwrap_or_default()),
        };
        let refineries = refineries(corp, now)?;
        let drills: Vec<Drill> = refineries.iter().map(|r| r.drill.clone()).collect();
        let plan = planner::plan(&drills, cadence, now);
        let mut rows = Vec::new();
        for ((drill, advice), refinery) in plan.advice.iter().zip(&refineries) {
            let (status, pops, action) = advice_parts(advice);
            if matches!(advice, Advice::Start { .. } | Advice::NoSlot) {
                idle.push(vec![
                    drill.name.clone().into(),
                    corp_name.clone().into(),
                    refinery.system.clone().into(),
                    refinery.moon.clone().into(),
                    refinery
                        .last_pop
                        .map_or_else(|| "".into(), |t| time(rfc3339(t))),
                    action.clone(),
                ]);
            }
            rows.push(vec![
                drill.name.clone().into(),
                refinery.moon.clone().into(),
                status,
                pops,
                action,
            ]);
        }
        plans.push((corp, corp_name, every, at, cadence, plan, rows));
    }
    page = page.table(with_rows(
        Table::new(vec![
            Column::text("Structure"),
            Column::text("Corporation"),
            Column::text("System"),
            Column::text("Last moon"),
            Column::numeric("Last pop"),
            Column::text("What to do"),
        ])
        .title("Idle refineries")
        .empty("None idle: every drill has a chunk coming."),
        idle,
    ));
    for (corp, corp_name, every, at, cadence, plan, rows) in plans {
        if !plan.advice.is_empty() {
            page = page.timeline(plan_timeline(
                &plan.advice,
                cadence,
                now,
                format!("{corp_name}: the next two weeks"),
            ));
        }
        if managed.contains(&corp) {
            page = page.form(
                Form::new(format!("cadence_{corp}"), "Save cadence")
                    .title(format!("{corp_name}'s cadence"))
                    .description("One pop every so many hours, lined up on a time of day (EVE).")
                    .field(
                        Field::number("every_hours", "Keep pops apart by (hours)")
                            .range(Some(1.0), Some(168.0), true)
                            .value(every.to_string())
                            .help("Any whole number of hours, 1 to 168: 12, 24 and 48 are common.")
                            .required(),
                    )
                    .field(
                        Field::text("at_time", "Lined up on (HH:MM EVE)", 5)
                            .value(at.clone())
                            .required(),
                    ),
            );
        }
        page = page
            .table(with_rows(
                Table::new(vec![
                    Column::text("Structure"),
                    Column::text("Last moon"),
                    Column::text("Status"),
                    Column::numeric("Pops"),
                    Column::text("What to do"),
                ])
                .title(format!(
                    "Refineries of {corp_name}: a pop every {every}h at {at} EVE"
                ))
                .empty("No refineries seen for this corporation yet."),
                rows,
            ))
            .table(with_rows(
                Table::new(vec![Column::numeric("Slot with no pop")])
                    .title(format!("{corp_name}: gaps in the next two weeks"))
                    .empty("Every slot has a pop."),
                plan.gaps.iter().take(100).map(|g| vec![time(rfc3339(*g))]),
            ));
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
                    .range(Some(0.0), Some(48.0), true)
                    .value(settings.fresh.num_hours().to_string())
                    .help(
                        "Not in aa-moonmining: 0 is off, as there. On, those with basic_access \
                         alone (Blue) see popped moons after this many hours, on an old-moon list.",
                    )
                    .required(),
            )
            .field(
                // A channel no longer assigned to the app starts on "No
                // pings": a select can't start on a value it doesn't list.
                Field::select("ping_channel", "Ping pops to", channels.clone())
                    .value(
                        settings
                            .channel
                            .filter(|c| channels.iter().any(|(id, _)| id == c))
                            .unwrap_or_default(),
                    )
                    .help("Not in aa-moonmining: no channel, no pings."),
            )
            .field(Field::checkbox(
                "pings",
                "Ping Members at each pop",
                settings.pings,
            ))
            .field(
                Field::number("volume_per_day", "Ore a drill pulls a day (m³)")
                    .range(Some(1.0), Some(10_000_000.0), true)
                    .value(format!("{:.0}", settings.rates.per_day))
                    .help("aa-moonmining's MOONMINING_VOLUME_PER_DAY: 960400 unless CCP changes it")
                    .required(),
            )
            .field(
                Field::number("days_per_month", "Days in a month")
                    .range(Some(28.0), Some(31.0), false)
                    .value(settings.rates.days_per_month.to_string())
                    .help("aa-moonmining's MOONMINING_DAYS_PER_MONTH: 30.4. Moons' monthly value uses both")
                    .required(),
            )
            .field(
                Field::number("stale_hours", "Hours after the chunk arrives until an extraction is Past")
                    .range(Some(1.0), Some(168.0), true)
                    .value(settings.stale.num_hours().to_string())
                    .help("aa-moonmining's MOONMINING_COMPLETED_EXTRACTIONS_HOURS_UNTIL_STALE: 12")
                    .required(),
            )
            .field(
                Field::number("old_moons_shown", "Old moons shown beside fresh ones")
                    .range(Some(0.0), Some(50.0), true)
                    .value(settings.old_shown.to_string())
                    .help("Not in aa-moonmining: the newest moons popped in the last two days, beside the fresh ones on Extractions and on Blue's old-moon list (without when they popped), while the Members-only window is on. 0 shows none")
                    .required(),
            ),
    ))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let hours: i64 = submission
        .value("fresh_hours")
        .parse()
        .map_err(|_| PageError::Failed("fresh_hours wasn't a number".into()))?;
    let channel = submission.value("ping_channel");
    // Checked against the form's ranges by the host.
    let number = |name: &str| -> Result<f64, PageError> {
        submission
            .value(name)
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| PageError::Failed(format!("{name} wasn't a number")))
    };
    let (per_day, days) = (number("volume_per_day")?, number("days_per_month")?);
    let stale: i64 = submission
        .value("stale_hours")
        .parse()
        .map_err(|_| PageError::Failed("stale_hours wasn't a number".into()))?;
    let old_shown: i64 = submission
        .value("old_moons_shown")
        .parse()
        .map_err(|_| PageError::Failed("old_moons_shown wasn't a number".into()))?;
    storage::execute(
        "UPDATE settings SET fresh_hours = $1, ping_channel = $2, pings = $3, volume_per_day = $4, \
         days_per_month = $5, stale_hours = $6, old_moons_shown = $7 WHERE id = 1",
        &[
            hours.into(),
            (!channel.is_empty()).then(|| channel.to_owned()).into(),
            submission.checked("pings").into(),
            per_day.into(),
            days.into(),
            stale.into(),
            old_shown.into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    // Admins see who changed what in the plugin's log.
    log::info(format!(
        "settings changed by {} ({}): members-only {hours}h, channel {channel:?}, pings {}, \
         {per_day} m³ a day, {days} days a month, past after {stale}h, {old_shown} old moons shown",
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
        return Ok(SubmitResult::Page(with_links(
            planner_page(viewer)?.text("Write the time as HH:MM, e.g. 19:00."),
            viewer,
        )?));
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
