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
//! - Admin notices (aa-moonmining's MOONMINING_ADMIN_NOTIFICATIONS_ENABLED,
//!   on for a new install): holders of `manage`, superusers included, hear
//!   when an owner is added and when ESI refuses one's refineries. They
//!   name the corporation, never the character: only app admins see who
//!   the data sources are.

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
use tether_plugin_sdk::notify::{self, Level};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Lane, LaneItem, Page, PageError, Plugin, Request, Section, SettingsForm,
    SettingsGroup, Stat, Submission, SubmitResult, Table, Timeline, Tone, Value, badge, character,
    countdown, isk, item_type, link, log, time,
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
        match segments.as_slice() {
            [""] => extractions_page(&viewer),
            ["moons"] => moons::moons_page(&viewer, &moons::Filter::from(&request)),
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
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("settings", "settings") => save_settings(&viewer, &submission),
            ("planner", form) if form.starts_with("cadence_") => {
                save_cadence(&viewer, form, &submission)
            }
            ("upload", "survey") => moons::upload(&viewer, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(None),
            SYNC_MORE => sync(Some(&job)),
            "ledger" => ledger(None),
            LEDGER_MORE => ledger(Some(&job)),
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
    /// aa-moonmining's MOONMINING_ADMIN_NOTIFICATIONS_ENABLED.
    admin_notices: bool,
    /// Whether the owners in use when admin notices arrived are recorded
    /// (as told already).
    sources_known: bool,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT fresh_hours, ping_channel, pings, volume_per_day, days_per_month, stale_hours, \
                old_moons_shown, admin_notifications, sources_known \
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
        admin_notices: row
            .and_then(|r| r.get(7))
            .and_then(Db::as_bool)
            .unwrap_or(false),
        sources_known: row
            .and_then(|r| r.get(8))
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
/// Calls a run keeps back for names.
const NAME_RESERVE: usize = 8;
/// Calls a sync keeps back for moons, systems and names.
const PLACES_RESERVE: usize = 10;
/// The sync run that carries on where one out of ESI calls stopped.
const SYNC_MORE: &str = "sync_more";
/// The ledger run that carries on where one out of ESI calls stopped.
const LEDGER_MORE: &str = "ledger_more";
/// A ledger tried this recently isn't read again (an SQL interval): ESI
/// caches it an hour, and this leaves room for follow-up runs and the
/// scheduler's drift.
const LEDGER_FRESH: &str = "50 minutes";
/// Refused ledger reads a run tolerates: each is an ESI error, and the
/// host throttles an app at 30 in five minutes.
const LEDGER_REFUSALS: usize = 4;
/// Station Manager entries not confirmed for this long are dropped (the
/// corporation's roles can't be read any more).
const MANAGERS_KEPT: &str = "2 days";

/// A `ledger_more` run's corporations: those whose observers this round
/// listed, and those it has yet to list, in turn.
#[derive(Deserialize)]
struct LedgerMore {
    corporations: Vec<i64>,
    unlisted: Vec<i64>,
}

/// A `sync_more` run's round: when the scheduled run began. Corporations
/// read since are done.
#[derive(Deserialize)]
struct SyncMore {
    since: String,
}

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

/// Why `get_pages` has no answer.
enum Missed {
    /// The run's ESI calls ran out, before the first page or between
    /// pages.
    Budget,
    /// ESI (or the host) said no: logged.
    Esi(esi::Error),
}

/// Every page of an endpoint, within the budget.
fn get_pages(
    budget: &mut Budget,
    endpoint: &str,
    subject: Subject,
    params: &[(String, String)],
    what: &str,
) -> Result<Vec<String>, Missed> {
    if !budget.take() {
        return Err(Missed::Budget);
    }
    let first = match esi::get(endpoint, subject, params, Some(1)) {
        Ok(first) => first,
        Err(err) => {
            log::warn(format!("{what}: {}", esi::describe(&err)));
            return Err(Missed::Esi(err));
        }
    };
    let mut bodies = vec![first.body];
    for page in 2..=first.pages {
        if !budget.take() {
            log::info(format!("{what}: out of ESI calls this run"));
            return Err(Missed::Budget);
        }
        match esi::get(endpoint, subject, params, Some(page)) {
            Ok(response) => bodies.push(response.body),
            Err(err) => {
                log::warn(format!("{what}: {}", esi::describe(&err)));
                return Err(Missed::Esi(err));
            }
        }
    }
    Ok(bodies)
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
    by_corporation(&esi::data_sources())
}

fn by_corporation(sources: &[esi::Character]) -> Vec<(i64, Subject)> {
    let mut seen = Vec::new();
    for source in sources {
        if !seen.iter().any(|(c, _)| *c == source.corporation_id) {
            seen.push((source.corporation_id, Subject::DataSource(source.id)));
        }
    }
    seen
}

// ---- admin notices -----------------------------------------------------------

/// Notices a run sends at most (the host allows 10 notify calls a run).
const NOTICES_PER_RUN: usize = 8;
/// Owners announced a run at most.
const OWNERS_PER_RUN: i64 = 5;

/// aa-moonmining's admin notices (MOONMINING_ADMIN_NOTIFICATIONS_ENABLED),
/// as AA's notify_admins tells every superuser: here every holder of
/// `manage`, superusers included. Best effort.
fn tell_admins(left: &mut usize, title: &str, message: &str, level: Level) {
    if *left == 0 {
        return;
    }
    *left -= 1;
    if let Err(err) = notify::holders("manage", title, message, level, None) {
        log::warn(format!("an admin notice wasn't sent: {err:?}"));
    }
}

/// Records the owners (data sources) in use, so each one added is
/// announced once. The first sync after admin notices arrived records an
/// instance's owners as told already. A row stays when its source goes:
/// the host's list can come back empty on a fault, and dropping rows
/// would announce everyone again.
fn note_sources(s: &Settings, all: &[esi::Character]) -> Result<(), JobError> {
    if all.is_empty() {
        return Ok(());
    }
    let rows: Vec<serde_json::Value> = all
        .iter()
        .map(|c| serde_json::json!({ "character_id": c.id, "corporation_id": c.corporation_id }))
        .collect();
    storage::transaction(&[
        Statement::new(
            "INSERT INTO sources (character_id, corporation_id, announced) \
             SELECT character_id, corporation_id, $2 \
             FROM json_to_recordset($1::json) AS x(character_id bigint, corporation_id bigint) \
             ON CONFLICT (character_id, corporation_id) DO NOTHING",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                (!s.sources_known).into(),
            ],
        ),
        Statement::new(
            "UPDATE settings SET sources_known = true WHERE NOT sources_known",
            vec![],
        ),
    ])
    .map_err(|e| retry("recording owners", e))?;
    Ok(())
}

/// aa-moonmining's "Owner added" notice for each owner not told yet, a
/// few a run. aa-moonmining names who added it; here it names the
/// corporation alone, as holders of `manage` don't see data sources. One
/// whose corporation has no name yet (ESI said no) waits for a sync that
/// names it, or a day. With the notices off they're marked told, so
/// turning them on announces only owners added after.
fn announce_sources(s: &Settings, notices: &mut usize) -> Result<(), JobError> {
    if !s.admin_notices {
        storage::execute(
            "UPDATE sources SET announced = true WHERE NOT announced",
            &[],
        )
        .map_err(|e| retry("marking owners told", e))?;
        return Ok(());
    }
    let rows = storage::query(
        "SELECT s.character_id, s.corporation_id, n.name, s.seen_at < now() - interval '1 day' \
         FROM sources s LEFT JOIN names n ON n.id = s.corporation_id \
         WHERE NOT s.announced ORDER BY s.seen_at LIMIT $1",
        &[OWNERS_PER_RUN.into()],
    )
    .map_err(|e| retry("reading owners", e))?;
    for row in &rows.rows {
        if *notices == 0 {
            break;
        }
        let corp = int(row, 1);
        let corporation = match row.get(2).and_then(Db::as_text) {
            Some(name) => name.to_owned(),
            None if row.get(3).and_then(Db::as_bool) == Some(true) => format!("corporation {corp}"),
            None => continue,
        };
        tell_admins(
            notices,
            &format!("Owner added: {corporation}"),
            &format!("{corporation} was added as a new owner."),
            Level::Info,
        );
        storage::execute(
            "UPDATE sources SET announced = true WHERE character_id = $1 AND corporation_id = $2",
            &[int(row, 0).into(), corp.into()],
        )
        .map_err(|e| retry("marking an owner told", e))?;
    }
    Ok(())
}

/// aa-moonmining's "Owner disabled" notice, once a failing streak: ESI
/// refused (403) the owner's refineries. aa-moonmining disables the owner
/// there; Moon Mining keeps reading through it, and tells again only
/// after a read has worked. aa-moonmining names the sync character; here
/// an app admin finds it on the Data sources page.
fn owner_refused(
    s: &Settings,
    notices: &mut usize,
    character: i64,
    corp: i64,
    err: &esi::Error,
) -> Result<(), JobError> {
    // A token that stopped working sends nothing, in aa-moonmining too.
    if !matches!(err, esi::Error::Status(403)) || (s.admin_notices && *notices == 0) {
        return Ok(());
    }
    let rows = storage::query(
        "UPDATE sources s SET failing_since = now() \
         WHERE character_id = $1 AND corporation_id = $2 AND failing_since IS NULL \
         RETURNING (SELECT n.name FROM names n WHERE n.id = s.corporation_id)",
        &[character.into(), corp.into()],
    )
    .map_err(|e| retry("marking an owner failing", e))?;
    let Some(row) = rows.rows.first() else {
        return Ok(());
    };
    if s.admin_notices {
        let corporation = row
            .first()
            .and_then(Db::as_text)
            .map_or_else(|| format!("corporation {corp}"), str::to_owned);
        tell_admins(
            notices,
            &format!("Owner can't be read: {corporation}"),
            &format!(
                "Moon Mining can no longer read {corporation}'s refineries: {}. It keeps trying \
                 at each sync; an app admin can check the owner on its Data sources page.",
                esi::describe(err)
            ),
            Level::Danger,
        );
    }
    Ok(())
}

/// A read that worked ends the owner's failing streak.
fn owner_read(character: i64, corp: i64) -> Result<(), JobError> {
    storage::execute(
        "UPDATE sources SET failing_since = NULL \
         WHERE character_id = $1 AND corporation_id = $2 AND failing_since IS NOT NULL",
        &[character.into(), corp.into()],
    )
    .map_err(|e| retry("marking an owner read", e))?;
    Ok(())
}

// ---- the sync ----------------------------------------------------------------

/// Every 10 minutes, as aa-moonmining's run_regular_updates: the owners'
/// corporations' names (for the admin notices), each corporation's
/// extractions and refineries, the longest unread first, a ping queued
/// for each new or moved pop, the admin notices, then places and names. A
/// run out of ESI calls carries on a minute later (`sync_more`) with the
/// corporations it didn't reach, so every one is read however many there
/// are.
fn sync(more: Option<&Job>) -> Result<(), JobError> {
    let now = Utc::now();
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let all = esi::data_sources();
    note_sources(&settings, &all)?;
    let sources = by_corporation(&all);
    if sources.is_empty() {
        log::info("no data sources added yet");
        return Ok(());
    }
    // This round began when the scheduled run did (the database's clock):
    // a follow-up skips corporations read since.
    let since = match more {
        Some(job) => {
            serde_json::from_str::<SyncMore>(&job.payload)
                .map_err(|e| JobError::Permanent(format!("sync_more payload: {e}")))?
                .since
        }
        None => {
            let rows =
                storage::query("SELECT now()", &[]).map_err(|e| retry("reading the time", e))?;
            rows.rows.first().map(|r| text(r, 0)).unwrap_or_default()
        }
    };
    let read = storage::query(
        "SELECT corporation_id, synced_at, synced_at >= $1 FROM corporations",
        &[Db::timestamp(&since)],
    )
    .map_err(|e| retry("reading corporations", e))?;
    let mut due: Vec<(Option<DateTime<Utc>>, i64, Subject)> = sources
        .iter()
        .filter_map(|(corp, subject)| {
            let row = read.rows.iter().find(|r| int(r, 0) == *corp);
            let this_round = row.and_then(|r| r.get(2)).and_then(Db::as_bool) == Some(true);
            (!this_round).then(|| (row.and_then(|r| when(r, 1)), *corp, *subject))
        })
        .collect();
    // Never read first, then the longest unread.
    due.sort_by_key(|(at, _, _)| *at);
    let mut budget = Budget(ESI_BUDGET - PLACES_RESERVE);
    // Named before the calls go, so the notices name them: a call only
    // when an owner's corporation is new.
    let owners: Vec<i64> = sources.iter().map(|(corp, _)| *corp).collect();
    learn_names(&mut budget, &owners)?;
    let full = budget.0;
    let mut queue = QUEUE_BUDGET;
    let mut notices = NOTICES_PER_RUN;
    let (mut done, mut left) = (0, 0);
    for (i, (_, corp, subject)) in due.iter().enumerate() {
        // Two calls at least: extractions, then refineries.
        if budget.0 < 2 {
            left = due.len() - i;
            break;
        }
        let whole = budget.0 == full;
        let character = match subject {
            Subject::DataSource(id) => *id,
            _ => 0,
        };
        match read_corporation(&mut budget, &mut queue, *corp, *subject, now)? {
            Read::Done => owner_read(character, *corp)?,
            Read::Refused(err) => owner_refused(&settings, &mut notices, character, *corp, &err)?,
            // Its pages ran past the run's calls: the next run reads it
            // first, with all of them. One that had them all waits for
            // the next round.
            Read::OutOfCalls if !whole => {
                left = due.len() - i;
                break;
            }
            Read::OutOfCalls => log::warn(format!(
                "corporation {corp}: its structures have more pages than one run may read"
            )),
        }
        storage::execute(
            "INSERT INTO corporations (corporation_id, synced_at) VALUES ($1, now()) \
             ON CONFLICT (corporation_id) DO UPDATE SET synced_at = now()",
            &[(*corp).into()],
        )
        .map_err(|e| retry("marking a corporation read", e))?;
        done += 1;
    }
    // A ping at each coming pop not queued for its time yet.
    let pops = storage::query(
        "SELECT structure_id, chunk_arrival, natural_decay FROM extractions \
         WHERE natural_decay > $1 AND cancelled_at IS NULL AND queued_for IS DISTINCT FROM natural_decay \
         ORDER BY natural_decay LIMIT $2",
        &[Db::timestamp(rfc3339(now)), (queue as i64).into()],
    )
    .map_err(|e| retry("finding pops to queue", e))?;
    for row in &pops.rows {
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
    announce_sources(&settings, &mut notices)?;
    budget.0 += PLACES_RESERVE;
    places(&mut budget, &sources)?;
    // Carry on while runs get somewhere. Queued last: a follow-up that
    // failed after queuing its own would end as replaced, its error lost.
    if left > 0 && done > 0 {
        log::info(format!(
            "{left} corporations wait for the next run, a minute on: out of ESI calls"
        ));
        jobs::enqueue(
            NewJob::new(SYNC_MORE)
                .key(SYNC_MORE)
                .payload(serde_json::json!({ "since": since }).to_string())
                .at(rfc3339(now + Duration::minutes(1))),
        )
        .map_err(|e| retry("queuing the next sync", e))?;
    }
    Ok(())
}

/// How reading a corporation went.
enum Read {
    /// Its refineries were read, and its extractions unless ESI refused
    /// them.
    Done,
    /// ESI refused its refineries (logged).
    Refused(esi::Error),
    /// The run's calls ran out on its pages.
    OutOfCalls,
}

/// One corporation's extractions (with the pings of those it no longer
/// has taken back) and refineries. ESI's refusals leave what's stored as
/// it was.
fn read_corporation(
    budget: &mut Budget,
    queue: &mut usize,
    corp: i64,
    subject: Subject,
    now: DateTime<Utc>,
) -> Result<Read, JobError> {
    match get_pages(
        budget,
        "corporation-mining-extractions",
        subject,
        &[],
        &format!("extractions for corporation {corp}"),
    ) {
        Ok(bodies) => {
            storage::transaction(&[
                Statement::new(
                    "INSERT INTO extractions (structure_id, chunk_arrival, moon_id, corporation_id, extraction_start, natural_decay, seen_at) \
                     SELECT structure_id, chunk_arrival_time, moon_id, $2, extraction_start_time, natural_decay_time, now() \
                     FROM json_to_recordset($1::json) AS x(structure_id bigint, moon_id bigint, \
                          extraction_start_time timestamptz, chunk_arrival_time timestamptz, natural_decay_time timestamptz) \
                     ON CONFLICT (structure_id, chunk_arrival) DO UPDATE SET moon_id = EXCLUDED.moon_id, \
                     natural_decay = EXCLUDED.natural_decay, seen_at = now(), cancelled_at = NULL",
                    vec![Db::json(concat(&bodies)), corp.into()],
                ),
                Statement::new(
                    "INSERT INTO moons (moon_id) SELECT DISTINCT moon_id FROM extractions ON CONFLICT DO NOTHING",
                    vec![],
                ),
            ])
            .map_err(|e| retry("storing extractions", e))?;
            // Ones that were coming but the corporation no longer has:
            // cancelled (a restart shows as a new one). Kept for the Past
            // tab; their pings go.
            let gone = storage::query(
                "UPDATE extractions SET cancelled_at = now() WHERE corporation_id = $1 AND chunk_arrival > $2 \
                 AND cancelled_at IS NULL AND seen_at < now() - interval '1 minute' \
                 RETURNING structure_id, chunk_arrival",
                &[corp.into(), Db::timestamp(rfc3339(now))],
            )
            .map_err(|e| retry("marking cancelled extractions", e))?;
            for row in &gone.rows {
                if *queue == 0 {
                    break;
                }
                *queue -= 1;
                let _ = jobs::cancel(&pop_key(int(row, 0), &text(row, 1)));
            }
        }
        Err(Missed::Budget) => return Ok(Read::OutOfCalls),
        Err(Missed::Esi(_)) => {}
    }
    // Refineries' names (a source without the role still has extractions).
    let bodies = match get_pages(
        budget,
        "corporation-structures",
        subject,
        &[],
        &format!("structures for corporation {corp}"),
    ) {
        Ok(bodies) => bodies,
        Err(Missed::Budget) => return Ok(Read::OutOfCalls),
        Err(Missed::Esi(err)) => return Ok(Read::Refused(err)),
    };
    let listed = Db::json(concat(&bodies));
    storage::transaction(&[
        Statement::new(
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
         drill = EXCLUDED.drill, gone_at = NULL, updated_at = now()",
        vec![
            listed.clone(),
            corp.into(),
            format!("{{{},{}}}", REFINERIES[0], REFINERIES[1]).into(),
        ],
        ),
        // aa-moonmining deletes refineries the corporation no longer lists
        // (`models/owners.py:210-211`): gone, they own no moon. Kept for
        // their past extractions' names.
        Statement::new(
            "UPDATE structures SET gone_at = now() WHERE corporation_id = $2 AND gone_at IS NULL \
             AND structure_id NOT IN (SELECT structure_id FROM json_to_recordset($1::json) AS x(structure_id bigint) \
                                      WHERE structure_id IS NOT NULL)",
            vec![listed, corp.into()],
        ),
    ])
    .map_err(|e| retry("storing structures", e))?;
    Ok(Read::Done)
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
             UNION SELECT corporation_id FROM extractions UNION SELECT type_id FROM ore_types \
             UNION SELECT corporation_id FROM sources) x \
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

/// Marks an observer's ledger tried for this hour, read or not. As
/// aa-moonmining, which stamps `ledger_last_update_at` before it fetches
/// (`_reset_update_status`), so a failed read waits for the next run; and
/// a ledger that can't be read or stored never holds the front of the
/// queue.
fn ledger_tried(observer: i64) -> Result<(), JobError> {
    storage::execute(
        "UPDATE observers SET synced_at = now() WHERE observer_id = $1",
        &[observer.into()],
    )
    .map_err(|e| retry("marking a ledger tried", e))?;
    Ok(())
}

/// How listing corporations' mining observers went.
struct Lists {
    /// Those listed.
    listed: Vec<i64>,
    /// Those the run's calls didn't reach, in turn.
    unlisted: Vec<i64>,
    /// How many were tried, listed or not.
    tried: usize,
}

/// Each corporation's mining observers, in turn, while the run's calls
/// last. Observers ESI no longer lists aren't read any more, as
/// aa-moonmining reads only the listed ones; what their ledgers held
/// stays. A list ESI refuses leaves its corporation out of this round:
/// its observers cost no errors.
fn list_observers(budget: &mut Budget, todo: &[(i64, Subject)]) -> Result<Lists, JobError> {
    let mut lists = Lists {
        listed: Vec::new(),
        unlisted: Vec::new(),
        tried: 0,
    };
    for (i, (corp, subject)) in todo.iter().enumerate() {
        let whole = budget.0 == ESI_BUDGET - NAME_RESERVE;
        match get_pages(
            budget,
            "corporation-mining-observers",
            *subject,
            &[],
            &format!("observers for corporation {corp}"),
        ) {
            Ok(bodies) => {
                let list = concat(&bodies);
                storage::transaction(&[
                    Statement::new(
                        "INSERT INTO observers (observer_id, corporation_id) \
                         SELECT observer_id, $2 FROM json_to_recordset($1::json) AS x(observer_id bigint) \
                         ON CONFLICT (observer_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id",
                        vec![Db::json(list.clone()), (*corp).into()],
                    ),
                    Statement::new(
                        "DELETE FROM observers o WHERE o.corporation_id = $2 AND NOT EXISTS \
                         (SELECT 1 FROM json_to_recordset($1::json) AS x(observer_id bigint) \
                          WHERE x.observer_id = o.observer_id)",
                        vec![Db::json(list), (*corp).into()],
                    ),
                ])
                .map_err(|e| retry("storing observers", e))?;
                lists.listed.push(*corp);
            }
            // Out of calls: this one and the rest wait for the next run,
            // which lists them first, with all its calls.
            Err(Missed::Budget) if !whole => {
                lists.unlisted = todo[i..].iter().map(|(c, _)| *c).collect();
                break;
            }
            // One that had them all waits for the next round.
            Err(Missed::Budget) => log::warn(format!(
                "corporation {corp}: its mining observers have more pages than one run may read"
            )),
            Err(Missed::Esi(_)) => {}
        }
        lists.tried += 1;
        storage::execute(
            "INSERT INTO corporations (corporation_id, listed_at) VALUES ($1, now()) \
             ON CONFLICT (corporation_id) DO UPDATE SET listed_at = now()",
            &[(*corp).into()],
        )
        .map_err(|e| retry("marking a corporation listed", e))?;
    }
    Ok(lists)
}

/// The corporations, the longest unlisted first (never listed before
/// any).
fn by_listing(sources: &[(i64, Subject)]) -> Result<Vec<(i64, Subject)>, JobError> {
    let rows = storage::query(
        "SELECT corporation_id, listed_at FROM corporations WHERE listed_at IS NOT NULL",
        &[],
    )
    .map_err(|e| retry("reading corporations", e))?;
    let mut order = sources.to_vec();
    order.sort_by_key(|(corp, _)| {
        rows.rows
            .iter()
            .find(|r| int(r, 0) == *corp)
            .and_then(|r| when(r, 1))
    });
    Ok(order)
}

/// Hourly, as aa-moonmining's run_report_updates: each corporation's
/// observers, the longest unlisted first, then every listed observer's
/// ledger not tried this hour, oldest first. A run out of ESI calls
/// carries on a minute later (`ledger_more`) with the corporations and
/// ledgers it didn't reach, so every one is read however many there are.
fn ledger(more: Option<&Job>) -> Result<(), JobError> {
    let mut budget = Budget(ESI_BUDGET - NAME_RESERVE);
    let sources = sources_by_corporation();
    // Those still read, in turn.
    let still = |corps: &[i64]| -> Vec<(i64, Subject)> {
        corps
            .iter()
            .filter_map(|corp| sources.iter().find(|(c, _)| c == corp).copied())
            .collect()
    };
    let (mut listed, todo) = match more {
        Some(job) => {
            let payload: LedgerMore = serde_json::from_str(&job.payload)
                .map_err(|e| JobError::Permanent(format!("ledger_more payload: {e}")))?;
            let listed: Vec<i64> = still(&payload.corporations)
                .into_iter()
                .map(|(c, _)| c)
                .collect();
            (listed, still(&payload.unlisted))
        }
        None => (Vec::new(), by_listing(&sources)?),
    };
    let lists = list_observers(&mut budget, &todo)?;
    listed.extend(&lists.listed);
    if listed.is_empty() && lists.unlisted.is_empty() {
        return Ok(());
    }
    let corporations: Vec<String> = listed.iter().map(i64::to_string).collect();
    // More rows than calls is enough: each costs at least one.
    let due = storage::query(
        &format!(
            "SELECT observer_id, corporation_id FROM observers \
             WHERE corporation_id = ANY(string_to_array($1, ',')::bigint[]) \
               AND (synced_at IS NULL OR synced_at < now() - interval '{LEDGER_FRESH}') \
             ORDER BY synced_at NULLS FIRST, observer_id LIMIT $2"
        ),
        &[corporations.join(",").into(), (ESI_BUDGET as i64).into()],
    )
    .map_err(|e| retry("reading observers", e))?;
    let mut people = Vec::new();
    // Ledgers stored, and observers marked tried, this run.
    let (mut stored, mut tried, mut refused) = (0, 0, 0);
    let mut out_of_calls = false;
    for row in &due.rows {
        let (observer, corp) = (int(row, 0), int(row, 1));
        let Some((_, subject)) = sources.iter().find(|(c, _)| *c == corp) else {
            continue;
        };
        if budget.0 == 0 {
            out_of_calls = true;
            break;
        }
        let params = [("observer_id".to_owned(), observer.to_string())];
        let had = budget.0;
        let bodies = match get_pages(
            &mut budget,
            "corporation-mining-observer",
            *subject,
            &params,
            &format!("observer {observer}"),
        ) {
            Ok(bodies) => bodies,
            // Out of calls between its pages: the next run reads it
            // first, with all of a run's calls. One that had them all
            // and still ran out waits for the next hour.
            Err(Missed::Budget) => {
                out_of_calls = true;
                if had == ESI_BUDGET - NAME_RESERVE {
                    log::warn(format!(
                        "observer {observer}: its ledger has more pages than one run may read"
                    ));
                    ledger_tried(observer)?;
                    tried += 1;
                }
                break;
            }
            Err(Missed::Esi(_)) => {
                ledger_tried(observer)?;
                tried += 1;
                refused += 1;
                if refused >= LEDGER_REFUSALS {
                    log::info("ledgers: too many refused this run; the rest wait for the next");
                    break;
                }
                continue;
            }
        };
        let rows = concat(&bodies);
        let saved = storage::transaction(&[
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
        ]);
        // One ledger that can't be stored (larger than a statement may
        // carry, say) doesn't stop the others.
        if let Err(err) = saved {
            log::warn(format!(
                "observer {observer}: its ledger couldn't be stored: {err:?}"
            ));
            ledger_tried(observer)?;
            tried += 1;
            continue;
        }
        stored += 1;
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
    budget.0 += NAME_RESERVE;
    learn_names(&mut budget, &people)?;
    // Carry on only while runs get somewhere: the hourly run's follow-up
    // has all its calls; a follow-up must have tried a list, or read or
    // marked a ledger. Queued last: a follow-up that failed after queuing
    // its own would end as replaced, its error lost.
    let out_of_calls = out_of_calls || !lists.unlisted.is_empty();
    if out_of_calls && (more.is_none() || lists.tried + stored + tried > 0) {
        if lists.unlisted.is_empty() {
            log::info("ledgers: out of ESI calls; the rest in a minute");
        } else {
            log::info(format!(
                "{} corporations' mining observers wait for the next run, a minute on: out of \
                 ESI calls",
                lists.unlisted.len()
            ));
        }
        jobs::enqueue(
            NewJob::new(LEDGER_MORE)
                .key(LEDGER_MORE)
                .payload(
                    serde_json::json!({ "corporations": listed, "unlisted": lists.unlisted })
                        .to_string(),
                )
                .at(rfc3339(Utc::now() + Duration::minutes(1))),
        )
        .map_err(|e| retry("queuing the next ledger run", e))?;
    }
    Ok(())
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
    let mut sent = discord::send_embed(&channel, &card, Mention::State("Member".into()));
    // No Discord role mapped to Member: posted without the mention.
    if matches!(sent, Err(discord::Error::NotAllowed(_))) {
        sent = discord::send_embed(&channel, &card, Mention::None);
    }
    match sent {
        Ok(()) => Ok(()),
        // Refused for good (its channel isn't this app's any more: an admin
        // took it back on the Discord page): said in the log, once per
        // pop, and the job is done rather than dead.
        Err(discord::Error::NotAllowed(why) | discord::Error::Invalid(why)) => {
            log::warn(format!(
                "a pop wasn't posted: {why}; pick a channel in Settings, under Pops on Discord"
            ));
            Ok(())
        }
        Err(err) => {
            // Not sent: release the claim for the retry.
            let _ = storage::execute(
                "UPDATE extractions SET pinged = false WHERE structure_id = $1 AND chunk_arrival = $2",
                &[
                    payload.structure_id.into(),
                    Db::timestamp(payload.chunk_arrival),
                ],
            );
            Err(retry("sending the ping", err))
        }
    }
}

// ---- pages -----------------------------------------------------------------

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
    /// Cancelled, or auto-fractured more than the stale hours ago (AA's
    /// Past).
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
            "(e.cancelled_at IS NOT NULL OR e.natural_decay < $1) ORDER BY e.natural_decay DESC LIMIT 200"
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
    // aa-moonmining's Upcoming: not cancelled, and auto-fractured less
    // than the stale hours ago (so a chunk in space is still there);
    // Past: the rest.
    let stale = now - settings.stale;
    let upcoming = pops(stale, now + Duration::days(60))?;
    let past = extractions(Which::Past, &[Db::timestamp(rfc3339(stale))])?;
    let coming: Vec<&Pop> = upcoming.iter().filter(|p| p.decay > now).collect();
    let ready = coming.iter().filter(|p| p.arrival <= now).count();
    let coming_value: f64 = coming.iter().filter_map(|p| p.value()).sum();
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
        .empty("No extractions running. Has a data source been added?"),
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
            i64::try_from(coming.len() - ready).unwrap_or(i64::MAX),
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
            "From the corporations' mining observers, refreshed hourly; values at CCP's average \
             ore prices",
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
         WHERE s.corporation_id = $1 AND s.drill IS NOT FALSE AND s.gone_at IS NULL ORDER BY s.name",
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
         FROM (SELECT DISTINCT corporation_id FROM structures WHERE drill IS NOT FALSE AND gone_at IS NULL) c \
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
    // One form, saved at once from Tether's save bar (DESIGN.md, Save bar).
    Ok(Page::new("Moon Mining settings").settings(
        SettingsForm::new("settings")
            .group(
                SettingsGroup::new("Members-only window")
                    .description("Not in aa-moonmining: Members see popped moons first, Blue later.")
                .field(Field::number("fresh_hours", "Members-only hours after a pop")
                    .range(Some(0.0), Some(48.0), true)
                    .value(settings.fresh.num_hours().to_string())
                    .help(
                        "Not in aa-moonmining: 0 is off, as there. On, those with basic_access \
                         alone (Blue) see popped moons after this many hours, on an old-moon list.",
                    )
                    .required(),)
                .field(Field::number("old_moons_shown", "Old moons shown beside fresh ones")
                    .range(Some(0.0), Some(50.0), true)
                    .value(settings.old_shown.to_string())
                    .help("Not in aa-moonmining: the newest moons popped in the last two days, beside the fresh ones on Extractions and on Blue's old-moon list (without when they popped), while the Members-only window is on. 0 shows none")
                    .required(),)
            )
            .group(
                // A channel no longer assigned to the app starts on "No
                // pings" (a select can't start on a value it doesn't list),
                // and the group says why nothing is posted.
                if settings
                    .channel
                    .as_ref()
                    .is_some_and(|c| !channels.iter().any(|(id, _)| id == c))
                {
                    SettingsGroup::new("Pops on Discord").description(
                        "The channel pops went to isn't this app's any more, so none are \
                         posted: pick one.",
                    )
                } else {
                    SettingsGroup::new("Pops on Discord")
                }
                .field(Field::select("ping_channel", "Ping pops to", channels.clone())
                    .value(
                        settings
                            .channel
                            .filter(|c| channels.iter().any(|(id, _)| id == c))
                            .unwrap_or_default(),
                    )
                    .help("Not in aa-moonmining: no channel, no pings."),)
                .field(Field::checkbox(
                "pings",
                "Ping Members at each pop",
                settings.pings,
            ))
            )
            .group(
                SettingsGroup::new("Moon value")
                    .description("What a month of mining a moon is worth.")
                .field(Field::number("volume_per_day", "Ore a drill pulls a day (m³)")
                    .range(Some(1.0), Some(10_000_000.0), true)
                    .value(format!("{:.0}", settings.rates.per_day))
                    .help("Default: 960,400, unless CCP changes it.")
                    .required(),)
                .field(Field::number("days_per_month", "Days in a month")
                    .range(Some(28.0), Some(31.0), false)
                    .value(settings.rates.days_per_month.to_string())
                    .help("Moons' monthly value uses this and the ore a day. Default: 30.4.")
                    .required(),)
            )
            .group(SettingsGroup::new("Extractions")
                .field(Field::number("stale_hours", "Hours after auto-fracture until an extraction is Past")
                    .range(Some(1.0), Some(168.0), true)
                    .value(settings.stale.num_hours().to_string())
                    .help("Default: 12.")
                    .required(),))
            .group(SettingsGroup::new("Admin notices")
                .field(Field::checkbox(
                    "admin_notifications",
                    "Tell admins when an owner is added or can't be read",
                    settings.admin_notices,
                )
                .help(
                    "As aa-moonmining's admin notifications (on there): a notice in Tether's \
                     notifications for superusers and whoever holds Moon Mining's manage \
                     permission.",
                ))),
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
    let notices = submission.checked("admin_notifications");
    storage::execute(
        "UPDATE settings SET fresh_hours = $1, ping_channel = $2, pings = $3, volume_per_day = $4, \
         days_per_month = $5, stale_hours = $6, old_moons_shown = $7, admin_notifications = $8 \
         WHERE id = 1",
        &[
            hours.into(),
            (!channel.is_empty()).then(|| channel.to_owned()).into(),
            submission.checked("pings").into(),
            per_day.into(),
            days.into(),
            stale.into(),
            old_shown.into(),
            notices.into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    // Admins see who changed what in the plugin's log.
    log::info(format!(
        "settings changed by {} ({}): members-only {hours}h, channel {channel:?}, pings {}, \
         {per_day} m³ a day, {days} days a month, past after {stale}h, {old_shown} old moons \
         shown, admin notices {notices}",
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
