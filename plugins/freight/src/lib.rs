//! Freight (aa-freight).
//!
//! - **Contract handler**: the data source (a character added with Add data source by a
//!   `setup_contract_handler` holder; chosen with the mode when there are
//!   several) whose corporation's courier contracts
//!   are read every ten minutes, kept as the operation mode says (aa-freight's
//!   four: contracts to the alliance from its members; to the corporation
//!   from its members, from the alliance's, or from anyone).
//! - **Pricing** (`manage`, aa-freight's admin site): routes between
//!   locations, one way or both, with a base price, a minimum, a price per
//!   m3 (with the handler's global modifier when a route uses it), a price
//!   per collateral percent, and limits on volume and collateral; the
//!   settings (the modifier, the Discord channels, the pilot notices'
//!   mention, announcing every contract).
//! - **Calculator** (`use_calculator`): a route's reward for a volume and a
//!   collateral, with how to issue the contract.
//! - **Contracts** (`view_contracts`): Active (the outstanding and
//!   in-progress ones) and All, each checked against its route's pricing. **My contracts**
//!   (`use_calculator`): the viewer's own, outstanding, in progress,
//!   finished or failed. **Statistics** (`view_statistics`): the last 90 days'
//!   finished contracts by route, pilot, pilot corporation and customer.
//! - **Locations** (`add_location`): stations by id (named from ESI) and
//!   structures by id and name (apps can't read structures).
//! - **Discord**: new contracts to the pilots' channel, mentioning the
//!   Discord role of the state Settings names (aa-freight's
//!   FREIGHT_DISCORD_MENTIONS, none by default; without a role mapped to
//!   that state they go unmentioned, and Settings says so), and each
//!   contract's status changes to the customers' channel (both for priced
//!   contracts only, unless every contract is announced), naming only the
//!   issuer and route and mentioning nobody (apps can't message people, as
//!   aa-freight's direct messages do).

mod card;
mod pricing;

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use pricing::{Pricing, for_route, thousands};
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Character, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, SettingsForm,
    SettingsGroup, Submission, SubmitResult, Table, Tone, Value, action, actions, badge, character,
    corporation, isk, link, log, time,
};

const SYNC: &str = "sync";
const RELAY: &str = "relay";
/// Public endpoints read no subject.
const PUBLIC: Subject = Subject::Character(0);
/// aa-freight's FREIGHT_HOURS_UNTIL_STALE_STATUS: older news isn't sent.
const STALE_HOURS: i64 = 24;
/// aa-freight's FREIGHT_STATISTICS_MAX_DAYS.
const STATISTICS_DAYS: i64 = 90;
/// The host's limit on Discord messages per run.
const SENDS_PER_RUN: usize = 5;
const RELAY_GAP_SECONDS: i64 = 15;
/// After Discord refused for now (down, or rate limited).
const RELAY_BACKOFF_SECONDS: i64 = 60;
const DISCORD_MAX: usize = 1_500;
const MAX_ROWS: i64 = 500;
/// All contracts' rows: with Active's, within a page's 10,000 values.
const ALL_ROWS: i64 = 400;
const MAX_PRICINGS: i64 = 100;
/// Locations are route ends in selects, which hold at most 100 options.
const MAX_LOCATIONS: i64 = 100;
/// aa-freight's operation modes: value, label, what it keeps.
const MODES: [(&str, &str, &str); 4] = [
    (
        "my_alliance",
        "My Alliance",
        "Contracts assigned to the handler's alliance by its members.",
    ),
    (
        "my_corporation",
        "My Corporation",
        "Contracts assigned to the handler's corporation by its members.",
    ),
    (
        "corp_in_alliance",
        "Corporation in My Alliance",
        "Contracts assigned to the handler's corporation by members of its alliance.",
    ),
    (
        "corp_public",
        "Corporation public",
        "Contracts assigned to the handler's corporation by anyone.",
    ),
];
/// Statuses a contract's issuer hears about, as aa-freight's customer
/// notifications.
const CUSTOMER_STATUSES: [&str; 4] = ["outstanding", "in_progress", "finished", "failed"];

struct Freight;

impl Plugin for Freight {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => index_page(&viewer, None),
            ["handler"] => {
                // The manifest's rule asks for it too.
                need(&viewer, "setup_contract_handler")?;
                handler_page()
            }
            ["mine"] => {
                // The manifest's rule asks for it too (aa-freight's).
                need(&viewer, "use_calculator")?;
                mine_page(&viewer)
            }
            ["contracts"] => {
                // The manifest's rule asks for it too: All is every
                // customer's contracts.
                need(&viewer, "view_contracts")?;
                contracts_page()
            }
            ["statistics"] => statistics_page(),
            ["locations"] => locations_page(None),
            ["pricing"] => pricing_page(None),
            ["pricing", id] => edit_page(number(id)?, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = submission.request.path.clone();
        match (path.as_str(), submission.form.as_str()) {
            ("", "calculate") => {
                need(&viewer, "use_calculator")?;
                Ok(SubmitResult::Page(index_page(
                    &viewer,
                    Some(calculate(&submission)?),
                )?))
            }
            ("handler", "mode") => set_mode(&viewer, &submission),
            ("locations", "add_location") => add_location(&viewer, &submission),
            ("locations", "delete_location") => delete_location(&viewer, &submission),
            ("pricing", "add_pricing") => match save_pricing(&viewer, None, &submission)? {
                Ok(()) => Ok(SubmitResult::Redirect("pricing".to_owned())),
                Err(problem) => Ok(SubmitResult::Page(pricing_page(Some((
                    &problem,
                    &submission,
                )))?)),
            },
            ("pricing", "settings") => save_settings(&viewer, &submission),
            (edit, "edit_pricing") | (edit, "delete_pricing") => {
                let id = edit
                    .strip_prefix("pricing/")
                    .ok_or(PageError::NotFound)
                    .and_then(number)?;
                if submission.form == "delete_pricing" {
                    return delete_pricing(&viewer, id);
                }
                match save_pricing(&viewer, Some(id), &submission)? {
                    Ok(()) => Ok(SubmitResult::Redirect("pricing".to_owned())),
                    Err(problem) => Ok(SubmitResult::Page(edit_page(
                        id,
                        Some((&problem, &submission)),
                    )?)),
                }
            }
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

tether_plugin_sdk::export!(Freight);

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

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
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

fn number(text: &str) -> Result<i64, PageError> {
    text.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

fn need(viewer: &Viewer, permission: &str) -> Result<(), PageError> {
    if viewer.can(permission) {
        Ok(())
    } else {
        Err(PageError::Forbidden)
    }
}

fn when_value(t: Option<DateTime<Utc>>) -> Value {
    t.map_or_else(|| "".into(), |t| time(rfc3339(t)))
}

/// A select starting on `value` when it's one of its options (the host
/// refuses a page whose select starts on anything else).
fn select(name: &str, label: &str, options: Vec<(String, String)>, value: &str) -> Field {
    let known = options.iter().any(|(option, _)| option == value);
    let field = Field::select(name, label, options);
    if known { field.value(value) } else { field }
}

fn m3(volume: f64) -> String {
    format!("{} m3", thousands(volume))
}

/// An optional number from a form: none when empty.
fn optional(submission: &Submission, name: &str) -> Result<Option<f64>, String> {
    let value = submission.value(name).trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map(Some)
        .ok_or_else(|| format!("{name} isn't a number"))
}

/// Cut to Discord's limit, on a character boundary.
fn clip(message: String) -> String {
    if message.chars().count() <= DISCORD_MAX {
        return message;
    }
    let mut out: String = message.chars().take(DISCORD_MAX - 1).collect();
    out.push('…');
    out
}

/// Discord markdown out of names players choose.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        // No forged lines.
        if c.is_control() {
            out.push(' ');
            continue;
        }
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '<' | '@'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn status_badge(status: &str) -> Value {
    let (label, tone) = match status {
        "outstanding" => ("Outstanding", Tone::Accent),
        "in_progress" => ("In progress", Tone::Warning),
        "finished" | "finished_issuer" | "finished_contractor" => ("Finished", Tone::Success),
        "failed" => ("Failed", Tone::Danger),
        "cancelled" => ("Cancelled", Tone::Neutral),
        "rejected" => ("Rejected", Tone::Neutral),
        "deleted" => ("Deleted", Tone::Neutral),
        "reversed" => ("Reversed", Tone::Neutral),
        _ => ("Unknown", Tone::Neutral),
    };
    badge(label, tone).into()
}

// ---- settings, locations and the handler -------------------------------------

struct Settings {
    mode: String,
    modifier: Option<f64>,
    pilot_channel: Option<String>,
    customer_channel: Option<String>,
    notify_all: bool,
    synced_at: Option<DateTime<Utc>>,
    sync_error: Option<String>,
    handler_id: Option<i64>,
    /// aa-freight's FREIGHT_DISCORD_MENTIONS: the state whose Discord role
    /// pilot notices mention.
    pilot_ping: Option<String>,
    /// When the last pilot notice went out without that mention.
    pilot_ping_refused: Option<DateTime<Utc>>,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT operation_mode, price_per_volume_modifier, pilot_channel, customer_channel, \
             notify_all, synced_at, sync_error, handler_id, pilot_ping, pilot_ping_refused \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first().cloned().unwrap_or_default();
    let optional_text = |i: usize| {
        row.get(i)
            .and_then(Db::as_text)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    Ok(Settings {
        mode: optional_text(0).unwrap_or_else(|| MODES[0].0.to_owned()),
        modifier: row.get(1).and_then(Db::as_float),
        pilot_channel: optional_text(2),
        customer_channel: optional_text(3),
        notify_all: row.get(4).and_then(Db::as_bool).unwrap_or(false),
        synced_at: when(&row, 5),
        sync_error: optional_text(6),
        handler_id: row.get(7).and_then(Db::as_integer),
        pilot_ping: optional_text(8),
        pilot_ping_refused: when(&row, 9),
    })
}

fn mode_label(mode: &str) -> &'static str {
    MODES
        .iter()
        .find(|(value, _, _)| *value == mode)
        .map_or("Unknown", |(_, label, _)| label)
}

/// The contract handler (aa-freight has one): the owner chosen with the
/// operation mode, else the only owner there is. Never whichever sorts
/// first: adding an owner mustn't change whose contracts are read.
fn handler(settings: &Settings) -> Option<Character> {
    let sources = esi::data_sources();
    match settings.handler_id {
        Some(id) => sources.into_iter().find(|s| s.id == id),
        None if sources.len() == 1 => sources.into_iter().next(),
        None => None,
    }
}

/// What contracts are assigned to in this mode: the handler's alliance or
/// corporation.
fn organization(handler: &Character, mode: &str) -> Option<i64> {
    if mode == "my_alliance" {
        handler.alliance_id
    } else {
        Some(handler.corporation_id)
    }
}

/// The locations added (on Locations): what routes start and end at.
fn location_names() -> Result<Vec<(i64, String)>, storage::Error> {
    let rows = storage::query(
        "SELECT id, name FROM locations ORDER BY name LIMIT $1",
        &[MAX_LOCATIONS.into()],
    )?;
    Ok(rows.rows.iter().map(|r| (int(r, 0), text(r, 1))).collect())
}

/// Names for every place a contract names: the locations added, and the
/// stations ESI named (structures only when added).
fn places() -> Result<Vec<(i64, String)>, storage::Error> {
    let rows = storage::query(
        "SELECT id, name FROM locations UNION ALL \
         SELECT id, name FROM names WHERE id BETWEEN 60000000 AND 63999999 \
             AND id NOT IN (SELECT id FROM locations)",
        &[],
    )?;
    Ok(rows.rows.iter().map(|r| (int(r, 0), text(r, 1))).collect())
}

fn place(names: &[(i64, String)], id: i64) -> String {
    names
        .iter()
        .find(|(known, _)| *known == id)
        .map_or_else(|| format!("Location {id}"), |(_, name)| name.clone())
}

fn route_label(names: &[(i64, String)], p: &Pricing) -> String {
    format!(
        "{} {} {}",
        place(names, p.start),
        if p.bidirectional { "⇄" } else { "→" },
        place(names, p.end)
    )
}

fn name_of(id: i64) -> String {
    storage::query("SELECT name FROM names WHERE id = $1", &[id.into()])
        .ok()
        .and_then(|r| r.rows.first().map(|row| text(row, 0)))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_string())
}

// ---- the sync ------------------------------------------------------------------

/// The handler's corporation's courier contracts, as the operation mode
/// keeps them; then locations, names and the Discord notices.
fn sync() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let sources = esi::data_sources();
    if sources.is_empty() {
        return Ok(());
    }
    let Some(handler) = handler(&settings) else {
        return sync_failed(if settings.handler_id.is_some() {
            "The contract handler is no longer a data source: choose another under Operation mode."
        } else {
            "There are several data sources: choose the contract handler under Operation mode."
        });
    };
    if settings.handler_id.is_none() {
        // The only owner: kept as the handler, so adding another later
        // doesn't change it.
        storage::execute(
            "UPDATE settings SET handler_id = $1 WHERE id = 1 AND handler_id IS NULL",
            &[handler.id.into()],
        )
        .map_err(|e| retry("keeping the handler", e))?;
    }
    let Some(organization) = organization(&handler, &settings.mode) else {
        return sync_failed("The contract handler is in no alliance: choose a corporation mode.");
    };
    let bodies = match esi::get_all(
        "corporation-contracts",
        Subject::DataSource(handler.id),
        &[],
    ) {
        Ok(bodies) => bodies,
        Err(err) => return sync_failed(&format!("contracts not read: {err:?}")),
    };
    let mut all: Vec<serde_json::Value> = Vec::new();
    for body in bodies {
        let page: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        all.extend(page.as_array().cloned().unwrap_or_default());
    }
    let couriers: Vec<serde_json::Value> = all
        .into_iter()
        .filter(|c| c["type"].as_str() == Some("courier"))
        .filter(|c| c["assignee_id"].as_i64() == Some(organization))
        .collect();
    let kept = keep(&settings.mode, &handler, couriers)?;
    store(&kept)?;
    // Names for people, corporations and stations (/universe/names names
    // stations; structures are added by hand).
    let mut ids: Vec<i64> = Vec::new();
    for c in &kept {
        for field in [
            "issuer_id",
            "issuer_corporation_id",
            "acceptor_id",
            "start_location_id",
            "end_location_id",
        ] {
            if let Some(id) = c[field].as_i64() {
                ids.push(id);
            }
        }
    }
    ids.push(organization);
    let pilots = storage::query(
        "SELECT DISTINCT acceptor_corporation_id FROM contracts \
         WHERE acceptor_corporation_id IS NOT NULL",
        &[],
    )
    .map_err(|e| retry("reading pilot corporations", e))?;
    ids.extend(pilots.rows.iter().map(|r| int(r, 0)));
    learn_names(&ids)?;
    storage::execute(
        "UPDATE settings SET synced_at = now(), sync_error = NULL WHERE id = 1",
        &[],
    )
    .map_err(|e| retry("noting the sync", e))?;
    notify(&settings)?;
    Ok(())
}

fn sync_failed(why: &str) -> Result<(), JobError> {
    log::warn(why);
    storage::execute(
        "UPDATE settings SET sync_error = $1 WHERE id = 1",
        &[why.into()],
    )
    .map_err(|e| retry("noting a sync error", e))?;
    Ok(())
}

/// The contracts the operation mode keeps (all are courier contracts
/// assigned to its organization already). In both alliance modes, as
/// aa-freight's: those whose issuer is in the handler's alliance
/// (`freight/models/contract_handlers.py:343-350`); a contract kept once is
/// followed to its end, though its issuer leaves the alliance meanwhile.
fn keep(
    mode: &str,
    handler: &Character,
    couriers: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, JobError> {
    match mode {
        "my_corporation" => Ok(couriers
            .into_iter()
            .filter(|c| c["issuer_corporation_id"].as_i64() == Some(handler.corporation_id))
            .collect()),
        "my_alliance" | "corp_in_alliance" => {
            let Some(alliance) = handler.alliance_id else {
                return Ok(Vec::new());
            };
            let issuers: Vec<i64> = couriers
                .iter()
                .filter_map(|c| c["issuer_id"].as_i64())
                .collect();
            let members: Vec<i64> = affiliations(&issuers)?
                .into_iter()
                .filter(|(_, _, a)| *a == Some(alliance))
                .map(|(character, _, _)| character)
                .collect();
            let kept = stored(&couriers)?;
            Ok(couriers
                .into_iter()
                .filter(|c| from_member(c, &members, &kept))
                .collect())
        }
        // corp_public: everything assigned.
        _ => Ok(couriers),
    }
}

/// Whether a contract's issuer is one of the alliance's `members`, or the
/// contract is one already kept.
fn from_member(c: &serde_json::Value, members: &[i64], kept: &[i64]) -> bool {
    c["issuer_id"]
        .as_i64()
        .is_some_and(|id| members.contains(&id))
        || c["contract_id"]
            .as_i64()
            .is_some_and(|id| kept.contains(&id))
}

/// Which of these contracts are stored already.
fn stored(contracts: &[serde_json::Value]) -> Result<Vec<i64>, JobError> {
    let list = contracts
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows = storage::query(
        "SELECT contract_id FROM contracts \
         WHERE contract_id = ANY(string_to_array($1, ',')::bigint[])",
        &[list.into()],
    )
    .map_err(|e| retry("reading stored contracts", e))?;
    Ok(rows.rows.iter().map(|r| int(r, 0)).collect())
}

/// Characters' corporations and alliances now, from ESI's public
/// affiliation (a thousand at a time).
fn affiliations(ids: &[i64]) -> Result<Vec<(i64, i64, Option<i64>)>, JobError> {
    let mut ids: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = Vec::new();
    for chunk in ids.chunks(1000) {
        let ids = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let answer = esi::get(
            "character-affiliation",
            PUBLIC,
            &[("character_ids".to_owned(), ids)],
            None,
        )
        .map_err(|e| retry("reading affiliations", e))?;
        let list: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_default();
        out.extend(list.as_array().into_iter().flatten().filter_map(|a| {
            Some((
                a["character_id"].as_i64()?,
                a["corporation_id"].as_i64()?,
                a["alliance_id"].as_i64(),
            ))
        }));
    }
    Ok(out)
}

/// Pilots' corporations, for the statistics (aa-freight's pilot
/// corporations), of the acceptors of contracts whose isn't stored yet:
/// who accepted a contract isn't told with its corporation. A contract
/// accepted by a corporation names the corporation, which is its own (as
/// aa-freight); only characters go to ESI's affiliation, which refuses a
/// whole batch for one id that isn't a character's.
fn acceptor_corporations(contracts: &[serde_json::Value]) -> Vec<(i64, i64)> {
    let accepted: Vec<(i64, i64)> = contracts
        .iter()
        .filter_map(|c| {
            Some((
                c["contract_id"].as_i64()?,
                c["acceptor_id"].as_i64().filter(|id| *id > 0)?,
            ))
        })
        .collect();
    if accepted.is_empty() {
        return Vec::new();
    }
    let list = accepted
        .iter()
        .map(|(contract, _)| contract.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let done: Vec<i64> = match storage::query(
        "SELECT contract_id FROM contracts WHERE acceptor_corporation_id IS NOT NULL \
         AND contract_id = ANY(string_to_array($1, ',')::bigint[])",
        &[list.into()],
    ) {
        Ok(found) => found.rows.iter().map(|r| int(r, 0)).collect(),
        Err(err) => {
            log::info(format!("pilot corporations not read: {err:?}"));
            return Vec::new();
        }
    };
    let mut wanted: Vec<i64> = accepted
        .into_iter()
        .filter(|(contract, _)| !done.contains(contract))
        .map(|(_, acceptor)| acceptor)
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::new();
    let mut characters = Vec::new();
    for chunk in wanted.chunks(1000) {
        match esi::names(chunk) {
            Ok(named) => {
                for n in named.into_iter().filter(|n| chunk.contains(&n.id)) {
                    match n.category.as_str() {
                        "corporation" => out.push((n.id, n.id)),
                        "character" => characters.push(n.id),
                        _ => {}
                    }
                }
            }
            // Not told which is which: asked as characters, as before.
            Err(err) => {
                log::info(format!("acceptors not named: {}", esi::describe(&err)));
                characters.extend_from_slice(chunk);
            }
        }
    }
    if !characters.is_empty() {
        match affiliations(&characters) {
            Ok(found) => out.extend(
                found
                    .into_iter()
                    .map(|(c, corporation, _)| (c, corporation)),
            ),
            Err(why) => log::info(format!("pilot corporations not read: {why:?}")),
        }
    }
    out
}

fn store(contracts: &[serde_json::Value]) -> Result<(), JobError> {
    let corporations = acceptor_corporations(contracts);
    let corporation_of = |id: Option<i64>| {
        corporations
            .iter()
            .find(|(acceptor, _)| Some(*acceptor) == id)
            .map(|(_, corporation)| *corporation)
    };
    let rows: Vec<serde_json::Value> = contracts
        .iter()
        .filter_map(|c| {
            Some(serde_json::json!({
                "contract_id": c["contract_id"].as_i64()?,
                "issuer_id": c["issuer_id"].as_i64()?,
                "issuer_corporation_id": c["issuer_corporation_id"].as_i64()?,
                "acceptor_id": c["acceptor_id"].as_i64().filter(|id| *id > 0),
                "acceptor_corporation_id": corporation_of(c["acceptor_id"].as_i64()),
                "start_location": c["start_location_id"].as_i64()?,
                "end_location": c["end_location_id"].as_i64()?,
                "status": c["status"].as_str()?,
                "volume": c["volume"].as_f64().unwrap_or(0.0),
                "reward": c["reward"].as_f64().unwrap_or(0.0),
                "collateral": c["collateral"].as_f64().unwrap_or(0.0),
                "days_to_complete": c["days_to_complete"].as_i64(),
                "date_issued": c["date_issued"].as_str()?,
                "date_expired": c["date_expired"].as_str(),
                "date_accepted": c["date_accepted"].as_str(),
                "date_completed": c["date_completed"].as_str(),
                "title": c["title"].as_str().unwrap_or_default().chars().take(200).collect::<String>(),
            }))
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    storage::execute(
        "INSERT INTO contracts (contract_id, issuer_id, issuer_corporation_id, acceptor_id, \
             acceptor_corporation_id, start_location, end_location, status, volume, reward, \
             collateral, days_to_complete, date_issued, date_expired, date_accepted, \
             date_completed, title) \
         SELECT DISTINCT ON (contract_id) contract_id, issuer_id, issuer_corporation_id, \
             acceptor_id, acceptor_corporation_id, start_location, end_location, status, volume, \
             reward, collateral, days_to_complete, date_issued, date_expired, date_accepted, \
             date_completed, title \
         FROM json_to_recordset($1::json) AS x(contract_id bigint, issuer_id bigint, \
             issuer_corporation_id bigint, acceptor_id bigint, acceptor_corporation_id bigint, \
             start_location bigint, end_location bigint, status text, volume double precision, \
             reward double precision, collateral double precision, days_to_complete integer, \
             date_issued timestamptz, date_expired timestamptz, date_accepted timestamptz, \
             date_completed timestamptz, title text) \
         ON CONFLICT (contract_id) DO UPDATE SET acceptor_id = EXCLUDED.acceptor_id, \
             acceptor_corporation_id = coalesce(contracts.acceptor_corporation_id, \
                 EXCLUDED.acceptor_corporation_id), \
             status = EXCLUDED.status, date_accepted = EXCLUDED.date_accepted, \
             date_completed = EXCLUDED.date_completed, date_expired = EXCLUDED.date_expired, \
             updated_at = CASE WHEN contracts.status IS DISTINCT FROM EXCLUDED.status \
                 THEN now() ELSE contracts.updated_at END",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map_err(|e| retry("storing contracts", e))?;
    Ok(())
}

/// A station's name and its system's, from ESI.
fn station(id: i64) -> Result<(String, String), String> {
    let answer = esi::get(
        "universe-station",
        PUBLIC,
        &[("station_id".to_owned(), id.to_string())],
        None,
    )
    .map_err(|e| format!("{e:?}"))?;
    let station: serde_json::Value =
        serde_json::from_str(&answer.body).map_err(|e| e.to_string())?;
    let name = station["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or("no name")?
        .chars()
        .take(200)
        .collect::<String>();
    let system = station["system_id"]
        .as_i64()
        .and_then(|system| esi::names(&[system]).ok())
        .and_then(|named| named.into_iter().next())
        .map(|n| n.name)
        .unwrap_or_default();
    Ok((name, system))
}

/// Names for ids not named yet, a thousand at a time.
fn learn_names(ids: &[i64]) -> Result<(), JobError> {
    // Structures aren't in /universe/names.
    let mut ids: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| *id > 0 && *id < 1_000_000_000_000)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let known = storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[list.into()],
    )
    .map_err(|e| retry("reading names", e))?;
    let known: Vec<i64> = known.rows.iter().map(|r| int(r, 0)).collect();
    let missing: Vec<i64> = ids.into_iter().filter(|id| !known.contains(id)).collect();
    for chunk in missing.chunks(1000) {
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

// ---- Discord ---------------------------------------------------------------------

/// A stored contract, for notices and pages.
struct Contract {
    id: i64,
    issuer: i64,
    issuer_corporation: i64,
    acceptor: Option<i64>,
    start: i64,
    end: i64,
    status: String,
    volume: f64,
    reward: f64,
    collateral: f64,
    days_to_complete: Option<i64>,
    issued: Option<DateTime<Utc>>,
    expires: Option<DateTime<Utc>>,
    title: String,
}

const CONTRACT_COLUMNS: &str = "contract_id, issuer_id, issuer_corporation_id, acceptor_id, \
     start_location, end_location, status, volume, reward, collateral, days_to_complete, \
     date_issued, date_expired, title";

fn contracts(where_clause: &str, params: &[Db]) -> Result<Vec<Contract>, storage::Error> {
    contracts_up_to(where_clause, params, MAX_ROWS)
}

/// The newest `limit` contracts `where_clause` picks.
fn contracts_up_to(
    where_clause: &str,
    params: &[Db],
    limit: i64,
) -> Result<Vec<Contract>, storage::Error> {
    let rows = storage::query(
        &format!(
            "SELECT {CONTRACT_COLUMNS} FROM contracts {where_clause} \
             ORDER BY date_issued DESC LIMIT {limit}"
        ),
        params,
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Contract {
            id: int(r, 0),
            issuer: int(r, 1),
            issuer_corporation: int(r, 2),
            acceptor: r.get(3).and_then(Db::as_integer),
            start: int(r, 4),
            end: int(r, 5),
            status: text(r, 6),
            volume: float(r, 7),
            reward: float(r, 8),
            collateral: float(r, 9),
            days_to_complete: r.get(10).and_then(Db::as_integer),
            issued: when(r, 11),
            expires: when(r, 12),
            title: text(r, 13),
        })
        .collect())
}

impl Contract {
    /// Outstanding past its expiry: aa-freight's "expired".
    fn expired(&self, now: DateTime<Utc>) -> bool {
        self.status == "outstanding" && self.expires.is_some_and(|t| t < now)
    }

    /// The pricing check: none without a pricing, else its issues.
    fn check(&self, pricings: &[Pricing], modifier: Option<f64>) -> Option<Vec<String>> {
        for_route(pricings, self.start, self.end)
            .map(|p| p.issues(self.volume, self.collateral, Some(self.reward), modifier))
    }

    fn route(&self, names: &[(i64, String)]) -> String {
        format!("{} → {}", place(names, self.start), place(names, self.end))
    }
}

fn check_badge(check: Option<&Vec<String>>) -> Value {
    match check {
        None => badge("No pricing", Tone::Neutral).into(),
        Some(issues) if issues.is_empty() => badge("OK", Tone::Success).into(),
        Some(issues) => badge(issues.join("; "), Tone::Danger).into(),
    }
}

fn check_line(check: Option<&Vec<String>>) -> String {
    match check {
        None => "No pricing for this route".to_owned(),
        Some(issues) if issues.is_empty() => "Contract check: OK".to_owned(),
        Some(issues) => format!("Contract check: {}", issues.join("; ")),
    }
}

/// Queues aa-freight's pilot and customer notices, then relays them.
fn notify(settings: &Settings) -> Result<(), JobError> {
    let pricings = pricing::all().map_err(|e| retry("reading pricings", e))?;
    let names = places().map_err(|e| retry("reading locations", e))?;
    let now = Utc::now();
    let stale = rfc3339(now - Duration::hours(STALE_HOURS));
    let mut queued = false;

    // Pilots: new outstanding contracts, once each.
    let fresh = contracts(
        "WHERE status = 'outstanding' AND notified_at IS NULL AND date_issued > $1::timestamptz",
        &[Db::timestamp(stale.clone())],
    )
    .map_err(|e| retry("reading new contracts", e))?;
    for c in fresh.iter().filter(|c| !c.expired(now)) {
        let check = c.check(&pricings, settings.modifier);
        let send = settings
            .pilot_channel
            .as_ref()
            .filter(|_| check.is_some() || settings.notify_all);
        // Claimed and queued in one statement, so two syncs at once can't
        // both announce it. Every pilot notice carries the mention, as
        // aa-freight's.
        if let Some(channel) = send {
            let added = storage::execute(
                "WITH claimed AS (UPDATE contracts SET notified_at = now() \
                     WHERE contract_id = $1 AND notified_at IS NULL RETURNING 1) \
                 INSERT INTO outbox (channel, message, card, mention_state) \
                 SELECT $2, $3, $4, $5 FROM claimed",
                &[
                    c.id.into(),
                    channel.as_str().into(),
                    pilot_message(c, &names, check.as_ref()).into(),
                    pilot_card(c, &names, check.as_ref()),
                    settings.pilot_ping.clone().into(),
                ],
            )
            .map_err(|e| retry("queuing a pilot notice", e))?;
            queued |= added > 0;
        } else if check.is_some() {
            storage::execute(
                "UPDATE contracts SET notified_at = now() \
                 WHERE contract_id = $1 AND notified_at IS NULL",
                &[c.id.into()],
            )
            .map_err(|e| retry("noting a pilot notice", e))?;
        }
        // Unpriced and not announced: a pricing added later may still
        // announce it while it's fresh.
    }

    // Customers: each status once, while it's news (by when it happened,
    // so a first sync doesn't announce old deliveries).
    if let Some(channel) = &settings.customer_channel {
        let news = contracts(
            "WHERE status = ANY(string_to_array($1, ',')) \
             AND coalesce(CASE status WHEN 'outstanding' THEN date_issued \
                 WHEN 'in_progress' THEN date_accepted ELSE date_completed END, updated_at) \
                 > $2::timestamptz \
             AND NOT EXISTS (SELECT 1 FROM customer_notices n \
                 WHERE n.contract_id = contracts.contract_id AND n.status = contracts.status)",
            &[CUSTOMER_STATUSES.join(",").into(), Db::timestamp(stale)],
        )
        .map_err(|e| retry("reading status changes", e))?;
        for c in news.iter().filter(|c| !c.expired(now)) {
            let check = c.check(&pricings, settings.modifier);
            // Priced contracts only, unless every contract is announced
            // (aa-freight's FREIGHT_NOTIFY_ALL_CONTRACTS,
            // `freight/managers.py:428-429`). Not noted: a pricing added
            // while it's news still tells its customer.
            if check.is_none() && !settings.notify_all {
                continue;
            }
            let added = storage::execute(
                "WITH noticed AS (INSERT INTO customer_notices (contract_id, status) \
                     VALUES ($1, $2) ON CONFLICT DO NOTHING RETURNING 1) \
                 INSERT INTO outbox (channel, message, card) SELECT $3, $4, $5 FROM noticed",
                &[
                    c.id.into(),
                    c.status.as_str().into(),
                    channel.as_str().into(),
                    customer_message(c, &names, check.as_ref()).into(),
                    customer_card(c, &names, check.as_ref()),
                ],
            )
            .map_err(|e| retry("queuing a customer notice", e))?;
            queued |= added > 0;
        }
    }
    if queued {
        relay()?;
    }
    Ok(())
}

fn pilot_message(c: &Contract, names: &[(i64, String)], check: Option<&Vec<String>>) -> String {
    let mut lines = vec![
        format!(
            "**New courier contract** from {} ({}) looking to be picked up",
            escape(&name_of(c.issuer)),
            escape(&name_of(c.issuer_corporation))
        ),
        escape(&c.route(names)),
        format!(
            "Reward {} ISK · Collateral {} ISK · Volume {}",
            thousands(c.reward),
            thousands(c.collateral),
            m3(c.volume)
        ),
    ];
    if let Some(expires) = c.expires {
        lines.push(format!(
            "Expires {} · {} days to complete",
            expires.format("%Y-%m-%d %H:%M EVE"),
            c.days_to_complete.unwrap_or_default()
        ));
    }
    if !c.title.is_empty() {
        lines.push(format!("Note: {}", escape(&c.title)));
    }
    lines.push(check_line(check));
    clip(lines.join("\n"))
}

/// A contract check as a card field, and the card's colour for it.
fn check_field(check: Option<&Vec<String>>) -> (String, u32) {
    match check {
        None => ("No pricing for this route".to_owned(), card::BLUE),
        Some(issues) if issues.is_empty() => ("OK".to_owned(), card::GREEN),
        Some(issues) => (escape(&issues.join("; ")), card::ORANGE),
    }
}

/// The issuer, as a card's author: "Pilot (Corporation)".
fn issuer(c: &Contract) -> (String, i64) {
    (
        format!("{} ({})", name_of(c.issuer), name_of(c.issuer_corporation)),
        c.issuer,
    )
}

fn pilot_card(c: &Contract, names: &[(i64, String)], check: Option<&Vec<String>>) -> Db {
    let (check, color) = check_field(check);
    let mut fields = vec![
        ("Reward", format!("{} ISK", thousands(c.reward)), true),
        (
            "Collateral",
            format!("{} ISK", thousands(c.collateral)),
            true,
        ),
        ("Volume", m3(c.volume), true),
    ];
    if let Some(expires) = c.expires {
        fields.push((
            "Expires",
            format!(
                "{} · <t:{}:R>",
                expires.format("%Y-%m-%d %H:%M EVE"),
                expires.timestamp()
            ),
            true,
        ));
        fields.push((
            "Days to complete",
            c.days_to_complete.unwrap_or_default().to_string(),
            true,
        ));
    }
    fields.push(("Note", escape(&c.title), false));
    fields.push(("Contract check", check, false));
    Db::json(
        card::Card {
            title: "New courier contract",
            description: escape(&c.route(names)),
            color,
            author: issuer(c),
            fields,
            timestamp: c.issued.map(rfc3339),
        }
        .json()
        .to_string(),
    )
}

/// A customer's card: no more than their message says.
fn customer_card(c: &Contract, names: &[(i64, String)], check: Option<&Vec<String>>) -> Db {
    let (title, color) = match c.status.as_str() {
        "outstanding" => ("Contract waiting to be picked up", card::BLUE),
        "in_progress" => ("Contract accepted", card::BLUE),
        "finished" => ("Contract delivered", card::GREEN),
        _ => ("Contract failed", card::RED),
    };
    let mut fields = Vec::new();
    if c.status == "outstanding" {
        fields.push(("Contract check", check_field(check).0, false));
    }
    Db::json(
        card::Card {
            title,
            description: customer_message(c, names, None)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
            color,
            author: issuer(c),
            fields,
            timestamp: None,
        }
        .json()
        .to_string(),
    )
}

/// A customer's notice. aa-freight sends these as direct messages; here
/// they go to a shared channel, so they say no more than a customer needs:
/// not the collateral, the cargo or who is hauling it (a hauler's route
/// and collateral, told to a channel, is a gank target).
fn customer_message(c: &Contract, names: &[(i64, String)], check: Option<&Vec<String>>) -> String {
    let what = match c.status.as_str() {
        "outstanding" => "is waiting to be picked up",
        "in_progress" => "has been accepted",
        "finished" => "has been delivered",
        _ => "has failed",
    };
    let mut lines = vec![format!(
        "**{}**: your courier contract {} {what}",
        escape(&name_of(c.issuer)),
        escape(&c.route(names))
    )];
    if c.status == "outstanding" {
        lines.push(check_line(check));
    }
    clip(lines.join("\n"))
}

/// Sends up to the host's limit, then comes back for the rest.
fn relay() -> Result<(), JobError> {
    // Stale news isn't sent (as aa-freight), e.g. after Discord was down.
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND queued_at < now() - make_interval(hours => $1::int)",
        &[(STALE_HOURS as i32).into()],
    )
    .map_err(|e| retry("expiring messages", e))?;
    let mut gap = RELAY_GAP_SECONDS;
    let waiting = storage::query(
        "SELECT id, channel, message, card::text, mention_state FROM outbox \
         WHERE sent_at IS NULL AND failed IS NULL ORDER BY id LIMIT $1",
        &[(SENDS_PER_RUN as i64 + 1).into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    let mut sends = 0;
    for row in waiting.rows.iter().take(SENDS_PER_RUN) {
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
        let card = row
            .get(3)
            .and_then(Db::as_text)
            .and_then(|c| serde_json::from_str(c).ok())
            .and_then(|c| card::embed(&c));
        let (channel, message) = (text(row, 1), text(row, 2));
        let post = |mention: Mention| match &card {
            Some(card) => discord::send_embed(&channel, card, mention),
            None => discord::send(&channel, &message, mention),
        };
        let mention = row.get(4).and_then(Db::as_text).map(str::to_owned);
        sends += 1;
        let mut sent = post(mention.clone().map_or(Mention::None, Mention::State));
        // Refused with its mention: maybe no Discord role is mapped to the
        // state, so it goes without (as Structures' pings). Past the host's
        // limit for a run, the second try is rate limited and the message
        // released for later, never failed.
        let mut unmentioned = false;
        if mention.is_some() && matches!(sent, Err(discord::Error::NotAllowed(_))) {
            sends += 1;
            sent = post(Mention::None);
            unmentioned = true;
        }
        match sent {
            Ok(()) => {
                // Sent: whether the mention went with it, known now (the
                // host checks the channel before the mention).
                if let Some(state) = &mention {
                    if unmentioned {
                        log::warn(format!(
                            "a pilot notice went out without mentioning {state}: no Discord \
                             role is mapped to it"
                        ));
                    }
                    storage::execute(
                        "UPDATE settings SET pilot_ping_refused = CASE WHEN $2 THEN now() END \
                         WHERE id = 1 AND pilot_ping = $1",
                        &[state.as_str().into(), unmentioned.into()],
                    )
                    .map_err(|e| retry("noting the mention", e))?;
                }
            }
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

// ---- pages -----------------------------------------------------------------------

/// What the calculator worked out.
struct Calculation {
    pricing: i64,
    volume: Option<f64>,
    collateral: Option<f64>,
    outcome: Result<f64, Vec<String>>,
}

fn calculate(submission: &Submission) -> Result<Calculation, PageError> {
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let id = submission
        .value("pricing")
        .parse::<i64>()
        .unwrap_or_default();
    let modifier = settings()
        .map_err(|e| failed("reading settings", e))?
        .modifier;
    let volume = optional(submission, "volume").unwrap_or(None);
    let collateral = optional(submission, "collateral").unwrap_or(None);
    let Some(p) = pricings.iter().find(|p| p.id == id && p.active) else {
        return Ok(Calculation {
            pricing: id,
            volume,
            collateral,
            outcome: Err(vec!["choose a route".to_owned()]),
        });
    };
    let mut issues = Vec::new();
    if p.requires_volume() && volume.is_none() {
        issues.push("this route needs a volume".to_owned());
    }
    if p.requires_collateral() && collateral.is_none() {
        issues.push("this route needs a collateral".to_owned());
    }
    if volume.is_some_and(|v| v < 0.0) || collateral.is_some_and(|c| c < 0.0) {
        issues.push("volume and collateral can't be negative".to_owned());
    }
    let (v, c) = (volume.unwrap_or(0.0), collateral.unwrap_or(0.0));
    if issues.is_empty() {
        issues = p.issues(v, c, None, modifier);
    }
    Ok(Calculation {
        pricing: id,
        volume,
        collateral,
        outcome: if issues.is_empty() {
            Ok(p.price(v, c, modifier))
        } else {
            Err(issues)
        },
    })
}

/// Who the courier contracts go to, and the character reading them; `here`
/// on the Contract handler page, where it's chosen.
fn handler_card(
    settings: &Settings,
    handler: Option<&Character>,
    sources: &[Character],
    here: bool,
) -> Card {
    let mut card = Card::new("Contract handler");
    match handler {
        Some(h) => {
            let assignee = organization(h, &settings.mode);
            card = card
                .field(
                    "Contracts to",
                    assignee.map_or_else(
                        || "No alliance".into(),
                        |id| {
                            if settings.mode == "my_alliance" {
                                Value::from(tether_plugin_sdk::alliance(id, name_of(id)))
                            } else {
                                corporation(id, name_of(id)).into()
                            }
                        },
                    ),
                )
                .field("Operation mode", mode_label(&settings.mode))
                .field("Character", character(h.id, h.name.clone()))
                .field("Last sync", when_value(settings.synced_at));
            if let Some(error) = &settings.sync_error {
                card = card.field("Problem", badge(error.clone(), Tone::Danger));
            }
        }
        None if !sources.is_empty() => {
            card = card.description(if here {
                "Choose which data source is the contract handler, under Operation mode."
            } else {
                "Not chosen yet: a holder of Setup contract handler picks one of the data sources under Manage, Contract handler."
            });
        }
        None => {
            card = card.description(
                "None yet: a holder of Setup contract handler adds one with Add data source, a director of the corporation the contracts are assigned to (with the contracts scope).",
            );
        }
    }
    card
}

/// aa-freight's contract handler: which data source's corporation's
/// courier contracts are read, and the operation mode, for
/// `setup_contract_handler`.
fn handler_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let sources = esi::data_sources();
    let handler = handler(&settings);
    let options = MODES
        .iter()
        .map(|(value, label, _)| ((*value).to_owned(), (*label).to_owned()))
        .collect();
    let help = MODES
        .iter()
        .map(|(_, label, what)| format!("{label}: {what}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut form = Form::new("mode", "Save").title("Operation mode");
    if !sources.is_empty() {
        let owners = sources
            .iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    format!("{} ({})", s.name, name_of(s.corporation_id)),
                )
            })
            .collect();
        let chosen = handler
            .as_ref()
            .map(|h| h.id.to_string())
            .unwrap_or_default();
        form = form.field(
            select("handler", "Contract handler", owners, &chosen)
                .required()
                .help("The data source whose corporation's contracts are read. Changing it, or the mode, starts the contracts afresh."),
        );
    }
    Ok(Page::new("Contract handler")
        .description("Who the courier contracts are assigned to, and the character reading them")
        .card(handler_card(&settings, handler.as_ref(), &sources, true))
        .form(
            form.field(
                select("mode", "Mode", options, &settings.mode)
                    .required()
                    .help(help),
            ),
        ))
}

fn index_page(viewer: &Viewer, calculation: Option<Calculation>) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let names = places().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let sources = esi::data_sources();
    let handler = handler(&settings);
    let mut page = Page::new("Freight")
        .description("A central freight service: priced routes and their courier contracts")
        .card(handler_card(&settings, handler.as_ref(), &sources, false));

    // The calculator.
    let active: Vec<&Pricing> = pricings.iter().filter(|p| p.active).collect();
    if viewer.can("use_calculator") {
        if active.is_empty() {
            page = page.text("No routes are priced yet.");
        } else {
            if let Some(calc) = &calculation {
                page = page.card(result_card(
                    calc,
                    &pricings,
                    &names,
                    &settings,
                    handler.as_ref(),
                ));
            }
            let options = active
                .iter()
                .map(|p| (p.id.to_string(), route_label(&names, p)))
                .collect();
            let chosen = calculation
                .as_ref()
                .map(|c| c.pricing.to_string())
                .unwrap_or_default();
            let number = |v: Option<f64>| v.map(|v| v.to_string()).unwrap_or_default();
            page = page.form(
                Form::new("calculate", "Calculate reward")
                    .title("Reward calculator")
                    .field(select("pricing", "Route", options, &chosen).required())
                    .field(
                        Field::number("volume", "Volume (m3)")
                            .range(Some(0.0), None, false)
                            .value(number(calculation.as_ref().and_then(|c| c.volume))),
                    )
                    .field(
                        Field::number("collateral", "Collateral (ISK)")
                            .range(Some(0.0), None, false)
                            .value(number(calculation.as_ref().and_then(|c| c.collateral))),
                    ),
            );
        }
    }

    // The routes.
    let mut routes = Table::new(vec![
        Column::text("Route"),
        Column::text("Price"),
        Column::text("Limits"),
        Column::text("Days"),
        Column::text("Details"),
    ])
    .title("Routes")
    .empty("No routes are priced yet.");
    for p in &active {
        routes = routes.row(vec![
            route_label(&names, p).into(),
            price_text(p, settings.modifier).into(),
            limits_text(p).into(),
            days_text(p).into(),
            p.details.clone().into(),
        ]);
    }
    Ok(page.table(routes))
}

fn price_text(p: &Pricing, modifier: Option<f64>) -> String {
    if let Some(base) = p.price_base.filter(|_| p.is_fix_price()) {
        return format!("{} ISK fixed", thousands(base));
    }
    let mut parts = Vec::new();
    if let Some(base) = p.price_base {
        parts.push(format!("{} ISK base", thousands(base)));
    }
    if let Some(per) = p.price_per_volume_eff(modifier) {
        parts.push(format!("{} ISK per m3", thousands(per)));
    }
    if let Some(percent) = p.price_per_collateral_percent.filter(|v| *v != 0.0) {
        parts.push(format!("{percent}% of collateral"));
    }
    if let Some(min) = p.price_min {
        parts.push(format!("at least {} ISK", thousands(min)));
    }
    parts.join(", ")
}

fn limits_text(p: &Pricing) -> String {
    let mut parts = Vec::new();
    match (p.volume_min, p.volume_max) {
        (Some(min), Some(max)) => parts.push(format!("{} to {}", thousands(min), m3(max))),
        (Some(min), None) => parts.push(format!("at least {}", m3(min))),
        (None, Some(max)) => parts.push(format!("up to {}", m3(max))),
        (None, None) => {}
    }
    match (p.collateral_min, p.collateral_max) {
        (Some(min), Some(max)) => parts.push(format!(
            "{} to {} ISK collateral",
            thousands(min),
            thousands(max)
        )),
        (Some(min), None) => parts.push(format!("at least {} ISK collateral", thousands(min))),
        (None, Some(max)) => parts.push(format!("up to {} ISK collateral", thousands(max))),
        (None, None) => {}
    }
    parts.join("; ")
}

fn days_text(p: &Pricing) -> String {
    match (p.days_to_expire, p.days_to_complete) {
        (Some(e), Some(c)) => format!("{e} to expire, {c} to complete"),
        (Some(e), None) => format!("{e} to expire"),
        (None, Some(c)) => format!("{c} to complete"),
        (None, None) => String::new(),
    }
}

fn result_card(
    calc: &Calculation,
    pricings: &[Pricing],
    names: &[(i64, String)],
    settings: &Settings,
    handler: Option<&Character>,
) -> Card {
    let Some(p) = pricings.iter().find(|p| p.id == calc.pricing) else {
        return Card::new("Reward").description("Choose a route.");
    };
    match &calc.outcome {
        Err(issues) => Card::new("This contract wouldn't pass")
            .description(issues.join("; "))
            .field("Route", route_label(names, p)),
        Ok(reward) => {
            let mut card = Card::new("Reward")
                .description("Issue a private courier contract with these details:")
                .field("Contract type", "Courier")
                .field("Availability", "Private");
            if let Some(id) = handler.and_then(|h| organization(h, &settings.mode)) {
                card = card.field("Assignee", name_of(id));
            }
            card = card
                .field("Pick up", place(names, p.start))
                .field("Ship to", place(names, p.end))
                .field("Reward", isk(*reward))
                .field("Collateral", isk(calc.collateral.unwrap_or(0.0)));
            if let Some(days) = p.days_to_expire {
                card = card.field("Expiration", format!("{days} days"));
            }
            if let Some(days) = p.days_to_complete {
                card = card.field("Days to complete", days.to_string());
            }
            if !p.details.is_empty() {
                card = card.field("Details", p.details.clone());
            }
            card
        }
    }
}

fn set_mode(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(viewer, "setup_contract_handler")?;
    let mode = submission.value("mode");
    if !MODES.iter().any(|(value, _, _)| *value == mode) {
        return Err(PageError::NotFound);
    }
    let sources = esi::data_sources();
    let chosen = if sources.is_empty() {
        None
    } else {
        let id = submission.value("handler").parse::<i64>().ok();
        Some(
            sources
                .iter()
                .find(|s| Some(s.id) == id)
                .ok_or(PageError::NotFound)?
                .id,
        )
    };
    let before = settings().map_err(|e| failed("reading settings", e))?;
    let handler_id = chosen.or(before.handler_id);
    // Another handler or mode reads other contracts: the ones kept so far
    // (and their notices) go, and the next sync starts afresh.
    let changed = handler_id != before.handler_id || mode != before.mode;
    storage::execute(
        "UPDATE settings SET operation_mode = $1, handler_id = $2, sync_error = NULL WHERE id = 1",
        &[mode.into(), handler_id.into()],
    )
    .map_err(|e| failed("saving the mode", e))?;
    if changed {
        storage::execute("DELETE FROM contracts", &[])
            .map_err(|e| failed("clearing contracts", e))?;
    }
    log::info(format!(
        "operation mode set to {mode}, handler {} by {} ({})",
        handler_id.map_or_else(|| "none".to_owned(), |id| id.to_string()),
        viewer.main.name,
        viewer.main.id
    ));
    jobs::enqueue(NewJob::new(SYNC).key("sync-now")).map_err(|e| failed("queuing a sync", e))?;
    Ok(SubmitResult::Redirect("handler".to_owned()))
}

fn contract_table(title: &str, empty: &str) -> Table {
    Table::new(vec![
        Column::text("Route"),
        Column::text("Status"),
        Column::text("Check"),
        Column::text("Issuer"),
        Column::text("Acceptor"),
        Column::numeric("Reward"),
        Column::numeric("Collateral"),
        Column::numeric("Volume"),
        Column::numeric("Issued"),
        Column::numeric("Expires"),
    ])
    .title(title)
    .empty(empty)
}

/// The names of the issuers and acceptors of `lists`' contracts, read at
/// once (a page lists hundreds: a read each would outlast its time).
fn people(lists: &[&[Contract]]) -> Result<HashMap<i64, String>, PageError> {
    let ids = lists
        .iter()
        .flat_map(|list| list.iter())
        .flat_map(|c| [Some(c.issuer), c.acceptor])
        .flatten()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows = storage::query(
        "SELECT id, name FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[ids.into()],
    )
    .map_err(|e| failed("reading names", e))?;
    Ok(rows
        .rows
        .iter()
        .map(|r| (int(r, 0), text(r, 1)))
        .filter(|(_, name)| !name.is_empty())
        .collect())
}

fn contract_row(
    c: &Contract,
    names: &[(i64, String)],
    people: &HashMap<i64, String>,
    pricings: &[Pricing],
    modifier: Option<f64>,
    now: DateTime<Utc>,
) -> Vec<Value> {
    let check = c.check(pricings, modifier);
    let who = |id: i64| people.get(&id).cloned().unwrap_or_else(|| id.to_string());
    vec![
        if c.title.is_empty() {
            c.route(names).into()
        } else {
            format!("{} ({})", c.route(names), c.title).into()
        },
        if c.expired(now) {
            badge("Expired", Tone::Neutral).into()
        } else {
            status_badge(&c.status)
        },
        check_badge(check.as_ref()),
        character(c.issuer, who(c.issuer)).into(),
        c.acceptor
            .map_or_else(|| "".into(), |a| character(a, who(a)).into()),
        isk(c.reward),
        isk(c.collateral),
        m3(c.volume).into(),
        when_value(c.issued),
        // When it expires (or expired), whatever became of it: the column
        // says Expires, so nothing else goes in it.
        when_value(c.expires),
    ]
}

fn mine_page(viewer: &Viewer) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let names = places().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let mine = viewer
        .characters
        .iter()
        .map(|c| c.id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // aa-freight's statuses for My Contracts (`freight/managers.py:262-272`),
    // the same as its customers hear about.
    let list = contracts(
        "WHERE issuer_id = ANY(string_to_array($1, ',')::bigint[]) \
         AND status = ANY(string_to_array($2, ','))",
        &[mine.into(), CUSTOMER_STATUSES.join(",").into()],
    )
    .map_err(|e| failed("reading contracts", e))?;
    let now = Utc::now();
    let mut table = contract_table(
        "My contracts",
        "No courier contracts from your characters to the freight service.",
    );
    let people = people(&[&list])?;
    for c in &list {
        table = table.row(contract_row(
            c,
            &names,
            &people,
            &pricings,
            settings.modifier,
            now,
        ));
    }
    Ok(Page::new("My contracts")
        .description("Your characters' courier contracts to the freight service")
        .table(table))
}

fn contracts_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let names = places().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let now = Utc::now();
    // aa-freight's Active Contracts (`freight/managers.py:247-255`).
    let active = contracts(
        "WHERE status = 'in_progress' OR (status = 'outstanding' \
             AND (date_expired IS NULL OR date_expired > now()))",
        &[],
    )
    .map_err(|e| failed("reading contracts", e))?;
    // aa-freight's All Contracts (`freight/managers.py:257-260`): every
    // status, the newest first.
    let all = contracts_up_to("", &[], ALL_ROWS).map_err(|e| failed("reading contracts", e))?;
    let people = people(&[&active, &all])?;
    let row = |c: &Contract| contract_row(c, &names, &people, &pricings, settings.modifier, now);
    let mut active_table = contract_table(
        "Active contracts",
        "No outstanding or in-progress contracts.",
    );
    for c in &active {
        active_table = active_table.row(row(c));
    }
    let total = storage::query("SELECT count(*) FROM contracts", &[])
        .map_err(|e| failed("counting contracts", e))?
        .rows
        .first()
        .map(|r| int(r, 0))
        .unwrap_or_default();
    let mut all_table = contract_table("All contracts", "No contracts yet.");
    for c in &all {
        all_table = all_table.row(row(c));
    }
    let mut all_sections = Vec::new();
    if total > ALL_ROWS {
        all_sections.push(Section::Text(format!(
            "The newest {ALL_ROWS} of {} contracts.",
            thousands(total as f64)
        )));
    }
    all_sections.push(Section::Table(all_table));
    Ok(Page::new("Contracts")
        .description("Courier contracts, checked against their route's pricing")
        .tab("Active", vec![Section::Table(active_table)])
        .tab("All", all_sections))
}

fn statistics_page() -> Result<Page, PageError> {
    let names = places().map_err(|e| failed("reading locations", e))?;
    let since = rfc3339(Utc::now() - Duration::days(STATISTICS_DAYS));
    let finished = "status = 'finished' AND date_completed > $1::timestamptz";
    let totals = "count(*), coalesce(sum(reward), 0)::float8, \
         coalesce(sum(collateral), 0)::float8, coalesce(sum(volume), 0)::float8";
    let columns = |first: &str| {
        vec![
            Column::text(first),
            Column::numeric("Contracts"),
            Column::numeric("Rewards"),
            Column::numeric("Collaterals"),
            Column::numeric("Volume"),
        ]
    };
    let add = |table: Table, first: Value, r: &[Db], at: usize| {
        table.row(vec![
            first,
            int(r, at).into(),
            isk(float(r, at + 1)),
            isk(float(r, at + 2)),
            m3(float(r, at + 3)).into(),
        ])
    };
    let mut page = Page::new("Statistics").description(format!(
        "Finished courier contracts of the last {STATISTICS_DAYS} days"
    ));

    let routes = storage::query(
        &format!(
            "SELECT least(start_location, end_location), greatest(start_location, end_location), \
                 {totals} FROM contracts WHERE {finished} GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 100"
        ),
        &[Db::timestamp(since.clone())],
    )
    .map_err(|e| failed("reading routes", e))?;
    let mut table = Table::new(columns("Route"))
        .title("Routes")
        .empty("None yet.");
    for r in &routes.rows {
        let label = format!(
            "{} ⇄ {}",
            place(&names, int(r, 0)),
            place(&names, int(r, 1))
        );
        table = add(table, label.into(), r, 2);
    }
    page = page.table(table);

    for (title, column, entity) in [
        ("Pilots", "acceptor_id", "character"),
        (
            "Pilot corporations",
            "acceptor_corporation_id",
            "corporation",
        ),
        ("Customers", "issuer_id", "character"),
    ] {
        let rows = storage::query(
            &format!(
                "SELECT {column}, {totals} FROM contracts WHERE {finished} AND {column} IS NOT NULL \
                 GROUP BY 1 ORDER BY 2 DESC LIMIT 100"
            ),
            &[Db::timestamp(since.clone())],
        )
        .map_err(|e| failed("reading statistics", e))?;
        let mut table = Table::new(columns(title)).title(title).empty("None yet.");
        for r in &rows.rows {
            let id = int(r, 0);
            let who: Value = if entity == "corporation" {
                corporation(id, name_of(id)).into()
            } else {
                character(id, name_of(id)).into()
            };
            table = add(table, who, r, 1);
        }
        page = page.table(table);
    }
    Ok(page)
}

// ---- locations -------------------------------------------------------------------

fn locations_page(problem: Option<&str>) -> Result<Page, PageError> {
    let rows = storage::query(
        "SELECT id, name, system_name, category FROM locations ORDER BY name LIMIT $1",
        &[MAX_LOCATIONS.into()],
    )
    .map_err(|e| failed("reading locations", e))?;
    let used = storage::query(
        "SELECT start_location FROM pricings UNION SELECT end_location FROM pricings",
        &[],
    )
    .map_err(|e| failed("reading pricings", e))?;
    let used: Vec<i64> = used.rows.iter().map(|r| int(r, 0)).collect();
    let mut table = Table::new(vec![
        Column::text("Name"),
        Column::text("System"),
        Column::text("Kind"),
        Column::numeric("ID"),
        Column::text(""),
    ])
    .title("Locations")
    .empty("None yet.");
    for r in &rows.rows {
        let id = int(r, 0);
        table = table.row(vec![
            text(r, 1).into(),
            text(r, 2).into(),
            if text(r, 3) == "station" {
                "Station".into()
            } else {
                "Structure".into()
            },
            id.to_string().into(),
            if used.contains(&id) {
                "In a route".into()
            } else {
                actions(vec![
                    action("Remove", "delete_location")
                        .field("id", id.to_string())
                        .tone(Tone::Danger),
                ])
            },
        ]);
    }
    let mut form = Form::new("add_location", "Add or update")
        .title("Add or update a location")
        .description(
            "A station is named from ESI by its id. A structure needs its name as in game (Tether can't read structures).",
        )
        .field(
            Field::number("location_id", "Station or structure ID")
                .range(Some(60_000_000.0), None, true)
                .required(),
        )
        .field(Field::text("name", "Name (structures)", 200))
        .field(Field::text("system", "Solar system (structures)", 100));
    if let Some(problem) = problem {
        form = form.description(problem.to_owned());
    }
    Ok(Page::new("Locations")
        .description("Stations and structures routes start and end at")
        .form(form)
        .table(table))
}

fn add_location(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(viewer, "add_location")?;
    let Ok(id) = submission.value("location_id").parse::<i64>() else {
        return Ok(SubmitResult::Page(locations_page(Some(
            "That isn't an id.",
        ))?));
    };
    let (name, system, category) = if (60_000_000..64_000_000).contains(&id) {
        match station(id) {
            Ok((name, system)) => (name, system, "station"),
            Err(_) => {
                return Ok(SubmitResult::Page(locations_page(Some(
                    "ESI doesn't know that station.",
                ))?));
            }
        }
    } else if id >= 1_000_000_000_000 {
        let name = submission.value("name").trim().to_owned();
        if name.is_empty() {
            return Ok(SubmitResult::Page(locations_page(Some(
                "A structure needs its name.",
            ))?));
        }
        let system = submission.value("system").trim().to_owned();
        // Names go into Discord notices: one line each.
        if name.chars().chain(system.chars()).any(char::is_control) {
            return Ok(SubmitResult::Page(locations_page(Some(
                "Names are one line of text.",
            ))?));
        }
        (name, system, "structure")
    } else {
        return Ok(SubmitResult::Page(locations_page(Some(
            "That's neither a station nor a structure id.",
        ))?));
    };
    let count = storage::query(
        "SELECT count(*) FROM locations WHERE id <> $1",
        &[id.into()],
    )
    .map_err(|e| failed("counting locations", e))?;
    if count.rows.first().map(|r| int(r, 0)).unwrap_or_default() >= MAX_LOCATIONS {
        return Ok(SubmitResult::Page(locations_page(Some(
            "There are as many locations as there can be.",
        ))?));
    }
    storage::execute(
        "INSERT INTO locations (id, name, system_name, category) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, system_name = EXCLUDED.system_name, \
             category = EXCLUDED.category",
        &[
            id.into(),
            name.clone().into(),
            system.into(),
            category.into(),
        ],
    )
    .map_err(|e| failed("saving a location", e))?;
    log::info(format!(
        "location {id} ({name}) saved by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("locations".to_owned()))
}

fn delete_location(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(viewer, "add_location")?;
    let id = number(submission.value("id"))?;
    // Routes keep theirs (the pricing's foreign key refuses it too).
    let removed = storage::execute(
        "DELETE FROM locations WHERE id = $1 AND id NOT IN \
             (SELECT start_location FROM pricings UNION SELECT end_location FROM pricings)",
        &[id.into()],
    )
    .map_err(|e| failed("removing a location", e))?;
    if removed > 0 {
        log::info(format!(
            "location {id} removed by {} ({})",
            viewer.main.name, viewer.main.id
        ));
    }
    Ok(SubmitResult::Redirect("locations".to_owned()))
}

// ---- pricing ----------------------------------------------------------------------

fn pricing_form(
    id: &str,
    submit: &str,
    names: &[(i64, String)],
    current: Option<&Pricing>,
    posted: Option<&Submission>,
) -> Form {
    let options: Vec<(String, String)> = names
        .iter()
        .map(|(id, name)| (id.to_string(), name.clone()))
        .collect();
    // What was posted wins (a form sent back with a problem), else the
    // pricing being edited.
    let value = |name: &str, stored: Option<String>| -> String {
        posted.map_or_else(|| stored.unwrap_or_default(), |s| s.value(name).to_owned())
    };
    let checked = |name: &str, stored: bool| posted.map_or(stored, |s| s.checked(name));
    let f = |v: Option<f64>| v.map(|v| v.to_string());
    let i = |v: Option<i64>| v.map(|v| v.to_string());
    let money = |name: &str, label: &str, stored: Option<f64>| {
        Field::number(name, label)
            .range(Some(0.0), None, false)
            .value(value(name, f(stored)))
    };
    let p = current.cloned().unwrap_or(Pricing {
        active: true,
        bidirectional: true,
        ..Pricing::default()
    });
    Form::new(id, submit)
        .field(
            select(
                "start",
                "From",
                options.clone(),
                &value("start", current.map(|p| p.start.to_string())),
            )
            .required(),
        )
        .field(
            select(
                "end",
                "To",
                options,
                &value("end", current.map(|p| p.end.to_string())),
            )
            .required(),
        )
        .field(
            Field::checkbox(
                "bidirectional",
                "Both ways",
                checked("bidirectional", p.bidirectional),
            )
            .help("The same pricing for the way back."),
        )
        .field(Field::checkbox(
            "active",
            "Active",
            checked("active", p.active),
        ))
        .field(money("price_base", "Base price (ISK)", p.price_base))
        .field(money("price_min", "Minimum price (ISK)", p.price_min))
        .field(money(
            "price_per_volume",
            "Price per m3 (ISK)",
            p.price_per_volume,
        ))
        .field(
            Field::checkbox(
                "use_modifier",
                "Use the global modifier",
                checked("use_modifier", p.use_modifier),
            )
            .help("The price per m3 changes with the modifier in the settings."),
        )
        .field(money(
            "price_per_collateral_percent",
            "Price per collateral percent",
            p.price_per_collateral_percent,
        ))
        .field(money(
            "collateral_min",
            "Minimum collateral (ISK)",
            p.collateral_min,
        ))
        .field(money(
            "collateral_max",
            "Maximum collateral (ISK)",
            p.collateral_max,
        ))
        .field(money("volume_min", "Minimum volume (m3)", p.volume_min))
        .field(money("volume_max", "Maximum volume (m3)", p.volume_max))
        .field(
            Field::number("days_to_expire", "Days to expire")
                .range(Some(1.0), Some(30.0), true)
                .value(value("days_to_expire", i(p.days_to_expire))),
        )
        .field(
            Field::number("days_to_complete", "Days to complete")
                .range(Some(1.0), Some(30.0), true)
                .value(value("days_to_complete", i(p.days_to_complete))),
        )
        .field(
            Field::textarea("details", "Details", 2_000)
                .value(value("details", Some(p.details.clone()))),
        )
}

fn pricing_page(problem: Option<(&str, &Submission)>) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let names = location_names().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let mut table = Table::new(vec![
        Column::text("Route"),
        Column::text("Price"),
        Column::text("Limits"),
        Column::text(""),
    ])
    .title("Pricing")
    .empty("No routes yet.");
    for p in &pricings {
        table = table.row(vec![
            link(route_label(&names, p), format!("pricing/{}", p.id)).into(),
            price_text(p, settings.modifier).into(),
            limits_text(p).into(),
            if p.active {
                "".into()
            } else {
                badge("Inactive", Tone::Neutral).into()
            },
        ]);
    }
    let mut page = Page::new("Pricing")
        .description("Routes, how their rewards are worked out, and the settings")
        .table(table);
    if names.len() < 2 {
        page = page.text("Routes need two locations: add them on Locations (stations by ID, structures by ID and name).");
    } else {
        let mut form = pricing_form(
            "add_pricing",
            "Add route",
            &names,
            None,
            problem.map(|(_, s)| s),
        )
        .title("Add a route");
        if let Some((problem, _)) = problem {
            form = form.description(problem.to_owned());
        }
        page = page.form(form);
    }
    let channels: Vec<(String, String)> = std::iter::once((String::new(), "Not sent".to_owned()))
        .chain(
            discord::channels()
                .into_iter()
                .map(|c| (c.id, format!("#{}", c.name))),
        )
        .collect();
    // The settings, saved at once from Tether's save bar (DESIGN.md, Save
    // bar); the routes above are added and changed one by one.
    Ok(page.settings(
        SettingsForm::new("settings")
            .group(
                SettingsGroup::new("Prices").field(
                    Field::number("modifier", "Global price per m3 modifier (%)")
                        .range(Some(-100.0), Some(1_000.0), false)
                        .value(settings.modifier.map(|m| m.to_string()).unwrap_or_default())
                        .help("Raises or lowers the price per m3 of routes using it, e.g. 10 for 10% more."),
                ),
            )
            .group(
                SettingsGroup::new("Discord")
                    .field(
                        select("pilot_channel", "Pilots' channel", channels.clone(), settings.pilot_channel.as_deref().unwrap_or_default())
                            .help("New contracts, for pilots to pick up."),
                    )
                    .field(
                        Field::text("pilot_ping", "Pilot notices mention the role of state", 64)
                            .value(settings.pilot_ping.clone().unwrap_or_default())
                            .help(pilot_ping_help(&settings)),
                    )
                    .field(
                        select("customer_channel", "Customers' channel", channels, settings.customer_channel.as_deref().unwrap_or_default())
                            .help("Each contract's status changes, naming its issuer and route. Everyone in the channel sees every customer's notices (aa-freight sends them privately; apps can't)."),
                    )
                    .field(
                        Field::checkbox("notify_all", "Announce every contract", settings.notify_all)
                            .help("Pilots and customers hear about contracts on routes without a pricing too."),
                    ),
            ),
    ))
}

/// What the pilots' mention does, and, when the last pilot notice went
/// out without it, that it can't take effect as things stand, with the
/// fix (DESIGN.md, Works from defaults).
fn pilot_ping_help(settings: &Settings) -> String {
    // aa-freight's FREIGHT_DISCORD_MENTIONS: Tether's bot never pings
    // @everyone or @here, so a state's role stands in for them.
    let what = "New-contract notices mention the Discord role given to this state under Discord, \
                Roles (e.g. Member), in place of aa-freight's @here. Empty: no mention.";
    match (&settings.pilot_ping, settings.pilot_ping_refused) {
        (Some(_), Some(at)) => format!(
            "{what} The last pilot notice ({} EVE) went out without its mention: no Discord role \
             is mapped to that state. Map one under Discord, Roles, or clear this.",
            at.format("%Y-%m-%d %H:%M")
        ),
        _ => what.to_owned(),
    }
}

fn edit_page(id: i64, problem: Option<(&str, &Submission)>) -> Result<Page, PageError> {
    let names = location_names().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    let p = pricings
        .iter()
        .find(|p| p.id == id)
        .ok_or(PageError::NotFound)?;
    let mut form = pricing_form(
        "edit_pricing",
        "Save",
        &names,
        Some(p),
        problem.map(|(_, s)| s),
    )
    .title(route_label(&names, p));
    if let Some((problem, _)) = problem {
        form = form.description(problem.to_owned());
    }
    Ok(Page::new("Edit route")
        .form(form)
        .card(Card::new("Delete").field(
            "",
            actions(vec![
                action("Delete route", "delete_pricing")
                    .tone(Tone::Danger)
                    .confirm("Its contracts are no longer checked."),
            ]),
        )))
}

/// Saves a pricing: `Ok(Err(problem))` when the form needs fixing.
fn save_pricing(
    viewer: &Viewer,
    id: Option<i64>,
    submission: &Submission,
) -> Result<Result<(), String>, PageError> {
    need(viewer, "manage")?;
    let names = location_names().map_err(|e| failed("reading locations", e))?;
    let pricings = pricing::all().map_err(|e| failed("reading pricings", e))?;
    if id.is_none() && pricings.len() as i64 >= MAX_PRICINGS {
        return Ok(Err("There are as many routes as there can be.".to_owned()));
    }
    let location = |name: &str| {
        submission
            .value(name)
            .parse::<i64>()
            .ok()
            .filter(|id| names.iter().any(|(known, _)| known == id))
    };
    let (Some(start), Some(end)) = (location("start"), location("end")) else {
        return Ok(Err("Choose where the route starts and ends.".to_owned()));
    };
    if start == end {
        return Ok(Err(
            "A route starts and ends at different locations.".to_owned()
        ));
    }
    let mut numbers = Vec::new();
    for name in [
        "price_base",
        "price_min",
        "price_per_volume",
        "price_per_collateral_percent",
        "collateral_min",
        "collateral_max",
        "volume_min",
        "volume_max",
    ] {
        match optional(submission, name) {
            Ok(Some(v)) if v < 0.0 => return Ok(Err(format!("{name} can't be negative."))),
            Ok(v) => numbers.push(v),
            Err(why) => return Ok(Err(why)),
        }
    }
    let days = |name: &str| optional(submission, name).map(|v| v.map(|v| v as i64));
    let (Ok(days_to_expire), Ok(days_to_complete)) =
        (days("days_to_expire"), days("days_to_complete"))
    else {
        return Ok(Err("Days are whole numbers.".to_owned()));
    };
    let bidirectional = submission.checked("bidirectional");
    // aa-freight's checks: some price, and one pricing per route.
    if numbers[..4].iter().all(Option::is_none) {
        return Ok(Err(
            "Give at least one of the base price, minimum price, price per m3 or price per collateral percent."
                .to_owned(),
        ));
    }
    for other in pricings.iter().filter(|p| Some(p.id) != id) {
        if other.start == start && other.end == end {
            return Ok(Err("There's a pricing for this route already.".to_owned()));
        }
        if other.start == end && other.end == start && (other.bidirectional || bidirectional) {
            return Ok(Err(
                "There's a pricing for the way back already, and one of them is both ways."
                    .to_owned(),
            ));
        }
    }
    let details = submission.value("details").trim().to_owned();
    let mut values: Vec<Db> = vec![
        start.into(),
        end.into(),
        submission.checked("active").into(),
        bidirectional.into(),
        submission.checked("use_modifier").into(),
    ];
    values.extend(numbers.into_iter().map(Db::from));
    values.push(days_to_expire.map(|d| d as i32).into());
    values.push(days_to_complete.map(|d| d as i32).into());
    values.push(details.into());
    let columns = "start_location, end_location, active, bidirectional, use_modifier, price_base, \
         price_min, price_per_volume, price_per_collateral_percent, collateral_min, collateral_max, \
         volume_min, volume_max, days_to_expire, days_to_complete, details";
    match id {
        None => {
            storage::execute(
                &format!(
                    "INSERT INTO pricings ({columns}) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
                ),
                &values,
            )
            .map_err(|e| failed("saving a pricing", e))?;
        }
        Some(id) => {
            values.push(id.into());
            storage::execute(
                &format!(
                    "UPDATE pricings SET ({columns}) = \
                     ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
                     WHERE id = $17"
                ),
                &values,
            )
            .map_err(|e| failed("saving a pricing", e))?;
        }
    }
    log::info(format!(
        "route {} saved by {} ({})",
        format_args!("{} → {}", place(&names, start), place(&names, end)),
        viewer.main.name,
        viewer.main.id
    ));
    Ok(Ok(()))
}

fn delete_pricing(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    need(viewer, "manage")?;
    storage::execute("DELETE FROM pricings WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting a pricing", e))?;
    log::info(format!(
        "route {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("pricing".to_owned()))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(viewer, "manage")?;
    let Ok(modifier) = optional(submission, "modifier") else {
        return Err(PageError::NotFound);
    };
    let ping = submission.value("pilot_ping").trim();
    if ping.chars().count() > 64 {
        return Ok(SubmitResult::Page(
            pricing_page(None)?.text("A state's name is at most 64 characters."),
        ));
    }
    let ping = (!ping.is_empty()).then(|| ping.to_owned());
    let assigned: Vec<String> = discord::channels().into_iter().map(|c| c.id).collect();
    let channel = |name: &str| {
        let value = submission.value(name);
        assigned.iter().find(|id| id.as_str() == value).cloned()
    };
    // A different state: whether its role is mapped is for its notices
    // to find out.
    storage::execute(
        "UPDATE settings SET price_per_volume_modifier = $1, pilot_channel = $2, \
             customer_channel = $3, notify_all = $4, \
             pilot_ping_refused = CASE WHEN pilot_ping IS NOT DISTINCT FROM $5 \
                 THEN pilot_ping_refused END, \
             pilot_ping = $5 \
         WHERE id = 1",
        &[
            modifier.into(),
            channel("pilot_channel").into(),
            channel("customer_channel").into(),
            submission.checked("notify_all").into(),
            ping.clone().into(),
        ],
    )
    .map_err(|e| failed("saving settings", e))?;
    log::info(format!(
        "settings saved by {} ({}); pilot notices mention {ping:?}",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("pricing".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_text_is_escaped_and_clipped() {
        assert_eq!(escape("@everyone *hi*"), "\\@everyone \\*hi\\*");
        let long = clip("x".repeat(2_000));
        assert_eq!(long.chars().count(), DISCORD_MAX);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn modes_keep_what_aa_freight_keeps() {
        let handler = Character {
            id: 1,
            name: "Handler".to_owned(),
            corporation_id: 100,
            alliance_id: Some(1_000),
        };
        let contract = |issuer_corporation: i64| serde_json::json!({ "issuer_id": 5, "issuer_corporation_id": issuer_corporation });
        let kept = keep(
            "my_corporation",
            &handler,
            vec![contract(100), contract(200)],
        );
        assert_eq!(kept.map(|k| k.len()).unwrap_or_default(), 1);
        let kept = keep("corp_public", &handler, vec![contract(100), contract(200)]);
        assert_eq!(kept.map(|k| k.len()).unwrap_or_default(), 2);
        assert_eq!(organization(&handler, "my_alliance"), Some(1_000));
        // The alliance modes: a member's contract, or one kept already.
        let c = serde_json::json!({ "contract_id": 7, "issuer_id": 5 });
        assert!(from_member(&c, &[5], &[]));
        assert!(from_member(&c, &[], &[7]));
        assert!(!from_member(&c, &[6], &[8]));
        assert_eq!(organization(&handler, "corp_public"), Some(100));
    }
}
