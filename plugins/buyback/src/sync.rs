//! Contract tracking (aa-buybackprogram `update_all_contracts_esi`,
//! `_process_contract`, `_set_contract_notifications`), its notices, the
//! Discord relay, and the wallets' balances.
//!
//! Every 30 minutes each manager's contracts are read (its character's
//! own and its corporation's), and every calculation's tracking number
//! looked for in their titles. A matched contract is kept, its items read
//! and checked against the calculation once, and its status followed
//! until it's finished or rejected. AA's sync failures are fixed: an
//! items read that fails is tried again (B16), a title matching many
//! calculations doesn't stop the run (B17), only item exchanges count and
//! only their offered items (B22), and a reverse contract stops being read
//! once it's done (B23).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Utc;
use serde_json::{Value, json};
use tether_plugin_sdk::discord::{self, Embed, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::log;
use tether_plugin_sdk::notify::{self, Level};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};

use crate::{boolean, int, opt_int, retry, settings, text};

pub const RELAY: &str = "relay";
/// Messages per relay run (the host's limit), and the gaps.
const SENDS_PER_RUN: usize = 5;
const RELAY_GAP_SECONDS: i64 = 15;
const RELAY_BACKOFF_SECONDS: i64 = 60;
/// News older than this isn't sent.
const STALE_HOURS: i64 = 24;
/// Item reads and structure names a run, within its ESI budget.
const ITEMS_PER_RUN: usize = 30;

/// A contract as ESI lists it.
#[derive(Debug, Clone)]
struct EsiContract {
    contract_id: i64,
    kind: String,
    assignee_id: i64,
    availability: String,
    date_completed: Option<String>,
    date_expired: Option<String>,
    date_issued: String,
    for_corporation: bool,
    issuer_corporation_id: i64,
    issuer_id: i64,
    start_location_id: Option<i64>,
    price: f64,
    reward: f64,
    status: String,
    title: String,
    volume: f64,
    from_corporation: bool,
}

fn contract(v: &Value, from_corporation: bool) -> Option<EsiContract> {
    Some(EsiContract {
        contract_id: v["contract_id"].as_i64()?,
        kind: v["type"].as_str().unwrap_or_default().to_owned(),
        assignee_id: v["assignee_id"].as_i64().unwrap_or_default(),
        availability: v["availability"].as_str().unwrap_or_default().to_owned(),
        date_completed: v["date_completed"].as_str().map(str::to_owned),
        date_expired: v["date_expired"].as_str().map(str::to_owned),
        date_issued: v["date_issued"].as_str()?.to_owned(),
        for_corporation: v["for_corporation"].as_bool().unwrap_or(false),
        issuer_corporation_id: v["issuer_corporation_id"].as_i64().unwrap_or_default(),
        issuer_id: v["issuer_id"].as_i64().unwrap_or_default(),
        start_location_id: v["start_location_id"].as_i64(),
        price: v["price"].as_f64().unwrap_or_default(),
        reward: v["reward"].as_f64().unwrap_or_default(),
        status: v["status"].as_str().unwrap_or_default().to_owned(),
        title: v["title"].as_str().unwrap_or_default().to_owned(),
        volume: v["volume"].as_f64().unwrap_or_default(),
        from_corporation,
    })
}

/// A manager's contracts: its character's own and its corporation's,
/// item exchanges only (B22).
fn read_contracts(owner: i64) -> Result<Vec<EsiContract>, String> {
    let mut out = Vec::new();
    for (endpoint, corp) in [("source-contracts", false), ("corporation-contracts", true)] {
        match esi::get_all(endpoint, Subject::DataSource(owner), &[]) {
            Ok(bodies) => {
                for body in bodies {
                    let list: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    out.extend(
                        list.as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| contract(c, corp))
                            .filter(|c| c.kind == "item_exchange"),
                    );
                }
            }
            Err(err) => {
                return Err(format!(
                    "{}'s {}: {}",
                    owner,
                    if corp {
                        "corporation contracts"
                    } else {
                        "contracts"
                    },
                    esi::describe(&err)
                ));
            }
        }
    }
    Ok(out)
}

/// Every 30 minutes: each manager's contracts against every calculation.
pub fn contracts() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let sources: HashSet<i64> = esi::data_sources().into_iter().map(|c| c.id).collect();
    let owners: Vec<i64> = storage::query(
        "SELECT owner_character FROM programs UNION SELECT owner_character FROM reverse_programs",
        &[],
    )
    .map_err(|e| retry("reading programs", e))?
    .rows
    .iter()
    .map(|r| int(r, 0))
    .filter(|o| sources.contains(o))
    .collect();
    let mut problems = Vec::new();
    let mut fetched: Vec<(i64, EsiContract)> = Vec::new();
    for owner in owners {
        match read_contracts(owner) {
            Ok(list) => fetched.extend(list.into_iter().map(|c| (owner, c))),
            Err(why) => {
                log::warn(format!("contracts weren't read: {why}"));
                problems.push(why);
            }
        }
    }
    let mut matched: HashSet<i64> = HashSet::new();
    normal(&fetched, &mut matched)?;
    crate::reverse::match_contracts(&fetched_view(&fetched), &mut matched)?;
    if settings.track_prefill_contracts {
        untracked(&settings, &fetched, &matched)?;
    }
    read_items()?;
    storage::execute(
        "UPDATE settings SET sync_error = $1 WHERE id = 1",
        &[(if problems.is_empty() {
            None
        } else {
            Some(problems.join("; "))
        })
        .into()],
    )
    .map_err(|e| retry("noting the read", e))?;
    queue_relay()
}

/// What other modules need of a fetched contract.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub owner: i64,
    pub contract_id: i64,
    pub title: String,
    pub status: String,
    pub price: f64,
    pub json: Value,
    pub from_corporation: bool,
}

fn fetched_view(fetched: &[(i64, EsiContract)]) -> Vec<Fetched> {
    fetched
        .iter()
        .map(|(owner, c)| Fetched {
            owner: *owner,
            contract_id: c.contract_id,
            title: c.title.clone(),
            status: c.status.clone(),
            price: if c.reward > 0.0 { c.reward } else { c.price },
            json: to_json(c),
            from_corporation: c.from_corporation,
        })
        .collect()
}

fn to_json(c: &EsiContract) -> Value {
    json!({
        "contract_id": c.contract_id, "assignee_id": c.assignee_id, "availability": c.availability,
        "date_completed": c.date_completed, "date_expired": c.date_expired,
        "date_issued": c.date_issued, "for_corporation": c.for_corporation,
        "issuer_corporation_id": c.issuer_corporation_id, "issuer_id": c.issuer_id,
        "start_location_id": c.start_location_id, "price": c.price, "status": c.status,
        "title": c.title, "volume": c.volume,
    })
}

/// Upserts a contract; the previous status if it was kept already.
pub(crate) fn upsert(
    c: &Fetched,
    no_tracking: bool,
    reverse: bool,
) -> Result<Option<String>, JobError> {
    let before = storage::query(
        "SELECT status FROM contracts WHERE contract_id = $1",
        &[c.contract_id.into()],
    )
    .map_err(|e| retry("reading a contract", e))?
    .rows
    .first()
    .map(|r| text(r, 0));
    let j = &c.json;
    storage::execute(
        "INSERT INTO contracts (contract_id, assignee_id, availability, date_completed, date_expired, \
             date_issued, for_corporation, issuer_corporation_id, issuer_id, start_location_id, price, \
             status, title, volume, no_tracking, is_reverse, owner_character, from_corporation, seen_at) \
         VALUES ($1, $2, $3, $4::timestamptz, $5::timestamptz, $6::timestamptz, $7, $8, $9, $10, $11, \
             $12, $13, $14, $15, $16, $17, $18, now()) \
         ON CONFLICT (contract_id) DO UPDATE SET availability = EXCLUDED.availability, \
             date_completed = EXCLUDED.date_completed, date_expired = EXCLUDED.date_expired, \
             price = EXCLUDED.price, status = EXCLUDED.status, title = EXCLUDED.title, \
             volume = EXCLUDED.volume, seen_at = now()",
        &[
            c.contract_id.into(),
            j["assignee_id"].as_i64().unwrap_or_default().into(),
            j["availability"].as_str().unwrap_or_default().to_owned().into(),
            j["date_completed"].as_str().map(str::to_owned).into(),
            j["date_expired"].as_str().map(str::to_owned).into(),
            j["date_issued"].as_str().unwrap_or_default().to_owned().into(),
            j["for_corporation"].as_bool().unwrap_or(false).into(),
            j["issuer_corporation_id"].as_i64().unwrap_or_default().into(),
            j["issuer_id"].as_i64().unwrap_or_default().into(),
            j["start_location_id"].as_i64().into(),
            c.price.into(),
            c.status.clone().into(),
            c.title.clone().into(),
            j["volume"].as_f64().unwrap_or_default().into(),
            no_tracking.into(),
            reverse.into(),
            c.owner.into(),
            c.from_corporation.into(),
        ],
    )
    .map_err(|e| retry("storing a contract", e))?;
    Ok(before)
}

/// Normal buyback: each calculation not settled yet, against the
/// contracts read (a title containing its tracking number).
fn normal(fetched: &[(i64, EsiContract)], matched: &mut HashSet<i64>) -> Result<(), JobError> {
    let trackings = storage::query(
        "SELECT t.id, t.tracking_number, t.program_id, t.issuer_account FROM trackings t \
         LEFT JOIN contracts c ON c.contract_id = t.contract_id \
         WHERE t.program_id IS NOT NULL \
           AND (t.contract_id IS NULL OR c.status NOT IN ('finished', 'rejected')) \
           AND t.created_at > now() - interval '60 days'",
        &[],
    )
    .map_err(|e| retry("reading calculations", e))?;
    let view = fetched_view(fetched);
    for r in &trackings.rows {
        let (tracking, number, program, issuer) = (int(r, 0), text(r, 1), int(r, 2), opt_int(r, 3));
        let Some(c) = view.iter().find(|c| c.title.contains(&number)) else {
            continue;
        };
        matched.insert(c.contract_id);
        let before = upsert(c, false, false)?;
        if before.is_none() {
            storage::execute(
                "UPDATE trackings SET contract_id = $1 WHERE id = $2",
                &[c.contract_id.into(), tracking.into()],
            )
            .map_err(|e| retry("linking a contract", e))?;
            // Its items, checks and notices come once its items are read.
        } else if before.as_deref() == Some("outstanding")
            && matches!(c.status.as_str(), "finished" | "rejected")
            && let Some(account) = issuer
        {
            seller_notice(account, program, &number, c);
        }
    }
    Ok(())
}

/// The seller's accepted or rejected notice, unless they turned them off
/// (AA's crashed for sellers who never opened the app, B3).
fn seller_notice(account: i64, _program: i64, number: &str, c: &Fetched) {
    if crate::notifications_off(account) {
        return;
    }
    let (title, level) = if c.status == "finished" {
        ("Your buyback contract has been accepted", Level::Success)
    } else {
        ("Your buyback contract has been rejected", Level::Danger)
    };
    let message = format!(
        "Tracking #: {number}\nPrice: {} ISK",
        crate::isk_text(c.price)
    );
    if let Err(err) = notify::account(account, title, &message, level) {
        log::warn(format!("the seller wasn't told about {number}: {err:?}"));
    }
}

/// Contracts with a buyback prefix in their title but no calculation's
/// tracking number: possible scams (AA's untracked contracts).
fn untracked(
    settings: &crate::Settings,
    fetched: &[(i64, EsiContract)],
    matched: &HashSet<i64>,
) -> Result<(), JobError> {
    let mut prefixes: Vec<String> = storage::query(
        "SELECT DISTINCT tracking_prefill FROM programs WHERE tracking_prefill <> '' \
         UNION SELECT DISTINCT tracking_prefill FROM reverse_programs WHERE tracking_prefill <> ''",
        &[],
    )
    .map_err(|e| retry("reading prefixes", e))?
    .rows
    .iter()
    .map(|r| text(r, 0))
    .collect();
    if !settings.tracking_prefill.is_empty() {
        prefixes.push(settings.tracking_prefill.clone());
    }
    for (owner, c) in fetched {
        if matched.contains(&c.contract_id)
            || matches!(c.status.as_str(), "finished" | "deleted" | "rejected")
            || !prefixes.iter().any(|p| c.title.contains(p.as_str()))
        {
            continue;
        }
        // A calculation (any) whose number holds the whole title.
        let known = storage::query(
            "SELECT 1 FROM trackings WHERE strpos(tracking_number, $1) > 0 \
             UNION ALL SELECT 1 FROM reverse_trackings WHERE strpos(tracking_number, $1) > 0 LIMIT 1",
            &[c.title.clone().into()],
        )
        .map_err(|e| retry("reading calculations", e))?;
        if !known.rows.is_empty() {
            continue;
        }
        let view = &fetched_view(&[(*owner, c.clone())])[0];
        if upsert(view, true, false)?.is_none() {
            storage::execute(
                "INSERT INTO contract_flags (contract_id, tone, header, message) VALUES ($1, 'danger', \
                 'Suspicious Contract', 'Contract has no tracking object but it has a buyback prefill text! Possibly a scam contract.')",
                &[c.contract_id.into()],
            )
            .map_err(|e| retry("flagging a contract", e))?;
        }
    }
    Ok(())
}

/// Items of contracts kept but not read yet (a few a run), then their
/// checks and notices.
fn read_items() -> Result<(), JobError> {
    let waiting = storage::query(
        "SELECT contract_id, owner_character, from_corporation, is_reverse, no_tracking FROM contracts \
         WHERE NOT items_read AND status NOT IN ('deleted') ORDER BY seen_at LIMIT $1",
        &[(ITEMS_PER_RUN as i64).into()],
    )
    .map_err(|e| retry("reading contracts", e))?;
    for r in &waiting.rows {
        let (id, owner, corp, reverse, no_tracking) = (
            int(r, 0),
            int(r, 1),
            boolean(r, 2),
            boolean(r, 3),
            boolean(r, 4),
        );
        let endpoint = if corp {
            "corporation-contract-items"
        } else {
            "source-contract-items"
        };
        let items = match esi::get(
            endpoint,
            Subject::DataSource(owner),
            &[("contract_id".to_owned(), id.to_string())],
            None,
        ) {
            Ok(r) => serde_json::from_str::<Value>(&r.body).unwrap_or(Value::Null),
            Err(err) => {
                log::warn(format!(
                    "contract {id}'s items weren't read: {}",
                    esi::describe(&err)
                ));
                continue;
            }
        };
        // Normal: what the seller gives; reverse: what the buyer asks for.
        let want_included = !reverse;
        let rows: Vec<Value> = items
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["is_included"].as_bool().unwrap_or(true) == want_included)
            .filter_map(|i| Some(json!({ "type_id": i["type_id"].as_i64()?, "quantity": i["quantity"].as_i64()? })))
            .collect();
        storage::transaction(&[
            Statement::new("DELETE FROM contract_items WHERE contract_id = $1", vec![id.into()]),
            Statement::new(
                "INSERT INTO contract_items (contract_id, type_id, quantity) \
                 SELECT $1, type_id, quantity FROM jsonb_to_recordset($2::jsonb) AS x(type_id bigint, quantity bigint)",
                vec![id.into(), Db::json(Value::Array(rows).to_string())],
            ),
            Statement::new("UPDATE contracts SET items_read = true WHERE contract_id = $1", vec![id.into()]),
        ])
        .map_err(|e| retry("storing items", e))?;
        name_location(id, owner)?;
        if no_tracking {
            continue;
        }
        if reverse {
            crate::reverse::checks_and_notices(id)?;
        } else {
            checks_and_notices(id)?;
        }
    }
    Ok(())
}

/// Where a contract was made, by name: a station's, or a structure's as
/// the manager sees it (kept), else "Unknown" (asked again next time).
fn name_location(contract_id: i64, owner: i64) -> Result<(), JobError> {
    let row = storage::query(
        "SELECT start_location_id FROM contracts WHERE contract_id = $1 AND location_name IS NULL",
        &[contract_id.into()],
    )
    .map_err(|e| retry("reading a contract", e))?;
    let Some(place) = row.rows.first().and_then(|r| opt_int(r, 0)) else {
        return Ok(());
    };
    let kept = storage::query(
        "SELECT name FROM structure_names WHERE structure_id = $1",
        &[place.into()],
    )
    .map_err(|e| retry("reading names", e))?
    .rows
    .first()
    .map(|r| text(r, 0));
    let name = match kept {
        Some(name) => Some(name),
        None if place < 100_000_000 => esi::names(&[place])
            .ok()
            .and_then(|n| n.into_iter().find(|n| n.id == place))
            .map(|n| n.name),
        None => esi::get(
            "source-structure",
            Subject::DataSource(owner),
            &[("structure_id".to_owned(), place.to_string())],
            None,
        )
        .ok()
        .and_then(|r| serde_json::from_str::<Value>(&r.body).ok())
        .and_then(|v| v["name"].as_str().map(str::to_owned)),
    };
    let mut statements = vec![Statement::new(
        "UPDATE contracts SET location_name = $2 WHERE contract_id = $1",
        vec![
            contract_id.into(),
            name.clone().unwrap_or_else(|| "Unknown".to_owned()).into(),
        ],
    )];
    if let Some(name) = name {
        statements.push(Statement::new(
            "INSERT INTO structure_names (structure_id, name) VALUES ($1, $2) \
             ON CONFLICT (structure_id) DO UPDATE SET name = EXCLUDED.name, updated_at = now()",
            vec![place.into(), name.into()],
        ));
    }
    storage::transaction(&statements).map_err(|e| retry("naming a location", e))?;
    Ok(())
}

/// Merged quantities by type (B15: split stacks aren't a mismatch).
pub(crate) fn merged(rows: &[(i64, i64)]) -> BTreeMap<i64, i64> {
    let mut m = BTreeMap::new();
    for (t, q) in rows {
        *m.entry(*t).or_insert(0) += *q;
    }
    m
}

pub(crate) fn pairs(sql: &str, id: i64) -> Result<Vec<(i64, i64)>, JobError> {
    Ok(storage::query(sql, &[id.into()])
        .map_err(|e| retry("reading items", e))?
        .rows
        .iter()
        .map(|r| (int(r, 0), int(r, 1)))
        .collect())
}

/// A normal contract's checks against its calculation, once, then the
/// manager's notice and the program's card.
fn checks_and_notices(contract_id: i64) -> Result<(), JobError> {
    let rows = storage::query(
        "SELECT t.id, t.tracking_number, t.net_price::float8, t.donation::float8, t.notes, \
                c.price::float8, c.title, c.start_location_id, c.assignee_id, \
                p.id, p.is_corporation, p.owner_corporation, p.owner_character \
         FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
         JOIN programs p ON p.id = t.program_id WHERE t.contract_id = $1",
        &[contract_id.into()],
    )
    .map_err(|e| retry("reading the calculation", e))?;
    let Some(r) = rows.rows.first() else {
        return Ok(());
    };
    let (tracking, number) = (int(r, 0), text(r, 1));
    let (net, donation, notes) = (
        crate::float(r, 2),
        crate::float(r, 3),
        crate::opt_text(r, 4),
    );
    let (price, title, start, assignee) =
        (crate::float(r, 5), text(r, 6), opt_int(r, 7), int(r, 8));
    let (program_id, is_corp, owner_corp, owner_char) =
        (int(r, 9), boolean(r, 10), int(r, 11), int(r, 12));
    let mut flags: Vec<(&str, String, String)> = Vec::new();
    let wanted = merged(&pairs(
        "SELECT type_id, quantity FROM tracking_items WHERE tracking_id = $1",
        tracking,
    )?);
    let got = merged(&pairs(
        "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1",
        contract_id,
    )?);
    if wanted != got {
        flags.push(("danger", "Item mismatch".into(), "Tracked items do not match the actual items in the contract. See details for more info.".into()));
    }
    let (calc, asked) = (net.trunc(), price.trunc());
    if calc >= 0.0 && calc != asked {
        if asked > calc {
            flags.push((
                "danger",
                "High ask price".into(),
                "Ask price is above the calculated price for this contract".into(),
            ));
        } else {
            flags.push((
                "warning",
                "Low ask price".into(),
                "Ask price is below the calculated price for this contract".into(),
            ));
        }
    }
    // B4: locations without a station or structure id don't count.
    let places: Vec<i64> = storage::query(
        "SELECT l.structure_id FROM locations l JOIN program_locations pl ON pl.location_id = l.id \
         WHERE pl.program_id = $1 AND l.structure_id IS NOT NULL",
        &[program_id.into()],
    )
    .map_err(|e| retry("reading locations", e))?
    .rows
    .iter()
    .map(|r| int(r, 0))
    .collect();
    if !places.is_empty() && !start.is_some_and(|s| places.contains(&s)) {
        flags.push((
            "danger",
            "Location mismatch".into(),
            "Contract location does not match program location".into(),
        ));
    }
    if assignee == owner_corp && !is_corp {
        flags.push(("warning", "Receiver mismatch".into(), "Contract is made for the corporation while it should be made directly to the program manager's character".into()));
    } else if assignee != owner_corp && is_corp {
        flags.push(("warning", "Receiver mismatch".into(), "Contract is made for the program manager's character while it should be made to the manager's corporation".into()));
    }
    if title.trim() != number {
        flags.push(("warning", "Title variation".into(), format!("Contract description contains extra characters besides the tracking number. The description should be: '{number}', instead it is: '{title}'")));
    }
    if donation > 0.0 {
        flags.push((
            "success",
            "Donation".into(),
            "Contract contains a donation".into(),
        ));
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        flags.push(("info", "Note from seller".into(), n));
    }
    let watched: Vec<String> = watched_items(program_id, contract_id)?;
    if !watched.is_empty() {
        flags.push((
            "watch",
            "Watchlisted item".into(),
            format!(
                "Manual review recommended. Contains watchlisted items: {}",
                watched.join(", ")
            ),
        ));
    }
    let statements: Vec<Statement> = flags
        .iter()
        .map(|(tone, header, message)| {
            Statement::new(
                "INSERT INTO contract_flags (contract_id, tone, header, message) VALUES ($1, $2, $3, $4)",
                vec![contract_id.into(), (*tone).into(), header.clone().into(), message.clone().into()],
            )
        })
        .collect();
    if !statements.is_empty() {
        storage::transaction(&statements).map_err(|e| retry("flagging the contract", e))?;
    }
    new_contract_notice(program_id, contract_id, &number, owner_char, false)
}

/// Names of the contract's items on the program's watchlist.
fn watched_items(program_id: i64, contract_id: i64) -> Result<Vec<String>, JobError> {
    let ids: Vec<i64> = pairs(
        "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1",
        contract_id,
    )?
    .into_iter()
    .map(|(t, _)| t)
    .collect();
    let watch = storage::query(
        "SELECT type_id, group_id FROM watchlist WHERE program_id = $1",
        &[program_id.into()],
    )
    .map_err(|e| retry("reading the watchlist", e))?;
    let (types, groups): (Vec<i64>, Vec<i64>) = (
        watch.rows.iter().filter_map(|r| opt_int(r, 0)).collect(),
        watch.rows.iter().filter_map(|r| opt_int(r, 1)).collect(),
    );
    if types.is_empty() && groups.is_empty() {
        return Ok(Vec::new());
    }
    let info = crate::statics::by_ids(&ids).map_err(|e| retry("reading item data", e))?;
    let mut names: Vec<String> = info
        .values()
        .filter(|t| types.contains(&t.id) || groups.contains(&t.group_id))
        .map(|t| t.name.clone())
        .collect();
    names.sort();
    Ok(names)
}

/// A new contract: the manager's notice (if the program says so) and the
/// program's card (if it has a channel).
pub(crate) fn new_contract_notice(
    program_id: i64,
    contract_id: i64,
    number: &str,
    _owner: i64,
    reverse: bool,
) -> Result<(), JobError> {
    let table = if reverse {
        "reverse_programs"
    } else {
        "programs"
    };
    let show_items = if reverse {
        "true"
    } else {
        "discord_show_item_list"
    };
    let rows = storage::query(
        &format!(
            "SELECT p.name, p.notify_manager, p.manager_account, p.discord_channel, {show_items}, \
                    c.date_issued, c.issuer_id, c.assignee_id, c.location_name, c.volume::float8, c.price::float8 \
             FROM {table} p, contracts c WHERE p.id = $1 AND c.contract_id = $2"
        ),
        &[program_id.into(), contract_id.into()],
    )
    .map_err(|e| retry("reading the program", e))?;
    let Some(r) = rows.rows.first() else {
        return Ok(());
    };
    let name = text(r, 0);
    let title = if reverse {
        format!("New reverse buyback request for program {name}")
    } else {
        format!("New buyback contract assigned for program {name}")
    };
    let ids = [int(r, 6), int(r, 7)];
    let names: HashMap<i64, String> = esi::names(&ids)
        .map(|n| n.into_iter().map(|n| (n.id, n.name)).collect())
        .unwrap_or_default();
    let who = |id: i64| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let location = crate::opt_text(r, 8).unwrap_or_else(|| "Unknown".to_owned());
    let issued = text(r, 5);
    let price = crate::float(r, 10);
    if boolean(r, 1) {
        let message = format!(
            "Date issued: {issued}\nIssued from: {}\nLocation: {location}\nTracking #: {number}\nPrice: {} ISK",
            who(ids[0]),
            crate::isk_text(price)
        );
        if let Err(err) = notify::account(int(r, 2), &title, &message, Level::Success) {
            log::warn(format!("the manager wasn't told about {number}: {err:?}"));
        }
    }
    let Some(channel) = crate::opt_text(r, 3) else {
        return Ok(());
    };
    let flags: Vec<String> = storage::query(
        "SELECT message FROM contract_flags WHERE contract_id = $1 ORDER BY id",
        &[contract_id.into()],
    )
    .map_err(|e| retry("reading flags", e))?
    .rows
    .iter()
    .map(|r| text(r, 0))
    .collect();
    let mut description = String::new();
    if boolean(r, 4) {
        let items = pairs(
            "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1",
            contract_id,
        )?;
        let info = crate::statics::by_ids(&items.iter().map(|(t, _)| *t).collect::<Vec<_>>())
            .unwrap_or_default();
        for (t, q) in &items {
            let line = format!(
                "{} x {q}\n",
                info.get(t)
                    .map_or_else(|| t.to_string(), |i| i.name.clone())
            );
            if description.len() + line.len() > 1800 {
                description.push_str("…and more: see the contract in Tether.");
                break;
            }
            description.push_str(&line);
        }
    }
    let mut card = json!({
        "title": title,
        "fields": [
            ["Date issued", issued],
            ["Issued from", crate::escape(&who(ids[0]))],
            ["Issued to", crate::escape(&who(ids[1]))],
            ["Location", crate::escape(&location)],
            ["Tracking #", number],
            ["Volume", format!("{} m3", crate::float(r, 9).round())],
            ["Value", format!("{} ISK", crate::isk_text(price))],
        ],
        "notes": flags.join("\n\n").chars().take(1000).collect::<String>(),
        "color": 0x005B_C0DE,
    });
    if !description.trim().is_empty() {
        card["description"] = Value::String(crate::escape(description.trim()));
    }
    let page = if reverse {
        format!("reverse/tracking/{number}")
    } else {
        format!("tracking/{number}")
    };
    storage::execute(
        "INSERT INTO outbox (channel, card, page) VALUES ($1, $2::jsonb, $3)",
        &[channel.into(), Db::json(card.to_string()), page.into()],
    )
    .map_err(|e| retry("queuing the card", e))?;
    Ok(())
}

fn embed(card: &Value) -> Option<Embed> {
    let mut e = Embed::new(card["title"].as_str()?.to_owned());
    if let Some(d) = card["description"].as_str() {
        e = e.description(d.to_owned());
    }
    if let Some(c) = card["color"].as_u64().and_then(|c| u32::try_from(c).ok()) {
        e = e.color(c);
    }
    for f in card["fields"].as_array().into_iter().flatten() {
        if let (Some(k), Some(v)) = (f[0].as_str(), f[1].as_str())
            && !v.is_empty()
        {
            e = e.field(k.to_owned(), v.to_owned());
        }
    }
    if let Some(n) = card["notes"].as_str().filter(|n| !n.trim().is_empty()) {
        e = e.wide_field("Notes", crate::escape(n));
    }
    Some(e.footer("Buyback"))
}

fn queue_relay() -> Result<(), JobError> {
    let waiting = storage::query(
        "SELECT 1 FROM outbox WHERE sent_at IS NULL AND failed IS NULL LIMIT 1",
        &[],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    if waiting.rows.is_empty() {
        return Ok(());
    }
    jobs::enqueue(NewJob::new(RELAY).key(RELAY)).map_err(|e| retry("queuing the relay", e))
}

/// Posts waiting cards, up to the host's limit a run, then comes back.
pub fn relay() -> Result<(), JobError> {
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND queued_at < now() - make_interval(hours => $1::int)",
        &[STALE_HOURS.into()],
    )
    .map_err(|e| retry("expiring cards", e))?;
    let waiting = storage::query(
        "SELECT id, channel, card::text, page FROM outbox WHERE sent_at IS NULL AND failed IS NULL \
         ORDER BY id LIMIT $1",
        &[(SENDS_PER_RUN as i64).into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    let mut gap = RELAY_GAP_SECONDS;
    for r in &waiting.rows {
        let id = int(r, 0);
        let claimed = storage::execute(
            "UPDATE outbox SET sent_at = now() WHERE id = $1 AND sent_at IS NULL AND failed IS NULL",
            &[id.into()],
        )
        .map_err(|e| retry("claiming a card", e))?;
        if claimed == 0 {
            continue;
        }
        let card: Value = serde_json::from_str(&text(r, 2)).unwrap_or(Value::Null);
        let Some(e) = embed(&card) else {
            continue;
        };
        let sent = match crate::opt_text(r, 3) {
            Some(page) => discord::send_linked_embed(&text(r, 1), &e, &page, Mention::None),
            None => discord::send_embed(&text(r, 1), &e, Mention::None),
        };
        match sent {
            Ok(()) => {}
            Err(discord::Error::NotAllowed(why) | discord::Error::Invalid(why)) => {
                log::warn(format!("a card wasn't posted: {why}"));
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL, failed = $2 WHERE id = $1",
                    &[id.into(), why.into()],
                )
                .map_err(|e| retry("marking a card", e))?;
            }
            Err(err) => {
                log::info(format!("Discord: {err:?}; trying again in a minute"));
                gap = RELAY_BACKOFF_SECONDS;
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL WHERE id = $1",
                    &[id.into()],
                )
                .map_err(|e| retry("releasing a card", e))?;
                break;
            }
        }
    }
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
        let at = crate::rfc3339(Utc::now() + chrono::Duration::seconds(gap));
        jobs::enqueue(NewJob::new(RELAY).key(RELAY).at(at))
            .map_err(|e| retry("queuing the relay", e))?;
    }
    Ok(())
}

/// Hourly: each manager corporation's wallet balances and division names
/// (Accountant and Director roles; without them, nothing changes).
pub fn wallets() -> Result<(), JobError> {
    let sources: HashMap<i64, i64> = esi::data_sources()
        .into_iter()
        .map(|c| (c.corporation_id, c.id))
        .collect();
    let owners: Vec<i64> = storage::query(
        "SELECT DISTINCT owner_corporation FROM programs UNION SELECT DISTINCT owner_corporation FROM reverse_programs",
        &[],
    )
    .map_err(|e| retry("reading programs", e))?
    .rows
    .iter()
    .map(|r| int(r, 0))
    .collect();
    for corp in owners {
        let Some(owner) = sources.get(&corp).copied() else {
            continue;
        };
        let divisions = esi::get(
            "corporation-divisions",
            Subject::DataSource(owner),
            &[],
            None,
        )
        .ok()
        .and_then(|r| serde_json::from_str::<Value>(&r.body).ok());
        let names = |kind: &str, n: i64| -> Option<String> {
            divisions.as_ref()?[kind]
                .as_array()?
                .iter()
                .find(|d| d["division"].as_i64() == Some(n))?["name"]
                .as_str()
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
        };
        let ordinal = ["1st", "2nd", "3rd", "4th", "5th", "6th", "7th"];
        let hangars: Vec<Value> = (1..=7)
            .map(|n| {
                json!({
                    "division": n,
                    "name": names("hangar", n).unwrap_or_else(|| format!("{} Division", ordinal[(n - 1) as usize])),
                })
            })
            .collect();
        let mut statements = vec![Statement::new(
            "INSERT INTO hangar_divisions (corporation_id, division, name, updated_at) \
             SELECT $1, division, name, now() FROM jsonb_to_recordset($2::jsonb) AS x(division int, name text) \
             ON CONFLICT (corporation_id, division) DO UPDATE SET name = EXCLUDED.name, updated_at = now()",
            vec![corp.into(), Db::json(Value::Array(hangars).to_string())],
        )];
        match esi::get("corporation-wallets", Subject::DataSource(owner), &[], None) {
            Ok(r) => {
                let list: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
                let rows: Vec<Value> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|w| {
                        let n = w["division"].as_i64()?;
                        Some(json!({
                            "division": n,
                            "balance": w["balance"].as_f64().unwrap_or_default(),
                            "name": names("wallet", n).unwrap_or_else(|| {
                                if n == 1 { "Master Wallet".to_owned() } else { format!("Division {n}") }
                            }),
                        }))
                    })
                    .collect();
                statements.push(Statement::new(
                    "INSERT INTO wallets (corporation_id, division, name, balance, updated_at) \
                     SELECT $1, division, name, balance, now() FROM jsonb_to_recordset($2::jsonb) \
                          AS x(division int, name text, balance numeric) \
                     ON CONFLICT (corporation_id, division) DO UPDATE SET name = EXCLUDED.name, \
                          balance = EXCLUDED.balance, updated_at = now()",
                    vec![corp.into(), Db::json(Value::Array(rows).to_string())],
                ));
            }
            Err(err) => log::info(format!(
                "corporation {corp}'s wallets weren't read: {}",
                esi::describe(&err)
            )),
        }
        storage::transaction(&statements).map_err(|e| retry("storing wallets", e))?;
    }
    Ok(())
}
