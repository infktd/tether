//! Structures (aa-structures; PRD F23).
//!
//! - Owners are corporations, added through a data source (AA's "Add
//!   Structure Owner"): a character with the in-game Station Manager role,
//!   offered by its owner and approved by an admin.
//! - The structure list: name, type, system and region, fuel and time
//!   left, services, state and its timer, reinforce hour; filtered by
//!   owner, low fuel and reinforced, and seen by permission (the viewer's
//!   corporation, alliance, or all).
//! - Owners' structure notifications (attacks, reinforcements, fuel,
//!   services, power, anchoring, moon drills) go to the Discord channels a
//!   manager picks, once each; Tether adds its own low-fuel alerts at
//!   chosen thresholds.
//! - Timers from notifications and structures' states are listed here and
//!   published for Structure Timers after every sync (friendly, and
//!   corporation-only if a manager says so, as aa-structures'
//!   STRUCTURES_TIMERS_ARE_CORP_RESTRICTED).
//! - ESI is read gently: notifications at most every 10 minutes and
//!   structures every hour (their cache times), and an owner ESI answers
//!   403 for (a lost role) is left alone for an hour, doubling to a day.

mod notification;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission, SubmitResult,
    Table, Tone, Value, badge, link, log, time,
};

use crate::notification::{Category, Context, Fields};

/// Notifications older than this aren't sent (aa-structures' default).
const RELAY_WITHIN: Duration = Duration::hours(24);
/// How often ESI may be asked, a little under the cache times so a
/// 10-minute schedule doesn't slip a whole run.
const NOTIFICATIONS_EVERY: &str = "9 minutes";
const STRUCTURES_EVERY: &str = "55 minutes";
/// The host allows 100 ESI calls per run; stop short.
const ESI_BUDGET: usize = 90;
/// Discord messages per call (the host's limit), and the gap between
/// relay runs so 20 a minute isn't passed.
const SENDS_PER_RUN: usize = 5;
const RELAY_GAP: Duration = Duration::seconds(15);
/// Rows per table on a page (the host caps values per page).
const LIST_ROWS: i64 = 400;
const SHORT_ROWS: i64 = 100;
/// Low-fuel thresholds: hours, at most this many.
const MAX_THRESHOLDS: usize = 5;
const MAX_THRESHOLD_HOURS: i64 = 2160;
/// Timers published for Structure Timers (the host's limit).
const MAX_PUBLISHED: i64 = 500;
/// The one-off job that publishes timers at once (after a settings change).
const PUBLISH_JOB: &str = "publish_timers";

struct Structures;

impl Plugin for Structures {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if path.is_empty() {
            return list_page(&viewer, None);
        }
        if let Some(corp) = path.strip_prefix("owner/") {
            let corp: i64 = corp.parse().map_err(|_| PageError::NotFound)?;
            return list_page(&viewer, Some(corp));
        }
        match path {
            "settings" => settings_page(None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("settings", "settings") => save_settings(&viewer, &submission),
            ("settings", "retry") => retry_owner(&viewer, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(),
            "relay" => relay(),
            PUBLISH_JOB => publish_timers(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Structures);

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

fn with_rows(mut table: Table, rows: impl IntoIterator<Item = Vec<Value>>) -> Table {
    for row in rows {
        table = table.row(row);
    }
    table
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
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

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Ids as a comma list, for `string_to_array($n, ',')::bigint[]`.
fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// "3d 4h", "5h 12m".
fn left(d: Duration) -> String {
    if d <= Duration::zero() {
        return "none".to_owned();
    }
    let (days, hours, minutes) = (d.num_days(), d.num_hours() % 24, d.num_minutes() % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h {minutes}m")
    }
}

struct Settings {
    attack: Option<String>,
    fuel: Option<String>,
    state: Option<String>,
    moon: Option<String>,
    /// Largest first.
    thresholds: Vec<i64>,
    mention: bool,
    /// Published timers are seen only by the owning corporation.
    timers_corporation_only: bool,
}

impl Settings {
    fn channel(&self, category: Category) -> Option<&str> {
        match category {
            Category::Attack => self.attack.as_deref(),
            Category::Fuel => self.fuel.as_deref(),
            Category::State => self.state.as_deref(),
            Category::Moon => self.moon.as_deref(),
        }
    }
}

/// "72, 24,6" into [72, 24, 6]: whole hours, 1 to 2160, at most 5.
fn parse_thresholds(text: &str) -> Option<Vec<i64>> {
    let mut hours = Vec::new();
    for part in text.split(',') {
        let h: i64 = part.trim().parse().ok()?;
        if !(1..=MAX_THRESHOLD_HOURS).contains(&h) {
            return None;
        }
        hours.push(h);
    }
    hours.sort_unstable_by(|a, b| b.cmp(a));
    hours.dedup();
    (!hours.is_empty() && hours.len() <= MAX_THRESHOLDS).then_some(hours)
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT attack_channel, fuel_channel, state_channel, moon_channel, fuel_thresholds, mention_members, \
                timers_corporation_only \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first();
    let channel = |i: usize| {
        row.and_then(|r| r.get(i))
            .and_then(Db::as_text)
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
    };
    Ok(Settings {
        attack: channel(0),
        fuel: channel(1),
        state: channel(2),
        moon: channel(3),
        thresholds: row
            .and_then(|r| r.get(4))
            .and_then(Db::as_text)
            .and_then(parse_thresholds)
            .unwrap_or_else(|| vec![72, 24, 6]),
        mention: row
            .and_then(|r| r.get(5))
            .and_then(Db::as_bool)
            .unwrap_or(false),
        timers_corporation_only: row
            .and_then(|r| r.get(6))
            .and_then(Db::as_bool)
            .unwrap_or(false),
    })
}

// ---- jobs ------------------------------------------------------------------

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

/// What ESI said, for an owner's record.
enum Outcome {
    Ok(Vec<String>),
    /// Leave this owner alone for a while (a lost role or token).
    BackOff(String),
    /// Try again next run.
    Later(String),
}

fn call(
    budget: &mut Budget,
    endpoint: &str,
    subject: Subject,
    params: &[(String, String)],
    paged: bool,
) -> Outcome {
    let describe = |err: esi::Error| {
        match err {
        esi::Error::Status(403) => Outcome::BackOff(
            "ESI said 403: the character has lost the in-game role (Station Manager) or left the corporation"
                .to_owned(),
        ),
        esi::Error::Token => Outcome::BackOff(
            "the character's token is gone: its owner must log in with it again".to_owned(),
        ),
        esi::Error::NotADataSource => {
            Outcome::BackOff("no longer an approved data source".to_owned())
        }
        other => Outcome::Later(format!("{other:?}")),
    }
    };
    if !budget.take() {
        return Outcome::Later("out of ESI calls this run".to_owned());
    }
    let first = match esi::get(endpoint, subject, params, paged.then_some(1)) {
        Ok(first) => first,
        Err(err) => return describe(err),
    };
    let mut bodies = vec![first.body];
    if paged {
        for page in 2..=first.pages {
            if !budget.take() {
                return Outcome::Later("out of ESI calls this run".to_owned());
            }
            match esi::get(endpoint, subject, params, Some(page)) {
                Ok(response) => bodies.push(response.body),
                Err(err) => return describe(err),
            }
        }
    }
    Outcome::Ok(bodies)
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

/// Which read of an owner.
#[derive(Clone, Copy)]
enum Read {
    Structures,
    Notifications,
}

impl Read {
    /// Column prefix in `owners` (fixed names, never data).
    fn column(self) -> &'static str {
        match self {
            Read::Structures => "structures",
            Read::Notifications => "notifications",
        }
    }

    fn every(self) -> &'static str {
        match self {
            Read::Structures => STRUCTURES_EVERY,
            Read::Notifications => NOTIFICATIONS_EVERY,
        }
    }
}

/// The owner character to read a corporation with now, if its last read
/// is old enough (the ESI cache) and one isn't backing off.
fn pick_owner(corp: i64, read: Read) -> Result<Option<i64>, JobError> {
    let c = read.column();
    let rows = storage::query(
        &format!(
            "SELECT character_id FROM owners o \
             WHERE corporation_id = $1 AND ({c}_retry_at IS NULL OR {c}_retry_at <= now()) \
               AND NOT EXISTS (SELECT 1 FROM owners f WHERE f.corporation_id = $1 \
                   AND f.{c}_at > now() - interval '{}') \
             ORDER BY {c}_failures, character_id LIMIT 1",
            read.every()
        ),
        &[corp.into()],
    )
    .map_err(|e| retry("picking an owner", e))?;
    Ok(rows.rows.first().map(|r| int(r, 0)))
}

fn record(owner: i64, read: Read, outcome: &Outcome) -> Result<(), JobError> {
    let c = read.column();
    let (sql, params): (String, Vec<Db>) = match outcome {
        Outcome::Ok(_) => (
            format!(
                "UPDATE owners SET {c}_at = now(), {c}_failures = 0, {c}_retry_at = NULL, \
                 last_error = NULL WHERE character_id = $1"
            ),
            vec![owner.into()],
        ),
        Outcome::BackOff(why) => {
            log::warn(format!("{c} for owner {owner}: {why}; backing off"));
            (
                format!(
                    "UPDATE owners SET {c}_failures = {c}_failures + 1, \
                     {c}_retry_at = now() + least(interval '1 hour' * power(2, least({c}_failures, 5)), interval '24 hours'), \
                     last_error = $2 WHERE character_id = $1"
                ),
                vec![owner.into(), why.as_str().into()],
            )
        }
        Outcome::Later(why) => {
            log::warn(format!("{c} for owner {owner}: {why}"));
            (
                "UPDATE owners SET last_error = $2 WHERE character_id = $1".to_owned(),
                vec![owner.into(), why.as_str().into()],
            )
        }
    };
    storage::execute(&sql, &params).map_err(|e| retry("recording a read", e))?;
    Ok(())
}

/// Every 10 minutes: owners, then each corporation's notifications and
/// (hourly) structures, names, messages, timers and low-fuel alerts. The
/// timers are published for Structure Timers whatever happened before (a
/// step failing, or owners gone), so what it shows follows at once.
fn sync() -> Result<(), JobError> {
    let synced = sync_steps();
    let published = publish_timers();
    synced?;
    published?;
    queue_relay(None)
}

fn sync_steps() -> Result<(), JobError> {
    let corporations = sync_owners()?;
    if corporations.is_empty() {
        log::info("no structure owners yet: approve a data source");
        return Ok(());
    }
    let mut budget = Budget(ESI_BUDGET);
    for &corp in &corporations {
        if let Some(owner) = pick_owner(corp, Read::Notifications)? {
            let outcome = call(
                &mut budget,
                "corporation-structure-notifications",
                Subject::DataSource(owner),
                &[],
                false,
            );
            if let Outcome::Ok(bodies) = &outcome {
                store_notifications(corp, bodies)?;
            }
            record(owner, Read::Notifications, &outcome)?;
        }
        if let Some(owner) = pick_owner(corp, Read::Structures)? {
            let outcome = call(
                &mut budget,
                "corporation-structures",
                Subject::DataSource(owner),
                &[],
                true,
            );
            if let Outcome::Ok(bodies) = &outcome {
                store_structures(corp, bodies)?;
            }
            record(owner, Read::Structures, &outcome)?;
        }
    }
    learn_systems(&mut budget)?;
    learn_names(&mut budget)?;
    handle_notifications()?;
    fuel_alerts()?;
    storage::execute(
        "DELETE FROM timers WHERE at < now() - interval '7 days'",
        &[],
    )
    .map_err(|e| retry("expiring timers", e))?;
    storage::execute(
        "DELETE FROM notifications WHERE at < now() - interval '60 days'",
        &[],
    )
    .map_err(|e| retry("expiring notifications", e))?;
    storage::execute(
        "DELETE FROM structure_owners WHERE seen_at < now() - interval '30 days'",
        &[],
    )
    .map_err(|e| retry("expiring structure owners", e))?;
    storage::execute(
        "DELETE FROM outbox WHERE created_at < now() - interval '30 days'",
        &[],
    )
    .map_err(|e| retry("expiring sent messages", e))?;
    Ok(())
}

/// The owners table follows the host's approved data sources; a
/// corporation with no owner left loses its structures (its timers expire
/// on their own). An empty list while owners are known is taken as a
/// hiccup for an hour before anything is removed.
fn sync_owners() -> Result<Vec<i64>, JobError> {
    let sources = esi::data_sources();
    if sources.is_empty() {
        let rows = storage::query(
            "UPDATE settings SET sources_missing_since = coalesce(sources_missing_since, now()) \
             WHERE id = 1 AND EXISTS (SELECT 1 FROM owners) \
             RETURNING sources_missing_since > now() - interval '1 hour'",
            &[],
        )
        .map_err(|e| retry("checking owners", e))?;
        if rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(Db::as_bool)
            .unwrap_or(false)
        {
            log::warn(
                "the host lists no data sources, but owners are known: keeping them for now \
                 (they go if none is approved for an hour)",
            );
            return Ok(Vec::new());
        }
    }
    let rows: Vec<serde_json::Value> = sources
        .iter()
        .map(|s| {
            serde_json::json!({
                "character_id": s.id,
                "character_name": s.name,
                "corporation_id": s.corporation_id,
                "alliance_id": s.alliance_id,
            })
        })
        .collect();
    let ids: Vec<i64> = sources.iter().map(|s| s.id).collect();
    storage::transaction(&[
        Statement::new(
            "INSERT INTO owners (character_id, character_name, corporation_id, alliance_id) \
             SELECT character_id, character_name, corporation_id, alliance_id \
             FROM json_to_recordset($1::json) AS x(character_id bigint, character_name text, \
                  corporation_id bigint, alliance_id bigint) \
             ON CONFLICT (character_id) DO UPDATE SET character_name = EXCLUDED.character_name, \
             corporation_id = EXCLUDED.corporation_id, alliance_id = EXCLUDED.alliance_id",
            vec![Db::json(serde_json::Value::Array(rows).to_string())],
        ),
        Statement::new(
            "DELETE FROM owners WHERE NOT (character_id = ANY(string_to_array($1, ',')::bigint[]))",
            vec![id_list(&ids).into()],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id NOT IN (SELECT corporation_id FROM owners)",
            vec![],
        ),
        Statement::new(
            "UPDATE settings SET sources_missing_since = NULL WHERE id = 1 AND $1 <> ''",
            vec![id_list(&ids).into()],
        ),
    ])
    .map_err(|e| retry("storing owners", e))?;
    let mut corporations: Vec<i64> = sources.iter().map(|s| s.corporation_id).collect();
    corporations.sort_unstable();
    corporations.dedup();
    Ok(corporations)
}

#[derive(Deserialize)]
struct Notification {
    notification_id: i64,
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    #[serde(default)]
    text: Option<String>,
}

/// New notifications, stored once by id (and once per event, whichever
/// owner character saw it).
fn store_notifications(corp: i64, bodies: &[String]) -> Result<(), JobError> {
    let mut rows = Vec::new();
    for body in bodies {
        let Ok(items) = serde_json::from_str::<Vec<Notification>>(body) else {
            log::warn(format!(
                "notifications for corporation {corp}: unexpected answer"
            ));
            continue;
        };
        for n in items {
            if notification::category(&n.kind).is_none() {
                continue;
            }
            let Some(at) = parse_time(&n.timestamp) else {
                continue;
            };
            let text = n.text.unwrap_or_default();
            let fields = Fields::parse(&text);
            let about = fields.structure_id().or_else(|| fields.int("moonID"));
            rows.push(serde_json::json!({
                "notification_id": n.notification_id,
                "type": n.kind,
                "at": rfc3339(at),
                "structure_id": fields.structure_id(),
                "event_key": format!("{}:{}:{}", n.kind, about.unwrap_or_default(), at.timestamp()),
                "text": text,
            }));
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    storage::execute(
        "INSERT INTO notifications (notification_id, corporation_id, type, at, structure_id, event_key, text) \
         SELECT notification_id, $2, type, at, structure_id, event_key, text \
         FROM json_to_recordset($1::json) AS x(notification_id bigint, type text, at timestamptz, \
              structure_id bigint, event_key text, text text) \
         ON CONFLICT DO NOTHING",
        &[
            Db::json(serde_json::Value::Array(rows).to_string()),
            corp.into(),
        ],
    )
    .map_err(|e| retry("storing notifications", e))?;
    Ok(())
}

/// The corporation's structures, replaced whole (gone ones were
/// destroyed or unanchored), and timers from their states.
fn store_structures(corp: i64, bodies: &[String]) -> Result<(), JobError> {
    storage::transaction(&[
        Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, name, type_id, system_id, fuel_expires, \
                 state, state_timer_start, state_timer_end, unanchors_at, reinforce_hour, \
                 next_reinforce_hour, next_reinforce_apply, services, updated_at) \
             SELECT structure_id, $2, coalesce(name, 'Structure ' || structure_id::text), type_id, system_id, \
                 fuel_expires, coalesce(state, 'unknown'), state_timer_start, state_timer_end, unanchors_at, \
                 reinforce_hour, next_reinforce_hour, next_reinforce_apply, coalesce(services, '[]'::jsonb), now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, name text, type_id bigint, \
                 system_id bigint, fuel_expires timestamptz, state text, state_timer_start timestamptz, \
                 state_timer_end timestamptz, unanchors_at timestamptz, reinforce_hour integer, \
                 next_reinforce_hour integer, next_reinforce_apply timestamptz, services jsonb) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                 name = EXCLUDED.name, type_id = EXCLUDED.type_id, system_id = EXCLUDED.system_id, \
                 fuel_expires = EXCLUDED.fuel_expires, state = EXCLUDED.state, \
                 state_timer_start = EXCLUDED.state_timer_start, state_timer_end = EXCLUDED.state_timer_end, \
                 unanchors_at = EXCLUDED.unanchors_at, reinforce_hour = EXCLUDED.reinforce_hour, \
                 next_reinforce_hour = EXCLUDED.next_reinforce_hour, \
                 next_reinforce_apply = EXCLUDED.next_reinforce_apply, services = EXCLUDED.services, \
                 updated_at = now()",
            vec![Db::json(concat(bodies)), corp.into()],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id = $1 AND updated_at < now()",
            vec![corp.into()],
        ),
        // Which corporation each structure was seen in, kept a while after
        // it's gone: notifications are relayed only for these.
        Statement::new(
            "INSERT INTO structure_owners (structure_id, corporation_id, seen_at) \
             SELECT structure_id, corporation_id, now() FROM structures WHERE corporation_id = $1 \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, seen_at = now()",
            vec![corp.into()],
        ),
        // Timers the states show: reinforced until, anchoring until, and
        // unanchoring, unless a notification gave the same one.
        Statement::new(
            "INSERT INTO timers (structure_id, kind, at, corporation_id) \
             SELECT s.structure_id, k.kind, k.at, s.corporation_id FROM structures s \
             CROSS JOIN LATERAL (VALUES \
                 (CASE s.state WHEN 'armor_reinforce' THEN 'Armor' WHEN 'hull_reinforce' THEN 'Hull' \
                      WHEN 'anchoring' THEN 'Anchoring' END, s.state_timer_end), \
                 ('Unanchoring', s.unanchors_at)) AS k(kind, at) \
             WHERE s.corporation_id = $1 AND k.kind IS NOT NULL AND k.at > now() \
               AND NOT EXISTS (SELECT 1 FROM timers t WHERE t.structure_id = s.structure_id \
                   AND t.kind = k.kind AND abs(extract(epoch FROM t.at - k.at)) < 300) \
             ON CONFLICT DO NOTHING",
            vec![corp.into()],
        ),
    ])
    .map_err(|e| retry("storing structures", e))?;
    Ok(())
}

#[derive(Deserialize)]
struct System {
    system_id: i64,
    name: String,
    security_status: f64,
    region_id: i64,
}

/// Security and region for systems with structures (once each).
fn learn_systems(budget: &mut Budget) -> Result<(), JobError> {
    let missing = storage::query(
        "SELECT DISTINCT s.system_id, s.corporation_id FROM structures s \
         WHERE NOT EXISTS (SELECT 1 FROM systems y WHERE y.system_id = s.system_id) LIMIT 30",
        &[],
    )
    .map_err(|e| retry("finding systems", e))?;
    for row in &missing.rows {
        let (system, corp) = (int(row, 0), int(row, 1));
        let owner = storage::query(
            "SELECT character_id FROM owners WHERE corporation_id = $1 ORDER BY structures_failures, character_id LIMIT 1",
            &[corp.into()],
        )
        .map_err(|e| retry("picking an owner", e))?;
        let Some(owner) = owner.rows.first().map(|r| int(r, 0)) else {
            continue;
        };
        // Two ESI requests behind one call (the host counts one).
        if !(budget.take() && budget.take()) {
            break;
        }
        match esi::get(
            "universe-system",
            Subject::DataSource(owner),
            &[("system_id".to_owned(), system.to_string())],
            None,
        ) {
            Ok(response) => {
                let Ok(s) = serde_json::from_str::<System>(&response.body) else {
                    log::warn(format!("system {system}: unexpected answer"));
                    continue;
                };
                storage::execute(
                    "INSERT INTO systems (system_id, name, security_status, region_id) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (system_id) DO NOTHING",
                    &[
                        s.system_id.into(),
                        s.name.into(),
                        s.security_status.into(),
                        s.region_id.into(),
                    ],
                )
                .map_err(|e| retry("storing a system", e))?;
            }
            Err(err) => log::warn(format!("system {system}: {err:?}")),
        }
    }
    Ok(())
}

/// Names for everything shown or sent that has none yet.
fn learn_names(budget: &mut Budget) -> Result<(), JobError> {
    let known = storage::query(
        "SELECT DISTINCT id FROM ( \
             SELECT type_id AS id FROM structures UNION SELECT system_id FROM structures \
             UNION SELECT region_id FROM systems UNION SELECT corporation_id FROM owners \
             UNION SELECT alliance_id FROM owners WHERE alliance_id IS NOT NULL) i \
         WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = i.id)",
        &[],
    )
    .map_err(|e| retry("finding names", e))?;
    let mut ids: Vec<i64> = known.rows.iter().map(|r| int(r, 0)).collect();
    let pending = storage::query(
        "SELECT text FROM notifications WHERE NOT handled ORDER BY at LIMIT 500",
        &[],
    )
    .map_err(|e| retry("reading notifications", e))?;
    for row in &pending.rows {
        ids.extend(Fields::parse(&text(row, 0)).ids());
    }
    ids.retain(|id| *id > 0);
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let have = storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[id_list(&ids).into()],
    )
    .map_err(|e| retry("reading names", e))?;
    let have: Vec<i64> = have.rows.iter().map(|r| int(r, 0)).collect();
    let missing: Vec<i64> = ids.into_iter().filter(|id| !have.contains(id)).collect();
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
        storage::execute(
            "INSERT INTO names (id, name, category) \
             SELECT id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            &[Db::json(serde_json::Value::Array(rows).to_string())],
        )
        .map_err(|e| retry("storing names", e))?;
    }
    Ok(())
}

/// Names by id, for the ids given.
fn names_for(ids: &[i64]) -> Result<Vec<(i64, String)>, storage::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = storage::query(
        "SELECT id, name FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[id_list(ids).into()],
    )?;
    Ok(rows.rows.iter().map(|r| (int(r, 0), text(r, 1))).collect())
}

/// New notifications into messages (recent ones, for a category with a
/// channel) and timers; each once.
fn handle_notifications() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let now = Utc::now();
    let rows = storage::query(
        "SELECT n.notification_id, n.type, n.at, n.text, n.structure_id, n.corporation_id, s.name, \
                so.structure_id IS NOT NULL \
         FROM notifications n \
         LEFT JOIN structure_owners so ON so.structure_id = n.structure_id AND so.corporation_id = n.corporation_id \
         LEFT JOIN structures s ON s.structure_id = n.structure_id AND s.corporation_id = n.corporation_id \
         WHERE NOT n.handled AND (so.structure_id IS NOT NULL OR n.at < now() - interval '2 hours') \
         ORDER BY n.at LIMIT 200",
        &[],
    )
    .map_err(|e| retry("reading notifications", e))?;
    let mut ids = Vec::new();
    let parsed: Vec<Fields> = rows
        .rows
        .iter()
        .map(|r| {
            let fields = Fields::parse(&text(r, 3));
            ids.extend(fields.ids());
            fields
        })
        .collect();
    let names = names_for(&ids).map_err(|e| retry("reading names", e))?;
    let lookup = |id: i64| names.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone());
    for (row, fields) in rows.rows.iter().zip(&parsed) {
        let (id, kind) = (int(row, 0), text(row, 1));
        let Some(at) = when(row, 2) else {
            continue;
        };
        let corp = int(row, 5);
        // Only structures seen in the corporation the owner read them for
        // (not one it left). One not seen yet waits two hours for the
        // structure list, then is dropped.
        let ours = row.get(7).and_then(Db::as_bool).unwrap_or(false);
        let mut statements = vec![Statement::new(
            "UPDATE notifications SET handled = true WHERE notification_id = $1",
            vec![id.into()],
        )];
        if let (Some(timer), Some(structure)) =
            (notification::timer(&kind, fields, at), opt_int(row, 4))
            && ours
            && timer.at > now
        {
            statements.push(Statement::new(
                "INSERT INTO timers (structure_id, kind, at, corporation_id) \
                 SELECT $1, $2, $3, $4 WHERE NOT EXISTS (SELECT 1 FROM timers t \
                     WHERE t.structure_id = $1 AND t.kind = $2 AND abs(extract(epoch FROM t.at - $3::timestamptz)) < 300) \
                 ON CONFLICT DO NOTHING",
                vec![
                    structure.into(),
                    timer.kind.into(),
                    Db::timestamp(rfc3339(timer.at)),
                    corp.into(),
                ],
            ));
        }
        let category = notification::category(&kind);
        let channel = category.and_then(|c| settings.channel(c));
        if let (Some(category), Some(channel)) = (category, channel)
            && ours
            && now - at <= RELAY_WITHIN
        {
            let cx = Context {
                structure: row.get(6).and_then(Db::as_text).map(str::to_owned),
                name: &lookup,
            };
            if let Some(message) = notification::message(&kind, fields, at, &cx) {
                statements.push(Statement::new(
                    "INSERT INTO outbox (key, channel, message, mention) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (key) DO NOTHING",
                    vec![
                        format!("notification:{id}").into(),
                        channel.into(),
                        message.into(),
                        (settings.mention && category == Category::Attack).into(),
                    ],
                ));
            }
        }
        storage::transaction(&statements).map_err(|e| retry("handling a notification", e))?;
    }
    Ok(())
}

/// Tether's low-fuel alerts: one per structure as its fuel falls under
/// each threshold, again after it's refuelled above it.
fn fuel_alerts() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    // Refuelled (or gone): the alerts reset.
    storage::execute(
        "DELETE FROM fuel_alerts f WHERE NOT EXISTS (SELECT 1 FROM structures s \
             WHERE s.structure_id = f.structure_id AND s.fuel_expires IS NOT NULL \
               AND s.fuel_expires <= now() + make_interval(hours => f.hours))",
        &[],
    )
    .map_err(|e| retry("resetting fuel alerts", e))?;
    let Some(channel) = settings.fuel.clone() else {
        return Ok(());
    };
    let now = Utc::now();
    // Largest first: a structure under several thresholds at once gets
    // one alert, for the smallest.
    let mut alerts: Vec<(i64, i64)> = Vec::new();
    for hours in &settings.thresholds {
        let crossed = storage::query(
            "INSERT INTO fuel_alerts (structure_id, hours) \
             SELECT structure_id, $1 FROM structures \
             WHERE fuel_expires IS NOT NULL AND fuel_expires > now() \
               AND fuel_expires <= now() + make_interval(hours => $1::integer) \
             ON CONFLICT DO NOTHING RETURNING structure_id",
            &[(*hours).into()],
        )
        .map_err(|e| retry("recording fuel alerts", e))?;
        for row in &crossed.rows {
            let structure = int(row, 0);
            alerts.retain(|(s, _)| *s != structure);
            alerts.push((structure, *hours));
        }
    }
    for (structure, hours) in alerts {
        let rows = storage::query(
            "SELECT s.name, coalesce(t.name, ''), coalesce(y.name, n.name, ''), s.fuel_expires \
             FROM structures s LEFT JOIN names t ON t.id = s.type_id \
             LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names n ON n.id = s.system_id \
             WHERE s.structure_id = $1",
            &[structure.into()],
        )
        .map_err(|e| retry("reading a structure", e))?;
        let Some(row) = rows.rows.first() else {
            continue;
        };
        let Some(expires) = when(row, 3) else {
            continue;
        };
        let (name, type_name, system) = (text(row, 0), text(row, 1), text(row, 2));
        let mut place = notification::escape(&name);
        if !type_name.is_empty() {
            place.push_str(&format!(" ({type_name})"));
        }
        if !system.is_empty() {
            place.push_str(&format!(" in {system}"));
        }
        let message = format!(
            "Low fuel: {place} runs out of fuel in {} ({} EVE), under the {hours}-hour alert.",
            left(expires - now),
            expires.format("%Y-%m-%d %H:%M")
        );
        storage::execute(
            "INSERT INTO outbox (key, channel, message) VALUES ($1, $2, $3) ON CONFLICT (key) DO NOTHING",
            &[
                format!("fuel:{structure}:{hours}:{}", expires.timestamp()).into(),
                channel.as_str().into(),
                message.into(),
            ],
        )
        .map_err(|e| retry("queuing a fuel alert", e))?;
    }
    Ok(())
}

/// A structure's name or a name ESI gave, made fit for a shared timer: one
/// line, at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Publishes the current timers for Structure Timers (aa-structures feeds
/// the timerboard): the latest of each kind per structure, up to a day
/// past, at most [`MAX_PUBLISHED`], soonest first. Each is friendly (our
/// structures), and corporation-only when the setting says so. Trouble
/// reading or publishing is retried.
fn publish_timers() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("publishing timers: reading settings", e))?;
    let rows = storage::query(
        &format!(
            "SELECT * FROM ( \
                 SELECT DISTINCT ON (t.structure_id, t.kind) t.structure_id, t.kind, t.at, s.name, \
                     coalesce(tn.name, '') AS type_name, coalesce(y.name, sn.name, '') AS system, \
                     coalesce(o.name, '') AS owner, s.corporation_id \
                 FROM timers t JOIN structures s ON s.structure_id = t.structure_id \
                 LEFT JOIN names tn ON tn.id = s.type_id \
                 LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
                 LEFT JOIN names o ON o.id = s.corporation_id \
                 WHERE t.at > now() - interval '1 day' \
                 ORDER BY t.structure_id, t.kind, t.at DESC) latest \
             ORDER BY 3, 1, 2 LIMIT {MAX_PUBLISHED}"
        ),
        &[],
    )
    .map_err(|e| retry("publishing timers: reading them", e))?;
    let timers: Vec<tether_plugin_sdk::timers::Timer> = rows
        .rows
        .iter()
        .filter_map(|r| {
            let (structure, kind, at) = (int(r, 0), text(r, 1).to_lowercase(), when(r, 2)?);
            let name = one_line(&text(r, 3), 150);
            let (type_name, owner) = (one_line(&text(r, 4), 100), one_line(&text(r, 6), 100));
            let mut details = match (type_name.is_empty(), owner.is_empty()) {
                (false, false) => format!("{type_name} of {owner}"),
                (false, true) => type_name,
                (true, false) => format!("A structure of {owner}"),
                (true, true) => String::new(),
            };
            if !details.is_empty() {
                details.push_str(". ");
            }
            details.push_str("From the structure's state or its notifications.");
            Some(tether_plugin_sdk::timers::Timer {
                key: format!("{structure}:{kind}"),
                title: format!("{name}: {kind} timer"),
                at: rfc3339(at),
                system: one_line(&text(r, 5), 100),
                details,
                objective: "friendly".to_owned(),
                corporation_id: settings.timers_corporation_only.then(|| int(r, 7)),
            })
        })
        .collect();
    tether_plugin_sdk::timers::publish(&timers).map_err(|e| retry("publishing timers", e))
}

/// Queues the relay when messages are waiting.
fn queue_relay(at: Option<DateTime<Utc>>) -> Result<(), JobError> {
    let waiting = storage::query(
        "SELECT 1 FROM outbox WHERE sent_at IS NULL AND failed IS NULL LIMIT 1",
        &[],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    if waiting.rows.is_empty() {
        return Ok(());
    }
    let mut job = NewJob::new("relay").key("relay");
    if let Some(at) = at {
        job = job.at(rfc3339(at));
    }
    jobs::enqueue(job).map_err(|e| retry("queuing the relay", e))
}

/// Sends waiting messages, five a run (the host's limit), again in 15
/// seconds while more wait. Each is claimed before it's sent, so a retry
/// can't send it twice.
fn relay() -> Result<(), JobError> {
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND created_at < now() - interval '1 day'",
        &[],
    )
    .map_err(|e| retry("expiring messages", e))?;
    let waiting = storage::query(
        "SELECT id, channel, message, mention FROM outbox WHERE sent_at IS NULL AND failed IS NULL \
         ORDER BY id LIMIT $1",
        &[count(SENDS_PER_RUN).into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    let mut sends = 0;
    let mut later = Utc::now() + RELAY_GAP;
    for row in &waiting.rows {
        if sends >= SENDS_PER_RUN {
            break;
        }
        let id = int(row, 0);
        let claimed = storage::execute(
            "UPDATE outbox SET sent_at = now() WHERE id = $1 AND sent_at IS NULL AND failed IS NULL",
            &[id.into()],
        )
        .map_err(|e| retry("claiming a message", e))?;
        if claimed == 0 {
            continue;
        }
        let (channel, message) = (text(row, 1), text(row, 2));
        let mention = row.get(3).and_then(Db::as_bool).unwrap_or(false);
        sends += 1;
        let mut result = discord::send(
            &channel,
            &message,
            if mention {
                Mention::State("Member".into())
            } else {
                Mention::None
            },
        );
        // No role mapped to Member: send it without the mention.
        if mention && matches!(result, Err(discord::Error::NotAllowed(_))) && sends < SENDS_PER_RUN
        {
            sends += 1;
            result = discord::send(&channel, &message, Mention::None);
        }
        match result {
            Ok(()) => {}
            Err(discord::Error::NotAllowed(why) | discord::Error::Invalid(why)) => {
                log::warn(format!("a Discord message wasn't sent: {why}"));
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL, failed = $2 WHERE id = $1",
                    &[id.into(), why.into()],
                )
                .map_err(|e| retry("marking a message", e))?;
            }
            Err(err) => {
                // Rate limited or Discord down: release it for later.
                log::info(format!("Discord: {err:?}; trying again in a minute"));
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL WHERE id = $1",
                    &[id.into()],
                )
                .map_err(|e| retry("releasing a message", e))?;
                later = Utc::now() + Duration::minutes(1);
                break;
            }
        }
    }
    queue_relay(Some(later))
}

// ---- pages -----------------------------------------------------------------

/// Which structures the viewer may see, as the first three parameters of
/// a query using [`VISIBLE`].
fn visibility(viewer: &Viewer) -> Option<Vec<Db>> {
    let all = viewer.can("view_all_structures");
    let corp = viewer.can("view_corporation_structures");
    let alliance = viewer.can("view_alliance_structures");
    if !(all || corp || alliance) {
        return None;
    }
    Some(vec![
        all.into(),
        corp.then_some(viewer.main.corporation_id).into(),
        viewer.main.alliance_id.filter(|_| alliance).into(),
    ])
}

/// A structure `s` the viewer may see (parameters from [`visibility`]).
const VISIBLE: &str = "($1::boolean OR s.corporation_id = $2::bigint \
     OR s.corporation_id IN (SELECT corporation_id FROM owners WHERE alliance_id = $3::bigint))";

fn state_badge(state: &str) -> Value {
    let (label, tone) = match state {
        "shield_vulnerable" => ("Shield vulnerable", Tone::Success),
        "armor_reinforce" => ("Armor reinforced", Tone::Danger),
        "armor_vulnerable" => ("Armor vulnerable", Tone::Danger),
        "hull_reinforce" => ("Hull reinforced", Tone::Danger),
        "hull_vulnerable" => ("Hull vulnerable", Tone::Danger),
        "anchoring" => ("Anchoring", Tone::Warning),
        "anchor_vulnerable" => ("Anchor vulnerable", Tone::Warning),
        "deploy_vulnerable" => ("Deploying", Tone::Warning),
        "onlining_vulnerable" => ("Onlining", Tone::Warning),
        "fitting_invulnerable" => ("Fitting invulnerable", Tone::Neutral),
        "online_deprecated" => ("Online", Tone::Neutral),
        "unanchored" => ("Unanchored", Tone::Neutral),
        _ => ("Unknown", Tone::Neutral),
    };
    badge(label, tone).into()
}

#[derive(Deserialize)]
struct Service {
    name: String,
    state: String,
}

fn services_text(json: &str) -> String {
    let services: Vec<Service> = serde_json::from_str(json).unwrap_or_default();
    if services.is_empty() {
        return "None".to_owned();
    }
    services
        .iter()
        .map(|s| {
            if s.state == "online" {
                s.name.clone()
            } else {
                format!("{} ({})", s.name, s.state)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A structure row, as read by [`STRUCTURE_ROW`].
const STRUCTURE_ROW: &str = "SELECT s.structure_id, s.name, coalesce(t.name, 'Type ' || s.type_id::text), \
        coalesce(y.name, sn.name, 'System ' || s.system_id::text), y.security_status, coalesce(r.name, ''), \
        s.fuel_expires, s.services::text, s.state, s.state_timer_end, s.reinforce_hour, \
        s.next_reinforce_hour, s.next_reinforce_apply, coalesce(o.name, 'Corporation ' || s.corporation_id::text) \
     FROM structures s \
     LEFT JOIN names t ON t.id = s.type_id \
     LEFT JOIN systems y ON y.system_id = s.system_id \
     LEFT JOIN names sn ON sn.id = s.system_id \
     LEFT JOIN names r ON r.id = y.region_id \
     LEFT JOIN names o ON o.id = s.corporation_id";

fn structure_row(row: &[Db], now: DateTime<Utc>, alert: i64) -> Vec<Value> {
    let system = match row.get(4).and_then(Db::as_float) {
        Some(sec) => format!("{} ({sec:.1})", text(row, 3)),
        None => text(row, 3),
    };
    let (expires, remaining): (Value, Value) = match when(row, 6) {
        Some(t) => {
            let d = t - now;
            let tone = if d < Duration::hours(24) {
                Tone::Danger
            } else if d < Duration::hours(alert) {
                Tone::Warning
            } else {
                Tone::Neutral
            };
            (time(rfc3339(t)), badge(left(d), tone).into())
        }
        None => ("".into(), badge("Low power", Tone::Warning).into()),
    };
    let reinforce = match (opt_int(row, 10), opt_int(row, 11), when(row, 12)) {
        (Some(h), Some(next), Some(from)) => format!(
            "{h:02}:00 ({next:02}:00 from {})",
            from.format("%Y-%m-%d %H:%M")
        ),
        (Some(h), _, _) => format!("{h:02}:00"),
        _ => String::new(),
    };
    vec![
        text(row, 13).into(),
        text(row, 1).into(),
        text(row, 2).into(),
        system.into(),
        text(row, 5).into(),
        expires,
        remaining,
        services_text(&text(row, 7)).into(),
        state_badge(&text(row, 8)),
        when(row, 9).map_or_else(|| "".into(), |t| time(rfc3339(t))),
        reinforce.into(),
    ]
}

fn structure_table(title: &str, empty: &str, rows: Vec<Vec<Value>>) -> Table {
    with_rows(
        Table::new(vec![
            Column::text("Owner"),
            Column::text("Name"),
            Column::text("Type"),
            Column::text("System"),
            Column::text("Region"),
            Column::numeric("Fuel expires"),
            Column::text("Fuel left"),
            Column::text("Services"),
            Column::text("State"),
            Column::numeric("State timer"),
            Column::text("Reinforce hour"),
        ])
        .title(title)
        .empty(empty),
        rows,
    )
}

fn list_page(viewer: &Viewer, owner: Option<i64>) -> Result<Page, PageError> {
    let Some(mut params) = visibility(viewer) else {
        return Ok(Page::new("Structures").text(
            "You can open Structures but may not see any structures. An admin grants \
             view_corporation_structures, view_alliance_structures or view_all_structures.",
        ));
    };
    params.push(owner.into());
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let alert = settings.thresholds.first().copied().unwrap_or(72);
    let now = Utc::now();
    let scope = format!("WHERE {VISIBLE} AND ($4::bigint IS NULL OR s.corporation_id = $4)");
    let owner_name = match owner {
        Some(corp) => {
            // Only an owner the viewer may see.
            let mut p = params[..3].to_vec();
            p.push(corp.into());
            let rows = storage::query(
                &format!(
                    "SELECT coalesce(n.name, 'Corporation ' || o.corporation_id::text) FROM owners o \
                     LEFT JOIN names n ON n.id = o.corporation_id WHERE o.corporation_id = $4 AND {} LIMIT 1",
                    VISIBLE.replace("s.corporation_id", "o.corporation_id")
                ),
                &p,
            )
            .map_err(|e| failed("reading owners", e))?;
            Some(
                rows.rows
                    .first()
                    .map(|r| text(r, 0))
                    .ok_or(PageError::NotFound)?,
            )
        }
        None => None,
    };
    let query = |filter: &str, order: &str, limit: i64| {
        let mut p = params.clone();
        p.push(limit.into());
        storage::query(
            &format!("{STRUCTURE_ROW} {scope} {filter} ORDER BY {order} LIMIT $5"),
            &p,
        )
        .map_err(|e| failed("reading structures", e))
    };
    let all = query("", "o.name, s.name", LIST_ROWS)?;
    let low = query(
        &format!(
            "AND (s.fuel_expires IS NULL OR s.fuel_expires <= now() + interval '{alert} hours')"
        ),
        "s.fuel_expires NULLS FIRST",
        SHORT_ROWS,
    )?;
    let reinforced = query(
        "AND s.state IN ('armor_reinforce', 'armor_vulnerable', 'hull_reinforce', 'hull_vulnerable')",
        "s.state_timer_end NULLS LAST",
        SHORT_ROWS,
    )?;
    let counts = storage::query(
        &format!(
            "SELECT count(*), \
                 count(*) FILTER (WHERE s.fuel_expires IS NULL OR s.fuel_expires <= now() + interval '{alert} hours'), \
                 count(*) FILTER (WHERE s.state IN ('armor_reinforce', 'armor_vulnerable', 'hull_reinforce', 'hull_vulnerable')) \
             FROM structures s {scope}"
        ),
        &params,
    )
    .map_err(|e| failed("counting structures", e))?;
    let timers = storage::query(
        &format!(
            "SELECT t.kind, t.at, s.name, coalesce(y.name, sn.name, ''), coalesce(o.name, '') \
             FROM timers t JOIN structures s ON s.structure_id = t.structure_id \
             LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
             LEFT JOIN names o ON o.id = s.corporation_id \
             {scope} AND t.at > now() ORDER BY t.at LIMIT {SHORT_ROWS}"
        ),
        &params,
    )
    .map_err(|e| failed("reading timers", e))?;
    let owners = storage::query(
        &format!(
            "SELECT o.corporation_id, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
                 coalesce(a.name, ''), \
                 (SELECT count(*) FROM structures s WHERE s.corporation_id = o.corporation_id), \
                 max(o.structures_at), bool_and(o.structures_retry_at > now()) \
             FROM owners o LEFT JOIN names n ON n.id = o.corporation_id LEFT JOIN names a ON a.id = o.alliance_id \
             WHERE {} \
             GROUP BY o.corporation_id, n.name, a.name ORDER BY 2 LIMIT {SHORT_ROWS}",
            VISIBLE.replace("s.corporation_id", "o.corporation_id")
        ),
        &params[..3],
    )
    .map_err(|e| failed("reading owners", e))?;

    let row = counts.rows.first();
    let total = row.map_or(0, |r| int(r, 0));
    let (low_count, reinforced_count) =
        (row.map_or(0, |r| int(r, 1)), row.map_or(0, |r| int(r, 2)));
    let next = timers.rows.first().and_then(|r| when(r, 1));
    let structures = all
        .rows
        .iter()
        .map(|r| structure_row(r, now, alert))
        .collect();
    let mut list = vec![Section::Table(structure_table(
        "Structures",
        "No structures yet. Owners' structures appear within the hour.",
        structures,
    ))];
    if total > LIST_ROWS {
        list.push(Section::Text(format!(
            "Showing the first {LIST_ROWS} of {total}: pick an owner on the Owners tab to see theirs."
        )));
    }
    let timer_table = with_rows(
        Table::new(vec![
            Column::numeric("When (EVE)"),
            Column::text("Timer"),
            Column::text("Structure"),
            Column::text("System"),
            Column::text("Owner"),
        ])
        .title("Upcoming timers")
        .empty("No upcoming timers."),
        timers.rows.iter().map(|r| {
            vec![
                when(r, 1).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                text(r, 0).into(),
                text(r, 2).into(),
                text(r, 3).into(),
                text(r, 4).into(),
            ]
        }),
    );
    let owner_table = with_rows(
        Table::new(vec![
            Column::text("Corporation"),
            Column::text("Alliance"),
            Column::numeric("Structures"),
            Column::numeric("Updated"),
            Column::text("Status"),
        ])
        .title("Owners")
        .empty("No owners yet."),
        owners.rows.iter().map(|r| {
            let status = if r.get(5).and_then(Db::as_bool).unwrap_or(false) {
                badge("Can't read", Tone::Danger)
            } else {
                badge("OK", Tone::Success)
            };
            vec![
                link(text(r, 1), format!("owner/{}", int(r, 0))).into(),
                text(r, 2).into(),
                int(r, 3).into(),
                when(r, 4).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                status.into(),
            ]
        }),
    );
    let title = match &owner_name {
        Some(name) => format!("Structures: {name}"),
        None => "Structures".to_owned(),
    };
    let mut page = Page::new(title)
        .description("Owners' Upwell structures, from ESI hourly; timers and alerts from their notifications")
        .stats(vec![
            Stat::new("Structures", total),
            Stat::new("Low fuel", low_count).caption(format!("under {alert} hours, or low power")),
            Stat::new("Reinforced", reinforced_count),
            match next {
                Some(t) => Stat::new("Next timer", badge(left(t - now), Tone::Accent))
                    .caption(format!("{} EVE", t.format("%Y-%m-%d %H:%M"))),
                None => Stat::new("Next timer", "none"),
            },
        ])
        .tab("Structures", list)
        .tab(
            "Low fuel",
            vec![Section::Table(structure_table(
                &format!("Under {alert} hours of fuel, or low power"),
                "No structure is low on fuel.",
                low.rows.iter().map(|r| structure_row(r, now, alert)).collect(),
            ))],
        )
        .tab(
            "Reinforced",
            vec![Section::Table(structure_table(
                "Reinforced or vulnerable",
                "No structure is reinforced.",
                reinforced
                    .rows
                    .iter()
                    .map(|r| structure_row(r, now, alert))
                    .collect(),
            ))],
        )
        .tab(
            "Timers",
            vec![
                Section::Table(timer_table),
                Section::Text(
                    "From structures' states and their notifications. Structure Timers shows \
                     them too, as automatic timers (corporation-only if the settings say so)."
                        .to_owned(),
                ),
            ],
        );
    if owner.is_none() {
        page = page.tab(
            "Owners",
            vec![
                Section::Table(owner_table),
                Section::Text(
                    "Add Structure Owner: a character with the in-game Station Manager role offers \
                     itself on the Dashboard (Structures: corporation data), and an admin approves it. \
                     Its corporation's structures show here within the hour."
                        .to_owned(),
                ),
            ],
        );
    } else {
        page = page.card(
            tether_plugin_sdk::Card::new("Owner")
                .field("All owners", link("Back to every owner", "")),
        );
    }
    Ok(page)
}

fn channel_field(name: &str, label: &str, help: &str, value: Option<&str>) -> Field {
    let mut options: Vec<(String, String)> = vec![(String::new(), "Not sent".to_owned())];
    options.extend(
        discord::channels()
            .into_iter()
            .map(|c| (c.id, format!("#{}", c.name))),
    );
    Field::select(name, label, options)
        .value(value.unwrap_or_default())
        .help(help)
}

fn settings_page(problem: Option<&str>) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let owners = storage::query(
        "SELECT o.character_id, o.character_name, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
             o.structures_at, o.notifications_at, o.last_error, \
             greatest(o.structures_retry_at, o.notifications_retry_at) \
         FROM owners o LEFT JOIN names n ON n.id = o.corporation_id ORDER BY 3, 2",
        &[],
    )
    .map_err(|e| failed("reading owners", e))?;
    let sent = storage::query(
        "SELECT created_at, message, sent_at IS NOT NULL, failed FROM outbox ORDER BY id DESC LIMIT 50",
        &[],
    )
    .map_err(|e| failed("reading messages", e))?;
    let now = Utc::now();
    let mut page = Page::new("Structures settings").description(
        "Where structure notifications go on Discord, and when Tether warns about fuel",
    );
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    let form = Form::new("settings", "Save")
        .title("Discord")
        .description(
            "Each kind of notification goes to one of the channels an admin assigned Structures \
             (Admin → Apps), within a day of it happening, once. Every owner's notifications and \
             alerts go to these channels, whatever the view permissions: anyone who can read a \
             channel sees every owner's structures named in it.",
        )
        .field(channel_field(
            "attack_channel",
            "Attacks",
            "Under attack, lost shields or armor (with the timer), destroyed.",
            settings.attack.as_deref(),
        ))
        .field(channel_field(
            "fuel_channel",
            "Fuel and services",
            "EVE's fuel alerts, services offline, low power, and Tether's low-fuel alerts below.",
            settings.fuel.as_deref(),
        ))
        .field(channel_field(
            "state_channel",
            "State changes",
            "Online, high power, anchoring and unanchoring.",
            settings.state.as_deref(),
        ))
        .field(channel_field(
            "moon_channel",
            "Moon extractions",
            "Extractions started, chunks arrived, fractures and cancellations.",
            settings.moon.as_deref(),
        ))
        .field(
            Field::text("fuel_thresholds", "Low-fuel alerts (hours left)", 40)
                .value(
                    settings
                        .thresholds
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
                .help("Tether posts once as a structure's fuel falls under each, e.g. 72, 24, 6 (at most 5, up to 2160).")
                .required(),
        )
        .field(Field::checkbox(
            "mention_members",
            "Mention Members on attacks (the Discord role mapped to Member)",
            settings.mention,
        ))
        .field(
            Field::checkbox(
                "timers_corporation_only",
                "Timers are corporation-only",
                settings.timers_corporation_only,
            )
            .help(
                "The timers Structures gives Structure Timers are seen only by pilots whose \
                 main is in the structure's corporation. Off: everyone who may see Structure \
                 Timers sees them.",
            ),
        );
    let owner_rows = owners.rows.iter().map(|r| {
        let backing_off = when(r, 6).filter(|t| *t > now);
        let status = match (&backing_off, r.get(5).and_then(Db::as_text)) {
            (Some(_), _) => badge("Backing off", Tone::Danger),
            (None, Some(_)) => badge("Last read failed", Tone::Warning),
            (None, None) => badge("OK", Tone::Success),
        };
        vec![
            text(r, 2).into(),
            text(r, 1).into(),
            when(r, 3).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            when(r, 4).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            status.into(),
            text(r, 5).into(),
            backing_off.map_or_else(|| "".into(), |t| time(rfc3339(t))),
        ]
    });
    let owner_table = with_rows(
        Table::new(vec![
            Column::text("Corporation"),
            Column::text("Character"),
            Column::numeric("Structures read"),
            Column::numeric("Notifications read"),
            Column::text("Status"),
            Column::text("Last problem"),
            Column::numeric("Next try"),
        ])
        .title("Owners")
        .empty("No owners yet: a Station Manager offers a character on the Dashboard, and an admin approves it."),
        owner_rows,
    );
    let choices: Vec<(String, String)> = owners
        .rows
        .iter()
        .map(|r| {
            (
                int(r, 0).to_string(),
                format!("{} ({})", text(r, 1), text(r, 2)),
            )
        })
        .collect();
    let sent_table = with_rows(
        Table::new(vec![
            Column::numeric("Queued"),
            Column::text("Message"),
            Column::text("Status"),
        ])
        .title("Recent messages")
        .empty("Nothing sent yet."),
        sent.rows.iter().map(|r| {
            let status = match (
                r.get(2).and_then(Db::as_bool).unwrap_or(false),
                r.get(3).and_then(Db::as_text),
            ) {
                (_, Some(why)) => badge(format!("Not sent: {why}"), Tone::Danger),
                (true, None) => badge("Sent", Tone::Success),
                (false, None) => badge("Waiting", Tone::Neutral),
            };
            vec![
                when(r, 0).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                text(r, 1).into(),
                status.into(),
            ]
        }),
    );
    page = page.form(form).table(owner_table);
    if !choices.is_empty() {
        page = page.form(
            Form::new("retry", "Retry now")
                .title("Retry an owner")
                .description(
                    "An owner ESI refused (a lost role or token) is left alone for an hour, doubling \
                     up to a day. Once it's fixed in game, retry it now.",
                )
                .field(Field::select("owner", "Owner", choices).required()),
        );
    }
    Ok(page.table(sent_table))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let Some(thresholds) = parse_thresholds(submission.value("fuel_thresholds")) else {
        return Ok(SubmitResult::Page(settings_page(Some(
            "Write the low-fuel alerts as hours separated by commas, e.g. 72, 24, 6: whole \
             numbers from 1 to 2160, at most 5.",
        ))?));
    };
    let thresholds = id_list(&thresholds);
    let channel = |name: &str| {
        let c = submission.value(name);
        (!c.is_empty()).then(|| c.to_owned())
    };
    storage::execute(
        "UPDATE settings SET attack_channel = $1, fuel_channel = $2, state_channel = $3, \
         moon_channel = $4, fuel_thresholds = $5, mention_members = $6, \
         timers_corporation_only = $7 WHERE id = 1",
        &[
            channel("attack_channel").into(),
            channel("fuel_channel").into(),
            channel("state_channel").into(),
            channel("moon_channel").into(),
            thresholds.as_str().into(),
            submission.checked("mention_members").into(),
            submission.checked("timers_corporation_only").into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    // Published again at once, so corporation-only takes effect now.
    jobs::enqueue(NewJob::new(PUBLISH_JOB).key(PUBLISH_JOB))
        .map_err(|e| failed("queuing the timers", e))?;
    log::info(format!(
        "settings changed by {} ({}): attacks {:?}, fuel {:?}, state {:?}, moons {:?}, alerts at {thresholds}h, mention {}, \
         timers corporation-only {}",
        viewer.main.name,
        viewer.main.id,
        channel("attack_channel"),
        channel("fuel_channel"),
        channel("state_channel"),
        channel("moon_channel"),
        submission.checked("mention_members"),
        submission.checked("timers_corporation_only"),
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

fn retry_owner(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let owner: i64 = submission
        .value("owner")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "UPDATE owners SET structures_failures = 0, structures_retry_at = NULL, \
         notifications_failures = 0, notifications_retry_at = NULL, last_error = NULL \
         WHERE character_id = $1",
        &[owner.into()],
    )
    .map_err(|e| failed("resetting the owner", e))?;
    jobs::enqueue(NewJob::new("sync").key("sync-now")).map_err(|e| failed("queuing a sync", e))?;
    log::info(format!(
        "owner {owner} retried by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_whole_hours_largest_first() {
        assert_eq!(parse_thresholds("6, 72,24"), Some(vec![72, 24, 6]));
        assert_eq!(parse_thresholds("24,24"), Some(vec![24]));
        assert_eq!(parse_thresholds(""), None);
        assert_eq!(parse_thresholds("0"), None);
        assert_eq!(parse_thresholds("2161"), None);
        assert_eq!(parse_thresholds("1,2,3,4,5,6"), None);
        assert_eq!(parse_thresholds("1.5"), None);
    }

    #[test]
    fn time_left_reads_short() {
        assert_eq!(left(Duration::hours(76)), "3d 4h");
        assert_eq!(left(Duration::minutes(312)), "5h 12m");
        assert_eq!(left(Duration::minutes(-1)), "none");
    }

    #[test]
    fn services_show_what_is_offline() {
        assert_eq!(
            services_text(
                r#"[{"name":"Manufacturing (Standard)","state":"online"},{"name":"Clone Bay","state":"offline"}]"#
            ),
            "Manufacturing (Standard), Clone Bay (offline)"
        );
        assert_eq!(services_text("[]"), "None");
    }
}
