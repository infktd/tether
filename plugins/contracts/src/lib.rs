//! Contracts (Jay, 2026-09-29; what Bastion's contract notices did).
//!
//! - **Owners**: characters added with Add owner by `add_contract_owner`
//!   holders; each one's corporation's contracts are read every five
//!   minutes, and those assigned to the corporation kept.
//! - **Discord** (`manage` picks the channel and what's sent): a card when
//!   a contract comes in, when it's completed, and when it expires or is
//!   rejected, cancelled or deleted, each switchable. A contract's
//!   description may link a Janice appraisal: its buy total is read once
//!   (with the admin's Janice API key, the app's secret) and the price
//!   checked against it, within a tolerance set in Settings. Contracts
//!   asking no ISK say so.
//! - **Contracts** (`view_contracts`): the latest, with their check.
//!
//! A corporation's first read takes what's there as the backlog: nothing
//! that happened before it is announced (a backlog contract completing or
//! expiring later is).

mod card;
mod janice;

use chrono::{DateTime, Duration, Utc};
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Column, Field, Form, Page, PageError, Plugin, Request, Stat, Submission, SubmitResult, Table,
    Tone, Value, badge, character, isk, log, time,
};

use card::{Check, Event, Notice};

const SYNC: &str = "sync";
const RELAY: &str = "relay";
const PUBLIC: Subject = Subject::Character(0);
/// News older than this isn't sent (after downtime, say).
const STALE_HOURS: i64 = 24;
/// Contracts' items, structures' names and appraisals read per run, well
/// within a run's ESI and HTTP budgets.
const ITEMS_PER_RUN: i64 = 20;
const LOCATIONS_PER_RUN: i64 = 10;
const APPRAISALS_PER_RUN: i64 = 10;
/// An appraisal Janice hasn't answered for this long goes out unchecked.
const APPRAISAL_WAIT_MINUTES: i64 = 30;
/// Discord messages per relay run (the host's limit), and the gaps.
const SENDS_PER_RUN: usize = 5;
const RELAY_GAP_SECONDS: i64 = 15;
const RELAY_BACKOFF_SECONDS: i64 = 60;
/// Contracts listed on the page.
const LISTED: i64 = 100;

struct Contracts;

impl Plugin for Contracts {
    fn render(request: Request) -> Result<Page, PageError> {
        // Someone signed in (the host checked the page's permission).
        identity::viewer().ok_or(PageError::Forbidden)?;
        match request.path.as_str() {
            "" => index_page(),
            "settings" => settings_page(),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("settings", "settings") => save_settings(&viewer, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            SYNC => sync(),
            RELAY => relay(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Contracts);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn float(row: &[Db], i: usize) -> f64 {
    row.get(i).and_then(Db::as_float).unwrap_or_default()
}

fn opt_float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
}

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(row: &[Db], i: usize) -> Option<String> {
    row.get(i).and_then(Db::as_text).map(str::to_owned)
}

fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Discord markdown out of names players choose.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // No bare links either: Discord links `https://…` on its own.
    let text = text.replace("://", ":\u{200B}//");
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '@' | '<'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// NPC stations' ids; Upwell structures' are far larger.
fn is_station(id: i64) -> bool {
    (60_000_000..64_000_000).contains(&id)
}

fn is_structure(id: i64) -> bool {
    id > 1_000_000_000_000
}

// ---- settings --------------------------------------------------------------

struct Settings {
    channel: Option<String>,
    notify_new: bool,
    notify_completed: bool,
    notify_ended: bool,
    tolerance: f64,
    synced_at: Option<DateTime<Utc>>,
    sync_error: Option<String>,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT channel, notify_new, notify_completed, notify_ended, tolerance_percent, \
                synced_at, sync_error FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first();
    let flag = |i: usize, default: bool| {
        row.and_then(|r| r.get(i))
            .and_then(Db::as_bool)
            .unwrap_or(default)
    };
    Ok(Settings {
        channel: row.and_then(|r| opt_text(r, 0)),
        notify_new: flag(1, true),
        notify_completed: flag(2, true),
        notify_ended: flag(3, false),
        tolerance: row.and_then(|r| opt_float(r, 4)).unwrap_or(1.0),
        synced_at: row.and_then(|r| when(r, 5)),
        sync_error: row.and_then(|r| opt_text(r, 6)),
    })
}

// ---- the sync --------------------------------------------------------------

/// Each corporation once, read through its first owner.
fn sources() -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for source in esi::data_sources() {
        if !out.iter().any(|(corp, _)| *corp == source.corporation_id) {
            out.push((source.corporation_id, source.id));
        }
    }
    out
}

fn sync() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let sources = sources();
    if sources.is_empty() {
        return Ok(());
    }
    let mut problems: Vec<String> = Vec::new();
    for (corp, source) in &sources {
        match esi::get_all("corporation-contracts", Subject::DataSource(*source), &[]) {
            Ok(bodies) => {
                let mut assigned: Vec<serde_json::Value> = Vec::new();
                for body in bodies {
                    let page: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    assigned.extend(
                        page.as_array()
                            .into_iter()
                            .flatten()
                            .filter(|c| c["assignee_id"].as_i64() == Some(*corp))
                            .cloned(),
                    );
                }
                // A corporation's first read is its backlog: never
                // announced as new.
                let known = storage::query(
                    "SELECT 1 FROM corporations WHERE corporation_id = $1",
                    &[(*corp).into()],
                )
                .map_err(|e| retry("reading corporations", e))?;
                store(*corp, &assigned, known.rows.is_empty())?;
                storage::execute(
                    "INSERT INTO corporations (corporation_id) VALUES ($1) ON CONFLICT DO NOTHING",
                    &[(*corp).into()],
                )
                .map_err(|e| retry("noting a corporation", e))?;
            }
            Err(err) => problems.push(format!("corporation {corp}: contracts not read: {err:?}")),
        }
    }
    read_items(&sources)?;
    read_locations(&sources)?;
    read_appraisals()?;
    learn_names()?;
    let error = (!problems.is_empty()).then(|| problems.join("; "));
    if let Some(error) = &error {
        log::warn(error);
    }
    storage::execute(
        "UPDATE settings SET synced_at = now(), sync_error = $1 WHERE id = 1",
        &[error.into()],
    )
    .map_err(|e| retry("noting the sync", e))?;
    notify(&settings)?;
    relay()
}

fn store(corp: i64, contracts: &[serde_json::Value], backlog: bool) -> Result<(), JobError> {
    let rows: Vec<serde_json::Value> = contracts
        .iter()
        .filter_map(|c| {
            let title: String = c["title"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            Some(serde_json::json!({
                "contract_id": c["contract_id"].as_i64()?,
                "type": c["type"].as_str()?,
                "status": c["status"].as_str()?,
                "issuer_id": c["issuer_id"].as_i64()?,
                "issuer_corporation_id": c["issuer_corporation_id"].as_i64()?,
                "acceptor_id": c["acceptor_id"].as_i64().filter(|id| *id > 0),
                "start_location": c["start_location_id"].as_i64(),
                "end_location": c["end_location_id"].as_i64(),
                "price": c["price"].as_f64().unwrap_or(0.0),
                "reward": c["reward"].as_f64().unwrap_or(0.0),
                "collateral": c["collateral"].as_f64().unwrap_or(0.0),
                "volume": c["volume"].as_f64().unwrap_or(0.0),
                "appraisal_code": janice::code(&title),
                "title": title,
                "date_issued": c["date_issued"].as_str()?,
                "date_expired": c["date_expired"].as_str(),
                "date_completed": c["date_completed"].as_str(),
            }))
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    storage::execute(
        "INSERT INTO contracts (contract_id, corporation_id, type, status, issuer_id, \
             issuer_corporation_id, acceptor_id, start_location, end_location, price, reward, \
             collateral, volume, title, appraisal_code, date_issued, date_expired, date_completed, \
             backlog) \
         SELECT DISTINCT ON (contract_id) contract_id, $2, type, status, issuer_id, \
             issuer_corporation_id, acceptor_id, start_location, end_location, price, reward, \
             collateral, volume, title, appraisal_code, date_issued, date_expired, date_completed, $3 \
         FROM json_to_recordset($1::json) AS x(contract_id bigint, type text, status text, \
             issuer_id bigint, issuer_corporation_id bigint, acceptor_id bigint, \
             start_location bigint, end_location bigint, price double precision, \
             reward double precision, collateral double precision, volume double precision, \
             title text, appraisal_code text, date_issued timestamptz, date_expired timestamptz, \
             date_completed timestamptz) \
         ON CONFLICT (contract_id) DO UPDATE SET status = EXCLUDED.status, \
             acceptor_id = EXCLUDED.acceptor_id, date_expired = EXCLUDED.date_expired, \
             date_completed = EXCLUDED.date_completed, \
             updated_at = CASE WHEN contracts.status IS DISTINCT FROM EXCLUDED.status \
                 THEN now() ELSE contracts.updated_at END",
        &[
            Db::json(serde_json::Value::Array(rows).to_string()),
            corp.into(),
            backlog.into(),
        ],
    )
    .map_err(|e| retry("storing contracts", e))?;
    Ok(())
}

/// An ESI refusal that won't change (not a rate limit, which does).
fn final_refusal(status: u16) -> bool {
    (400..500).contains(&status) && status != 420 && status != 429
}

fn source_for(sources: &[(i64, i64)], corp: i64) -> Option<Subject> {
    sources
        .iter()
        .find(|(c, _)| *c == corp)
        .map(|(_, source)| Subject::DataSource(*source))
}

/// What new contracts hand over or ask for.
fn read_items(sources: &[(i64, i64)]) -> Result<(), JobError> {
    let due = storage::query(
        "SELECT contract_id, corporation_id FROM contracts \
         WHERE items IS NULL AND items_error IS NULL AND NOT backlog \
           AND date_issued > now() - make_interval(hours => $1::int) \
         ORDER BY date_issued DESC LIMIT $2",
        &[STALE_HOURS.into(), ITEMS_PER_RUN.into()],
    )
    .map_err(|e| retry("finding contracts' items", e))?;
    for row in &due.rows {
        let (contract, corp) = (int(row, 0), int(row, 1));
        let Some(subject) = source_for(sources, corp) else {
            continue;
        };
        let (items, error) = match esi::get(
            "corporation-contract-items",
            subject,
            &[("contract_id".to_owned(), contract.to_string())],
            None,
        ) {
            Ok(answer) => {
                let list: serde_json::Value =
                    serde_json::from_str(&answer.body).unwrap_or_default();
                let items: Vec<serde_json::Value> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| {
                        Some(serde_json::json!([
                            i["type_id"].as_i64()?,
                            i["quantity"].as_i64()?,
                            i["is_included"].as_bool().unwrap_or(true),
                        ]))
                    })
                    .collect();
                (Some(serde_json::Value::Array(items).to_string()), None)
            }
            // Gone, or not the corporation's to read: the card goes
            // without its items.
            Err(esi::Error::Status(status)) if final_refusal(status) => {
                (None, Some(format!("ESI answered {status}")))
            }
            Err(err) => {
                log::info(format!("contract {contract}'s items not read: {err:?}"));
                continue;
            }
        };
        storage::execute(
            "UPDATE contracts SET items = $2::jsonb, items_error = $3 WHERE contract_id = $1",
            &[
                contract.into(),
                items.map_or(Db::Null, Db::json),
                error.into(),
            ],
        )
        .map_err(|e| retry("storing a contract's items", e))?;
    }
    Ok(())
}

/// Names for every listed contract's stations (public) and structures (as
/// the owner sees them; one it can't dock at, or whose owner is gone,
/// stays a number), the newest contracts' first.
fn read_locations(sources: &[(i64, i64)]) -> Result<(), JobError> {
    let due = storage::query(
        "SELECT l.id, (array_agg(c.corporation_id ORDER BY c.date_issued DESC))[1] \
         FROM contracts c \
         CROSS JOIN LATERAL (VALUES (c.start_location), (c.end_location)) l(id) \
         WHERE l.id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM locations x WHERE x.id = l.id) \
         GROUP BY l.id ORDER BY max(c.date_issued) DESC LIMIT $1",
        &[LOCATIONS_PER_RUN.into()],
    )
    .map_err(|e| retry("finding locations", e))?;
    for row in &due.rows {
        let (id, corp) = (int(row, 0), int(row, 1));
        let answer = if is_station(id) {
            esi::get(
                "universe-station",
                PUBLIC,
                &[("station_id".to_owned(), id.to_string())],
                None,
            )
        } else if is_structure(id) {
            // Its owner is gone: nobody to ask, so it stays a number.
            let Some(subject) = source_for(sources, corp) else {
                storage::execute(
                    "INSERT INTO locations (id, name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[id.into(), format!("Structure {id}").into()],
                )
                .map_err(|e| retry("storing a location", e))?;
                continue;
            };
            esi::get(
                "source-structure",
                subject,
                &[("structure_id".to_owned(), id.to_string())],
                None,
            )
        } else {
            continue;
        };
        let (name, system) = match answer {
            Ok(answer) => {
                let place: serde_json::Value =
                    serde_json::from_str(&answer.body).unwrap_or_default();
                let system = place["system_id"]
                    .as_i64()
                    .or_else(|| place["solar_system_id"].as_i64())
                    .and_then(|s| esi::names(&[s]).ok())
                    .and_then(|named| named.into_iter().next())
                    .map(|n| n.name)
                    .unwrap_or_default();
                let name: String = place["name"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                if name.is_empty() {
                    (format!("Location {id}"), system)
                } else {
                    (name, system)
                }
            }
            Err(esi::Error::Status(status)) if final_refusal(status) => {
                (format!("Structure {id}"), String::new())
            }
            Err(err) => {
                log::info(format!("location {id} not read: {err:?}"));
                continue;
            }
        };
        storage::execute(
            "INSERT INTO locations (id, name, system_name) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, system_name = EXCLUDED.system_name",
            &[id.into(), name.into(), system.into()],
        )
        .map_err(|e| retry("storing a location", e))?;
    }
    Ok(())
}

/// Each new contract's linked appraisal, read once its items are, and
/// checked against them: the issuer picks the appraisal, so it vouches
/// only for an appraisal of what the contract hands over, made about when
/// it was.
fn read_appraisals() -> Result<(), JobError> {
    let due = storage::query(
        "SELECT contract_id, appraisal_code, items::text, items_error, date_issued FROM contracts \
         WHERE appraisal_code IS NOT NULL AND checked_at IS NULL AND NOT backlog \
           AND (items IS NOT NULL OR items_error IS NOT NULL) \
           AND date_issued > now() - make_interval(hours => $1::int) \
         ORDER BY date_issued DESC LIMIT $2",
        &[STALE_HOURS.into(), APPRAISALS_PER_RUN.into()],
    )
    .map_err(|e| retry("finding appraisals", e))?;
    for row in &due.rows {
        let (contract, code) = (int(row, 0), text(row, 1));
        let included: Vec<(i64, i64)> = contract_items(opt_text(row, 2).as_deref())
            .into_iter()
            .filter(|i| i.2)
            .map(|i| (i.0, i.1))
            .collect();
        let issued = when(row, 4).unwrap_or_else(Utc::now);
        let (buy, problem, note) = match janice::read(&code) {
            Ok(janice::Appraisal::Buy {
                buy,
                items,
                created,
            }) => {
                let problem = if opt_text(row, 3).is_some() {
                    Some("the contract's items couldn't be read to compare".to_owned())
                } else {
                    janice::problem(&items, created, &included, issued)
                };
                (Some(buy), problem, None)
            }
            Ok(janice::Appraisal::NotChecked(why)) => (None, None, Some(why)),
            Err(err) => {
                log::info(format!("appraisal {code}: {err}"));
                continue;
            }
        };
        storage::execute(
            "UPDATE contracts SET appraisal_buy = $2, appraisal_problem = $3, appraisal_note = $4, \
             checked_at = now() WHERE contract_id = $1",
            &[contract.into(), buy.into(), problem.into(), note.into()],
        )
        .map_err(|e| retry("storing an appraisal", e))?;
    }
    Ok(())
}

/// A contract's stored items: (type id, quantity, included).
fn contract_items(json: Option<&str>) -> Vec<(i64, i64, bool)> {
    json.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|i| {
            Some((
                i[0].as_i64()?,
                i[1].as_i64()?,
                i[2].as_bool().unwrap_or(true),
            ))
        })
        .collect()
}

/// Names for the people, corporations and items of contracts not named
/// yet (structures and stations are locations).
fn learn_names() -> Result<(), JobError> {
    let wanted = storage::query(
        "SELECT DISTINCT id FROM ( \
             SELECT issuer_id AS id FROM contracts UNION SELECT issuer_corporation_id FROM contracts \
             UNION SELECT acceptor_id FROM contracts UNION SELECT corporation_id FROM contracts \
             UNION SELECT (i ->> 0)::bigint FROM contracts, jsonb_array_elements(coalesce(items, '[]')) i) x \
         WHERE id IS NOT NULL AND id > 0 AND id < 1000000000000 \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = x.id) LIMIT 2000",
        &[],
    )
    .map_err(|e| retry("finding names", e))?;
    let ids: Vec<i64> = wanted.rows.iter().map(|r| int(r, 0)).collect();
    for chunk in ids.chunks(1000) {
        match esi::names(chunk) {
            Ok(named) => {
                let rows: Vec<serde_json::Value> = named
                    .into_iter()
                    .map(|n| serde_json::json!({ "id": n.id, "name": n.name }))
                    .collect();
                storage::execute(
                    "INSERT INTO names (id, name) \
                     SELECT id, name FROM json_to_recordset($1::json) AS x(id bigint, name text) \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                    &[Db::json(serde_json::Value::Array(rows).to_string())],
                )
                .map_err(|e| retry("storing names", e))?;
            }
            Err(err) => log::info(format!("names not read: {err:?}")),
        }
    }
    Ok(())
}

// ---- notices ---------------------------------------------------------------

const ENDED: &str = "('rejected', 'cancelled', 'deleted', 'failed', 'reversed')";
const FINISHED: &str = "('finished', 'finished_issuer', 'finished_contractor')";

const NOTICE_COLUMNS: &str = "c.contract_id, c.type, c.status, c.corporation_id, \
     coalesce(co.name, 'Corporation ' || c.corporation_id), coalesce(i.name, 'Someone'), \
     a.name, coalesce(s.name, 'Location ' || c.start_location), e.name, c.price, c.reward, \
     c.collateral, c.volume, c.date_expired, c.items::text, c.appraisal_code, c.appraisal_buy, \
     c.appraisal_note, c.checked_at, c.date_completed, c.updated_at, c.date_issued, \
     c.appraisal_problem";

const NOTICE_JOINS: &str = "FROM contracts c \
     LEFT JOIN names co ON co.id = c.corporation_id LEFT JOIN names i ON i.id = c.issuer_id \
     LEFT JOIN names a ON a.id = c.acceptor_id LEFT JOIN locations s ON s.id = c.start_location \
     LEFT JOIN locations e ON e.id = c.end_location";

/// Queues the notices settings ask for, each once.
fn notify(settings: &Settings) -> Result<(), JobError> {
    let Some(channel) = settings.channel.clone() else {
        return Ok(());
    };
    let mut wanted: Vec<(Event, String)> = Vec::new();
    if settings.notify_new {
        // Ready once its items are read and its appraisal checked (or
        // Janice has had long enough).
        wanted.push((
            Event::New,
            format!(
                "c.status = 'outstanding' AND NOT c.backlog \
                 AND c.date_issued > now() - interval '{STALE_HOURS} hours' \
                 AND (c.items IS NOT NULL OR c.items_error IS NOT NULL) \
                 AND (c.appraisal_code IS NULL OR c.checked_at IS NOT NULL \
                      OR c.first_seen < now() - interval '{APPRAISAL_WAIT_MINUTES} minutes')"
            ),
        ));
    }
    if settings.notify_completed {
        wanted.push((
            Event::Completed,
            format!(
                "c.status IN {FINISHED} \
                 AND coalesce(c.date_completed, c.updated_at) > now() - interval '{STALE_HOURS} hours' \
                 AND (NOT c.backlog OR c.updated_at > c.first_seen)"
            ),
        ));
    }
    if settings.notify_ended {
        wanted.push((
            Event::Ended,
            format!(
                "((c.status IN {ENDED} AND c.updated_at > now() - interval '{STALE_HOURS} hours' \
                   AND c.updated_at > c.first_seen) \
                  OR (c.status = 'outstanding' AND c.date_expired < now() \
                      AND c.date_expired > now() - interval '{STALE_HOURS} hours' \
                      AND c.date_expired > c.first_seen))"
            ),
        ));
    }
    for (event, filter) in wanted {
        let rows = storage::query(
            &format!(
                "SELECT {NOTICE_COLUMNS} {NOTICE_JOINS} WHERE {filter} \
                 AND NOT EXISTS (SELECT 1 FROM notices n WHERE n.contract_id = c.contract_id \
                     AND n.event = $1) \
                 ORDER BY c.date_issued LIMIT 20"
            ),
            &[event.key().into()],
        )
        .map_err(|e| retry("finding notices", e))?;
        for row in &rows.rows {
            let notice = notice(row, event, settings.tolerance)?;
            let (message, card) = card::build(&notice);
            // Noted and queued in one statement: two syncs at once can't
            // both send it.
            storage::execute(
                "WITH noticed AS (INSERT INTO notices (contract_id, event) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING RETURNING 1) \
                 INSERT INTO outbox (channel, message, card) SELECT $3, $4, $5 FROM noticed",
                &[
                    int(row, 0).into(),
                    event.key().into(),
                    channel.clone().into(),
                    message.into(),
                    Db::json(card.to_string()),
                ],
            )
            .map_err(|e| retry("queuing a notice", e))?;
        }
    }
    Ok(())
}

/// A notice from a `NOTICE_COLUMNS` row.
fn notice(row: &[Db], event: Event, tolerance: f64) -> Result<Notice, JobError> {
    let status = text(row, 2);
    let expired = status == "outstanding" && when(row, 13).is_some_and(|t| t < Utc::now());
    let items = contract_items(opt_text(row, 14).as_deref());
    let type_ids: Vec<i64> = items.iter().map(|i| i.0).collect();
    let names = storage::query(
        "SELECT id, name FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[id_list(&type_ids).into()],
    )
    .map_err(|e| retry("reading item names", e))?;
    let name_of = |id: i64| {
        names
            .rows
            .iter()
            .find(|r| int(r, 0) == id)
            .map_or_else(|| format!("Type {id}"), |r| escape(&text(r, 1)))
    };
    let code = opt_text(row, 15);
    let check = match (&code, opt_float(row, 16), opt_text(row, 17), when(row, 18)) {
        (None, ..) => Check::NoLink,
        (Some(_), Some(buy), _, _) => match opt_text(row, 22) {
            Some(problem) => Check::Differs(buy, problem),
            None => Check::Buy(buy),
        },
        (Some(_), None, Some(note), _) => Check::NotChecked(note),
        (Some(_), None, None, _) => Check::NotChecked("Janice didn't answer".to_owned()),
    };
    let at = match event {
        Event::New => when(row, 21),
        Event::Completed => when(row, 19).or_else(|| when(row, 20)),
        Event::Ended => when(row, 20),
    }
    .unwrap_or_else(Utc::now);
    Ok(Notice {
        event,
        kind: text(row, 1),
        status: if expired {
            "expired".to_owned()
        } else {
            status
        },
        corporation: (int(row, 3), escape(&text(row, 4))),
        corporation_plain: text(row, 4),
        issuer: escape(&text(row, 5)),
        acceptor: opt_text(row, 6).map(|n| escape(&n)),
        location: escape(&text(row, 7)),
        end_location: opt_text(row, 8).map(|n| escape(&n)),
        price: float(row, 9),
        reward: float(row, 10),
        collateral: float(row, 11),
        volume: float(row, 12),
        expires: when(row, 13),
        at,
        items: items
            .into_iter()
            .map(|(id, quantity, included)| (id, name_of(id), quantity, included))
            .collect(),
        appraisal: code,
        check,
        tolerance,
    })
}

/// Sends up to the host's limit, then comes back for the rest.
fn relay() -> Result<(), JobError> {
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND queued_at < now() - make_interval(hours => $1::int)",
        &[STALE_HOURS.into()],
    )
    .map_err(|e| retry("expiring messages", e))?;
    let mut gap = RELAY_GAP_SECONDS;
    let waiting = storage::query(
        "SELECT id, channel, message, card::text FROM outbox \
         WHERE sent_at IS NULL AND failed IS NULL ORDER BY id LIMIT $1",
        &[(SENDS_PER_RUN as i64).into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    for row in &waiting.rows {
        let id = int(row, 0);
        let claimed = storage::execute(
            "UPDATE outbox SET sent_at = now() WHERE id = $1 AND sent_at IS NULL AND failed IS NULL",
            &[id.into()],
        )
        .map_err(|e| retry("claiming a message", e))?;
        if claimed == 0 {
            continue;
        }
        let card = opt_text(row, 3)
            .and_then(|c| serde_json::from_str(&c).ok())
            .and_then(|c| card::embed(&c));
        let sent = match &card {
            Some(card) => discord::send_embed(&text(row, 1), card, Mention::None),
            None => discord::send(&text(row, 1), &text(row, 2), Mention::None),
        };
        match sent {
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
                gap = RELAY_BACKOFF_SECONDS;
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL WHERE id = $1",
                    &[id.into()],
                )
                .map_err(|e| retry("releasing a message", e))?;
                break;
            }
        }
    }
    // Sent messages are kept a week, for the record.
    storage::execute(
        "DELETE FROM outbox WHERE queued_at < now() - interval '7 days'",
        &[],
    )
    .map_err(|e| retry("clearing the outbox", e))?;
    let left = storage::query(
        "SELECT 1 FROM outbox WHERE sent_at IS NULL AND failed IS NULL LIMIT 1",
        &[],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    if !left.rows.is_empty() {
        jobs::enqueue(
            NewJob::new(RELAY)
                .key("relay")
                .at(rfc3339(Utc::now() + Duration::seconds(gap))),
        )
        .map_err(|e| retry("queuing the relay", e))?;
    }
    Ok(())
}

// ---- pages -----------------------------------------------------------------

fn check_badge(
    price: f64,
    code: Option<&str>,
    buy: Option<f64>,
    note: Option<&str>,
    tolerance: f64,
) -> Value {
    match (code, buy) {
        (None, _) if price <= 0.0 => badge("No ISK", Tone::Neutral).into(),
        (None, _) => badge("No appraisal", Tone::Neutral).into(),
        (Some(_), Some(_)) if price <= 0.0 => badge("No ISK", Tone::Neutral).into(),
        (Some(_), Some(buy)) => {
            let (off, ok) = janice::compare(price, buy, tolerance);
            if ok {
                badge("Matches", Tone::Success).into()
            } else {
                badge(
                    format!(
                        "{:.1}% {}",
                        off.abs(),
                        if off > 0.0 { "over" } else { "under" }
                    ),
                    Tone::Danger,
                )
                .into()
            }
        }
        (Some(_), None) => badge(
            if note.is_some() {
                "Not checked"
            } else {
                "Checking"
            },
            Tone::Warning,
        )
        .into(),
    }
}

fn status_badge(status: &str, expired: bool) -> Value {
    let label = if expired {
        "Expired"
    } else {
        card::status_label(status)
    };
    let tone = match status {
        _ if expired => Tone::Neutral,
        "outstanding" => Tone::Accent,
        "in_progress" => Tone::Warning,
        "finished" | "finished_issuer" | "finished_contractor" => Tone::Success,
        _ => Tone::Neutral,
    };
    badge(label, tone).into()
}

fn index_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let rows = storage::query(
        &format!(
            "SELECT c.date_issued, c.type, c.issuer_id, coalesce(i.name, 'Character ' || c.issuer_id), \
                    coalesce(s.name, 'Location ' || c.start_location), c.price, c.appraisal_code, \
                    c.appraisal_buy, c.appraisal_note, c.status, \
                    c.status = 'outstanding' AND c.date_expired < now(), c.reward, \
                    c.appraisal_problem \
             FROM contracts c LEFT JOIN names i ON i.id = c.issuer_id \
             LEFT JOIN locations s ON s.id = c.start_location \
             ORDER BY c.date_issued DESC LIMIT {LISTED}"
        ),
        &[],
    )
    .map_err(|e| failed("reading contracts", e))?;
    let counts = storage::query(
        "SELECT count(*) FILTER (WHERE status = 'outstanding' AND (date_expired IS NULL OR date_expired > now())), \
                count(*) FILTER (WHERE status IN ('finished', 'finished_issuer', 'finished_contractor') \
                    AND date_completed > now() - interval '7 days') \
         FROM contracts",
        &[],
    )
    .map_err(|e| failed("counting contracts", e))?;
    let count = |i: usize| counts.rows.first().map_or(0, |r| int(r, i));
    let mut table = Table::new(vec![
        Column::numeric("Issued"),
        Column::text("Type"),
        Column::text("Issued by"),
        Column::text("Location"),
        Column::numeric("Price"),
        Column::numeric("Janice buy"),
        Column::text("Check"),
        Column::text("Status"),
    ])
    .title("Contracts assigned to the corporations")
    .empty("No contracts yet. Has an owner been added?");
    for r in &rows.rows {
        let price = float(r, 5);
        let kind = text(r, 1);
        table = table.row(vec![
            when(r, 0).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            card::kind_label(&kind).into(),
            character(int(r, 2), text(r, 3)).into(),
            text(r, 4).into(),
            if kind == "courier" {
                isk(float(r, 11))
            } else {
                isk(price)
            },
            opt_float(r, 7).map_or_else(|| "".into(), isk),
            if kind == "courier" {
                "".into()
            } else {
                match opt_text(r, 12) {
                    Some(_) if opt_float(r, 7).is_some() => {
                        badge("Doesn't vouch", Tone::Danger).into()
                    }
                    _ => check_badge(
                        price,
                        opt_text(r, 6).as_deref(),
                        opt_float(r, 7),
                        opt_text(r, 8).as_deref(),
                        settings.tolerance,
                    ),
                }
            },
            status_badge(
                &text(r, 9),
                r.get(10).and_then(Db::as_bool).unwrap_or(false),
            ),
        ]);
    }
    let mut page = Page::new("Contracts")
        .description(format!(
            "Contracts assigned to the owners' corporations, read every five minutes. Prices are \
             checked against the Janice appraisal a description links, within {}%.",
            settings.tolerance
        ))
        .stats(vec![
            Stat::new("Outstanding", count(0)),
            Stat::new("Completed", count(1)).caption("in the last 7 days"),
            Stat::new(
                "Last read",
                settings
                    .synced_at
                    .map_or_else(|| Value::from("not yet"), |t| time(rfc3339(t))),
            ),
        ]);
    // Settings open from the app's Administration page (Tether's own
    // Settings button), not from here.
    if let Some(error) = settings.sync_error {
        page = page.text(format!("The last sync had a problem: {error}"));
    }
    Ok(page.table(table))
}

fn settings_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let mut channels: Vec<(String, String)> = vec![(String::new(), "Not sent".to_owned())];
    channels.extend(
        discord::channels()
            .into_iter()
            .map(|c| (c.id, format!("#{}", c.name))),
    );
    let current = settings
        .channel
        .clone()
        .filter(|c| channels.iter().any(|(id, _)| id == c))
        .unwrap_or_default();
    Ok(Page::new("Contracts settings")
        .description(
            "The Janice API key is entered by an admin as this app's secret (Administration, Apps, \
             Contracts). Without it, contracts are still posted, their appraisal marked not checked.",
        )
        .link("Contracts", "")
        .form(
            Form::new("settings", "Save")
                .field(
                    Field::select("channel", "Post contracts to", channels)
                        .value(current)
                        .help("A channel an admin assigned this app on the Discord page."),
                )
                .field(Field::checkbox(
                    "notify_new",
                    "New contracts assigned to the corporation",
                    settings.notify_new,
                ))
                .field(Field::checkbox(
                    "notify_completed",
                    "Completed contracts",
                    settings.notify_completed,
                ))
                .field(Field::checkbox(
                    "notify_ended",
                    "Expired, rejected, cancelled or deleted contracts",
                    settings.notify_ended,
                ))
                .field(
                    Field::number("tolerance_percent", "A price matches its appraisal within (%)")
                        .range(Some(0.0), Some(100.0), false)
                        .value(settings.tolerance.to_string())
                        .help("How far a contract's price may be from the appraisal's buy total. 1 by default; 0 is exact.")
                        .required(),
                ),
        ))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !viewer.can("manage") {
        return Err(PageError::Forbidden);
    }
    let assigned: Vec<String> = discord::channels().into_iter().map(|c| c.id).collect();
    let value = submission.value("channel");
    let channel = assigned.iter().find(|id| id.as_str() == value).cloned();
    let tolerance = submission
        .value("tolerance_percent")
        .parse::<f64>()
        .ok()
        .filter(|t| t.is_finite() && (0.0..=100.0).contains(t))
        .ok_or_else(|| PageError::Failed("tolerance_percent wasn't a number".into()))?;
    storage::execute(
        "UPDATE settings SET channel = $1, notify_new = $2, notify_completed = $3, \
         notify_ended = $4, tolerance_percent = $5 WHERE id = 1",
        &[
            channel.clone().into(),
            submission.checked("notify_new").into(),
            submission.checked("notify_completed").into(),
            submission.checked("notify_ended").into(),
            tolerance.into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    log::info(format!(
        "settings changed by {} ({}): channel {channel:?}, new {}, completed {}, ended {}, \
         within {tolerance}%",
        viewer.main.name,
        viewer.main.id,
        submission.checked("notify_new"),
        submission.checked("notify_completed"),
        submission.checked("notify_ended"),
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_ping_or_link() {
        assert_eq!(escape("@everyone [x](y)"), "\\@everyone \\[x\\]\\(y\\)");
        assert!(!escape("https://evil.example/sso").contains("://"));
    }

    #[test]
    fn stations_and_structures_by_id() {
        assert!(is_station(60003760));
        assert!(!is_station(1035466617946));
        assert!(is_structure(1035466617946));
    }
}
