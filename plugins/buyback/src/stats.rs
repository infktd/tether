//! Statistics (aa-buybackprogram `views/stats.py`): a contract's details,
//! a pilot's own contracts, a manager's programs' and everyone's, each
//! program's leaderboard and its performance by month, with the CSV.
//!
//! AA's rules, with its bugs fixed (Jay, 2026-10-07): finished contracts
//! stay in the statistics after their expiry date (B5; outstanding ones
//! past it drop out, as in AA), performance ISK is quantity × unit value
//! (B8), and the leaderboard, performance and details pages check who may
//! see the program (B2). AA's charts are tables here, with its scaling
//! ("Millions", "Billions").

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use tether_plugin_sdk::downloads;
use tether_plugin_sdk::jobs::{self, NewJob};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Page, PageError, Request, Section, Stat, Submission, SubmitResult, Table, Tone,
    Toolbar, Value, action, badge, character, corporation, item_type, link, log,
};

use crate::calculator;
use crate::pricing::Totals;
use crate::programs::{self, Program};
use crate::statics;
use crate::{
    Access, boolean, failed, float, int, isk_text, json_ids, opt_int, opt_text, settings, text,
    when,
};

/// Rows a list shows (the host pages them, 25 at a time).
const MAX_ROWS: i64 = 500;
/// Rows the performance CSV takes, at most.
const MAX_EXPORT_ROWS: usize = 50_000;
/// Rows one storage read returns, at most.
const READ_CHUNK: usize = 5_000;
/// Statuses the lists count: AA's.
const OUTSTANDING: &str = "outstanding";
const FINISHED: &str = "finished";
/// B5: finished contracts stay; others drop out once expired.
const SHOWN: &str = "(c.status = 'finished' OR c.date_expired IS NULL OR c.date_expired >= now())";

// ---- whose contracts --------------------------------------------------------

/// Whose contracts a statistics page lists.
enum Scope {
    /// Made by these characters (My statistics).
    Issuer(Vec<i64>),
    /// Made to these characters and corporations (Program statistics).
    Assignee(Vec<i64>),
    /// Every contract (All statistics).
    All,
}

impl Scope {
    /// The condition, on `c` (contracts), with the ids as `$1`.
    fn sql(&self) -> &'static str {
        match self {
            Scope::Issuer(_) => {
                "c.issuer_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint)"
            }
            Scope::Assignee(_) => {
                "c.assignee_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint)"
            }
            Scope::All => "$1::jsonb IS NOT NULL",
        }
    }

    fn ids(&self) -> Db {
        match self {
            Scope::Issuer(ids) | Scope::Assignee(ids) => json_ids(ids),
            Scope::All => json_ids(&[]),
        }
    }
}

/// A tracked contract in a list.
struct Listed {
    number: String,
    program_name: Option<String>,
    contract_id: i64,
    issuer: i64,
    location: Option<String>,
    issued: Option<DateTime<Utc>>,
    completed: Option<DateTime<Utc>>,
    status: String,
    price: f64,
}

fn listed(
    scope: &Scope,
    program: Option<i64>,
    status: &str,
) -> Result<Vec<Listed>, storage::Error> {
    let rows = storage::query(
        &format!(
            "SELECT t.tracking_number, p.name, c.contract_id, c.issuer_id, c.location_name, \
                    c.date_issued, c.date_completed, c.status, c.price::float8 \
             FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
             LEFT JOIN programs p ON p.id = t.program_id \
             WHERE {} AND {SHOWN} AND ($2::bigint IS NULL OR t.program_id = $2) AND c.status = $3 \
             ORDER BY c.date_issued DESC, c.contract_id DESC LIMIT $4",
            scope.sql()
        ),
        &[scope.ids(), program.into(), status.into(), MAX_ROWS.into()],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Listed {
            number: text(r, 0),
            program_name: opt_text(r, 1),
            contract_id: int(r, 2),
            issuer: int(r, 3),
            location: opt_text(r, 4),
            issued: when(r, 5),
            completed: when(r, 6),
            status: text(r, 7),
            price: float(r, 8),
        })
        .collect())
}

/// AA's four tiles: outstanding count and value, completed count and
/// total bought.
struct Tiles {
    outstanding: i64,
    outstanding_value: f64,
    finished: i64,
    bought: f64,
}

fn tiles(scope: &Scope, program: Option<i64>) -> Result<Tiles, storage::Error> {
    let rows = storage::query(
        &format!(
            "SELECT count(*) FILTER (WHERE c.status = 'outstanding'), \
                    coalesce(sum(c.price) FILTER (WHERE c.status = 'outstanding'), 0)::float8, \
                    count(*) FILTER (WHERE c.status = 'finished'), \
                    coalesce(sum(c.price) FILTER (WHERE c.status = 'finished'), 0)::float8 \
             FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
             WHERE {} AND {SHOWN} AND ($2::bigint IS NULL OR t.program_id = $2)",
            scope.sql()
        ),
        &[scope.ids(), program.into()],
    )?;
    let r = rows.rows.first().cloned().unwrap_or_default();
    Ok(Tiles {
        outstanding: int(&r, 0),
        outstanding_value: float(&r, 1),
        finished: int(&r, 2),
        bought: float(&r, 3),
    })
}

/// The programs a scope's contracts belong to, for its filter.
fn program_choices(scope: &Scope) -> Result<Vec<(String, String)>, storage::Error> {
    let rows = storage::query(
        &format!(
            "SELECT DISTINCT p.id, p.name FROM trackings t \
             JOIN contracts c ON c.contract_id = t.contract_id \
             JOIN programs p ON p.id = t.program_id WHERE {} ORDER BY p.name, p.id LIMIT 100",
            scope.sql()
        ),
        &[scope.ids()],
    )?;
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            let name = text(r, 1);
            let label = if name.is_empty() {
                "Unnamed Program".to_owned()
            } else {
                name
            };
            (int(r, 0).to_string(), label)
        })
        .collect())
}

/// Contracts' flags, in order, by contract.
fn flags(contracts: &[i64]) -> Result<HashMap<i64, Vec<Flag>>, storage::Error> {
    let mut out: HashMap<i64, Vec<Flag>> = HashMap::new();
    if contracts.is_empty() {
        return Ok(out);
    }
    let rows = storage::query(
        "SELECT contract_id, tone, header, message FROM contract_flags \
         WHERE contract_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint) ORDER BY id",
        &[json_ids(contracts)],
    )?;
    for r in &rows.rows {
        out.entry(int(r, 0)).or_default().push(Flag {
            tone: text(r, 1),
            header: text(r, 2),
            message: text(r, 3),
        });
    }
    Ok(out)
}

/// One of a contract's flags (AA's ContractNotification).
struct Flag {
    tone: String,
    header: String,
    message: String,
}

/// AA's colours as Tether's tones: red, orange, green, else neutral.
fn flag_tone(tone: &str) -> Tone {
    match tone {
        "danger" => Tone::Danger,
        "warning" => Tone::Warning,
        "success" => Tone::Success,
        _ => Tone::Neutral,
    }
}

fn severity(tone: &str) -> u8 {
    match tone {
        "danger" => 4,
        "warning" => 3,
        "watch" => 2,
        "info" => 1,
        _ => 0,
    }
}

/// A list's Notes cell: the flags' headers in one badge, toned by the
/// worst of them.
fn notes_cell(flags: Option<&Vec<Flag>>) -> Value {
    let Some(flags) = flags.filter(|f| !f.is_empty()) else {
        return "".into();
    };
    let worst = flags
        .iter()
        .max_by_key(|f| severity(&f.tone))
        .map_or("info", |f| f.tone.as_str());
    let headers: Vec<&str> = flags.iter().map(|f| f.header.as_str()).collect();
    badge(headers.join(" · "), flag_tone(worst)).into()
}

fn status_badge(status: &str) -> Value {
    let tone = match status {
        "finished" | "finished_issuer" | "finished_contractor" => Tone::Success,
        "outstanding" | "in_progress" => Tone::Warning,
        "rejected" | "failed" | "deleted" | "reversed" => Tone::Danger,
        _ => Tone::Neutral,
    };
    let mut words = status.replace('_', " ");
    if let Some(first) = words.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    badge(words, tone).into()
}

/// How long, in two units: "2d 4h", "4h 13m", "13m".
fn span(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let minutes = (to - from).num_minutes().max(0);
    let (d, h, m) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// How long a contract waited: until now while outstanding, until it was
/// completed once it was.
fn pending(issued: Option<DateTime<Utc>>, completed: Option<DateTime<Utc>>) -> Value {
    match issued {
        Some(from) => span(from, completed.unwrap_or_else(Utc::now)).into(),
        None => "".into(),
    }
}

fn time_value(t: Option<DateTime<Utc>>) -> Value {
    t.map_or_else(|| "".into(), |t| Value::Time(crate::rfc3339(t)))
}

/// Names of characters and corporations, with their kind.
fn entities(ids: &[i64]) -> HashMap<i64, (String, String)> {
    let mut unique: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
    unique.sort_unstable();
    unique.dedup();
    let mut out = HashMap::new();
    for chunk in unique.chunks(1000) {
        if let Ok(named) = tether_plugin_sdk::esi::names(chunk) {
            for n in named {
                out.insert(n.id, (n.name, n.category));
            }
        }
    }
    out
}

/// A character's or corporation's picture and name.
fn entity(names: &HashMap<i64, (String, String)>, id: i64) -> Value {
    match names.get(&id) {
        Some((name, kind)) if kind == "corporation" => corporation(id, name.clone()).into(),
        Some((name, _)) => character(id, name.clone()).into(),
        None => character(id, "Unknown character").into(),
    }
}

fn program_param(request: &Request) -> Option<i64> {
    request.param("program").parse::<i64>().ok()
}

// ---- my statistics, program statistics, all statistics ------------------------

/// The tracked contracts' table: AA's columns, and the program.
fn contracts_table(
    rows: &[Listed],
    flags: &HashMap<i64, Vec<Flag>>,
    names: &HashMap<i64, (String, String)>,
    empty: &str,
) -> Table {
    let mut table = Table::new(vec![
        Column::text("Tracking #"),
        Column::text("Program"),
        Column::text("Issuer"),
        Column::text("Location"),
        Column::numeric("Date issued"),
        Column::numeric("Pending"),
        Column::text("Status"),
        Column::numeric("Price"),
        Column::text("Notes"),
    ]);
    for r in rows.iter().take(MAX_ROWS as usize) {
        table = table.row(vec![
            link(r.number.clone(), format!("tracking/{}", r.number)).into(),
            r.program_name
                .clone()
                .map_or_else(|| "Deleted program".to_owned(), |n| display(&n))
                .into(),
            entity(names, r.issuer),
            r.location.clone().unwrap_or_default().into(),
            time_value(r.issued),
            pending(r.issued, r.completed),
            status_badge(&r.status),
            Value::Isk(r.price),
            notes_cell(flags.get(&r.contract_id)),
        ]);
    }
    table.empty(empty)
}

fn display(name: &str) -> String {
    if name.is_empty() {
        "Unnamed Program".to_owned()
    } else {
        name.to_owned()
    }
}

/// AA's tiles.
fn tile_stats(t: &Tiles) -> Vec<Stat> {
    vec![
        Stat::new("Outstanding contracts", Value::Number(t.outstanding)),
        Stat::new("Outstanding value", Value::Isk(t.outstanding_value)),
        Stat::new("Completed contracts", Value::Number(t.finished)),
        Stat::new("Total bought", Value::Isk(t.bought)),
    ]
}

/// The Outstanding and Finished tabs of a scope.
fn contract_tabs(page: Page, scope: &Scope, program: Option<i64>) -> Result<Page, PageError> {
    let outstanding =
        listed(scope, program, OUTSTANDING).map_err(|e| failed("reading contracts", e))?;
    let finished = listed(scope, program, FINISHED).map_err(|e| failed("reading contracts", e))?;
    let ids: Vec<i64> = outstanding
        .iter()
        .chain(&finished)
        .map(|r| r.contract_id)
        .collect();
    let flags = flags(&ids).map_err(|e| failed("reading flags", e))?;
    let people: Vec<i64> = outstanding
        .iter()
        .chain(&finished)
        .map(|r| r.issuer)
        .collect();
    let names = entities(&people);
    Ok(page
        .tab(
            "Outstanding",
            vec![Section::Table(contracts_table(
                &outstanding,
                &flags,
                &names,
                "No outstanding contracts.",
            ))],
        )
        .tab(
            "Finished",
            vec![Section::Table(contracts_table(
                &finished,
                &flags,
                &names,
                "No finished contracts.",
            ))],
        ))
}

/// My statistics (AA's `my_stats`): contracts the viewer's characters
/// made.
pub fn mine(access: &Access, request: &Request) -> Result<Page, PageError> {
    if !access.basic() {
        return Err(PageError::NotFound);
    }
    let scope = Scope::Issuer(access.character_ids());
    let program = program_param(request);
    let t = tiles(&scope, program).map_err(|e| failed("reading contracts", e))?;
    let mut page = Page::new("My statistics")
        .description("Contracts your characters made to buyback programs.")
        .stats(tile_stats(&t));
    page = with_program_filter(page, &scope)?;
    contract_tabs(page, &scope, program)
}

fn with_program_filter(page: Page, scope: &Scope) -> Result<Page, PageError> {
    let choices = program_choices(scope).map_err(|e| failed("reading programs", e))?;
    if choices.len() < 2 {
        return Ok(page);
    }
    Ok(page.toolbar(Toolbar::new().filter("program", "Program", choices)))
}

/// Untracked contracts: a buyback prefix, but no calculation's number.
struct Untracked {
    contract_id: i64,
    issuer: i64,
    assignee: i64,
    location: Option<String>,
    issued: Option<DateTime<Utc>>,
    status: String,
    title: String,
    price: f64,
}

fn untracked(scope: &Scope) -> Result<(i64, Vec<Untracked>), storage::Error> {
    let filter = format!(
        "c.no_tracking AND NOT c.is_reverse AND c.status = 'outstanding' AND {}",
        scope.sql()
    );
    let count = storage::query(
        &format!("SELECT count(*) FROM contracts c WHERE {filter}"),
        &[scope.ids()],
    )?
    .rows
    .first()
    .map_or(0, |r| int(r, 0));
    let rows = storage::query(
        &format!(
            "SELECT c.contract_id, c.issuer_id, c.assignee_id, c.location_name, c.date_issued, \
                    c.status, c.title, c.price::float8 \
             FROM contracts c WHERE {filter} ORDER BY c.date_issued DESC LIMIT $2"
        ),
        &[scope.ids(), MAX_ROWS.into()],
    )?;
    Ok((
        count,
        rows.rows
            .iter()
            .map(|r| Untracked {
                contract_id: int(r, 0),
                issuer: int(r, 1),
                assignee: int(r, 2),
                location: opt_text(r, 3),
                issued: when(r, 4),
                status: text(r, 5),
                title: text(r, 6),
                price: float(r, 7),
            })
            .collect(),
    ))
}

/// The managers' funding wallets: each program with one, and its balance.
fn wallets_table(access: &Access, all: bool) -> Result<Table, PageError> {
    let rows = storage::query(
        "SELECT w.name, w.division, w.balance::float8, p.name, w.updated_at \
         FROM programs p JOIN wallets w \
              ON w.corporation_id = p.owner_corporation AND w.division = p.wallet_division \
         WHERE $1 OR p.manager_account = $2 \
            OR p.owner_character IN (SELECT jsonb_array_elements_text($3::jsonb)::bigint) \
         ORDER BY w.division, p.name, p.id",
        &[
            all.into(),
            access.account().into(),
            json_ids(&access.character_ids()),
        ],
    )
    .map_err(|e| failed("reading wallets", e))?;
    let mut table = Table::new(vec![
        Column::text("Wallet"),
        Column::numeric("Division"),
        Column::numeric("Current balance"),
        Column::text("Program"),
        Column::numeric("Read"),
    ])
    .title("Corporate funding wallets");
    for r in &rows.rows {
        table = table.row(vec![
            text(r, 0).into(),
            Value::Number(int(r, 1)),
            Value::Isk(float(r, 2)),
            display(&text(r, 3)).into(),
            time_value(when(r, 4)),
        ]);
    }
    Ok(table.empty("No program has a funding wallet."))
}

/// Program statistics (AA's `program_stats`: contracts to the viewer's
/// characters and their corporations) and All statistics
/// (`program_stats_all`: every contract).
pub fn programs(access: &Access, all: bool, request: &Request) -> Result<Page, PageError> {
    let allowed = if all {
        access.can("see_all_statics") || access.manage_all()
    } else {
        access.manager()
    };
    if !allowed {
        return Err(PageError::NotFound);
    }
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let scope = if all {
        Scope::All
    } else {
        let mut ids = access.character_ids();
        ids.extend(access.corporation_ids());
        Scope::Assignee(ids)
    };
    let program = program_param(request);
    let t = tiles(&scope, program).map_err(|e| failed("reading contracts", e))?;
    let (count, scams) = if settings.track_prefill_contracts {
        untracked(&scope).map_err(|e| failed("reading contracts", e))?
    } else {
        (0, Vec::new())
    };
    let mut stats = tile_stats(&t);
    if count > 0 {
        stats.push(Stat::new(
            "Untracked contracts",
            badge(count.to_string(), Tone::Danger),
        ));
    }
    let (title, description) = if all {
        (
            "All statistics",
            "Every program's contracts, untracked contracts and funding wallets.",
        )
    } else {
        (
            "Program statistics",
            "Contracts made to your characters and their corporations.",
        )
    };
    let mut page = Page::new(title).description(description).stats(stats);
    if count > 0 {
        page = page.card(Card::new("Possible scam contracts").description(format!(
            "You have {count} outstanding contracts that start with the buyback prefill text but do not match any tracking objects in your programs! These contracts are possibly scam contracts trying to identify as valid buyback contracts."
        )));
    }
    if access.manager() {
        page = page.card(Card::new("Contracts").field(
            "Read every manager's contracts now",
            action("Refresh contracts", "refresh_contracts"),
        ));
    }
    page = page.table(wallets_table(access, all)?);
    page = with_program_filter(page, &scope)?;
    page = contract_tabs(page, &scope, program)?;
    if settings.track_prefill_contracts {
        page = page.tab("Untracked", vec![Section::Table(untracked_table(&scams)?)]);
    }
    Ok(page)
}

fn untracked_table(rows: &[Untracked]) -> Result<Table, PageError> {
    let ids: Vec<i64> = rows.iter().map(|r| r.contract_id).collect();
    let flags = flags(&ids).map_err(|e| failed("reading flags", e))?;
    let people: Vec<i64> = rows.iter().flat_map(|r| [r.issuer, r.assignee]).collect();
    let names = entities(&people);
    let mut table = Table::new(vec![
        Column::text("Issuer"),
        Column::text("Assignee"),
        Column::text("Location"),
        Column::numeric("Date issued"),
        Column::numeric("Pending"),
        Column::text("Status"),
        Column::text("Description"),
        Column::numeric("Price"),
        Column::text("Notes"),
    ]);
    for r in rows {
        table = table.row(vec![
            entity(&names, r.issuer),
            entity(&names, r.assignee),
            r.location.clone().unwrap_or_default().into(),
            time_value(r.issued),
            pending(r.issued, None),
            status_badge(&r.status),
            r.title.clone().into(),
            Value::Isk(r.price),
            notes_cell(flags.get(&r.contract_id)),
        ]);
    }
    Ok(table.empty("No untracked contracts."))
}

// ---- a contract's details -----------------------------------------------------

/// A calculation, as kept.
struct Tracking {
    id: i64,
    number: String,
    program_id: Option<i64>,
    contract_id: Option<i64>,
    issuer_account: Option<i64>,
    totals: Totals,
}

/// Its contract.
struct Contract {
    id: i64,
    assignee: i64,
    issuer: i64,
    location: Option<String>,
    issued: Option<DateTime<Utc>>,
    completed: Option<DateTime<Utc>>,
    status: String,
    title: String,
    price: f64,
    volume: f64,
    items_read: bool,
}

fn tracking(number: &str) -> Result<Option<Tracking>, storage::Error> {
    let rows = storage::query(
        "SELECT id, tracking_number, program_id, contract_id, issuer_account, value::float8, \
                taxes::float8, hauling_cost::float8, donation::float8, net_price::float8, total_volume \
         FROM trackings WHERE tracking_number = $1",
        &[number.to_owned().into()],
    )?;
    Ok(rows.rows.first().map(|r| {
        let (value, taxes) = (float(r, 5), float(r, 6));
        Tracking {
            id: int(r, 0),
            number: text(r, 1),
            program_id: opt_int(r, 2),
            contract_id: opt_int(r, 3),
            issuer_account: opt_int(r, 4),
            totals: Totals {
                raw: value,
                after_tax: value - taxes,
                taxes,
                hauling: float(r, 7),
                donation: float(r, 8),
                net: float(r, 9),
                volume: float(r, 10),
            },
        }
    }))
}

fn contract(id: i64) -> Result<Option<Contract>, storage::Error> {
    let rows = storage::query(
        "SELECT contract_id, assignee_id, issuer_id, location_name, date_issued, date_completed, \
                status, title, price::float8, volume::float8, items_read \
         FROM contracts WHERE contract_id = $1",
        &[id.into()],
    )?;
    Ok(rows.rows.first().map(|r| Contract {
        id: int(r, 0),
        assignee: int(r, 1),
        issuer: int(r, 2),
        location: opt_text(r, 3),
        issued: when(r, 4),
        completed: when(r, 5),
        status: text(r, 6),
        title: text(r, 7),
        price: float(r, 8),
        volume: float(r, 9),
        items_read: boolean(r, 10),
    }))
}

/// Who may open a contract's details: its seller, its program's managers
/// (and managers it was made to), else anyone with basic access who may
/// use its program, unless the Settings restrict details.
fn may_see_details(
    access: &Access,
    t: &Tracking,
    program: Option<&Program>,
    contract: Option<&Contract>,
    restricted: bool,
) -> bool {
    let characters = access.character_ids();
    let seller = t.issuer_account == Some(access.account())
        || contract.is_some_and(|c| characters.contains(&c.issuer));
    let receiver = access.manager()
        && contract.is_some_and(|c| {
            characters.contains(&c.assignee) || access.corporation_ids().contains(&c.assignee)
        });
    let manager = access.manage_all() || program.is_some_and(|p| p.editable_by(access));
    if seller || receiver || manager {
        return true;
    }
    !restricted && access.basic() && program.is_some_and(|p| p.visible_to(access))
}

/// A calculation's items: (type, quantity, unit value), most first.
fn tracking_items(tracking: i64) -> Result<Vec<(i64, i64, f64)>, storage::Error> {
    Ok(storage::query(
        "SELECT type_id, quantity, buy_value::float8 FROM tracking_items WHERE tracking_id = $1 \
         ORDER BY quantity DESC, type_id",
        &[tracking.into()],
    )?
    .rows
    .iter()
    .map(|r| (int(r, 0), int(r, 1), float(r, 2)))
    .collect())
}

fn contract_items(contract: i64) -> Result<Vec<(i64, i64)>, storage::Error> {
    Ok(storage::query(
        "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1 \
         ORDER BY quantity DESC, type_id",
        &[contract.into()],
    )?
    .rows
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect())
}

/// AA's per-item notes on the Original calculation tab: what the contract
/// lacks.
fn calculation_note(
    name: &str,
    type_id: i64,
    wanted: &BTreeMap<i64, i64>,
    got: &BTreeMap<i64, i64>,
) -> Option<(String, Tone)> {
    match got.get(&type_id) {
        None => Some((
            format!("{name} is missing from the created contract"),
            Tone::Danger,
        )),
        Some(q) if wanted.get(&type_id) != Some(q) => Some((
            format!("Quantity for {name} in the contract does not match the calculation"),
            Tone::Danger,
        )),
        Some(_) => None,
    }
}

/// And on the Contract items tab: what the calculation lacks.
fn contract_note(
    name: &str,
    type_id: i64,
    wanted: &BTreeMap<i64, i64>,
    got: &BTreeMap<i64, i64>,
) -> Option<(String, Tone)> {
    match wanted.get(&type_id) {
        None => Some((
            format!("{name} is missing from the original calculation"),
            Tone::Warning,
        )),
        Some(q) if got.get(&type_id) != Some(q) => Some((
            format!("Quantity for {name} in the calculation does not match the tracking"),
            Tone::Warning,
        )),
        Some(_) => None,
    }
}

fn note_value(note: Option<(String, Tone)>) -> Value {
    note.map_or_else(|| "".into(), |(text, tone)| badge(text, tone).into())
}

fn type_name(types: &HashMap<i64, statics::TypeInfo>, id: i64) -> String {
    types
        .get(&id)
        .map_or_else(|| format!("Unknown item {id}"), |t| t.name.clone())
}

/// The contract's details (AA's `contract_details`): before a contract,
/// the calculation with how to make it; then the contract, its flags, and
/// its items against the calculation.
pub fn details(access: &Access, number: &str) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let t = tracking(number)
        .map_err(|e| failed("reading the calculation", e))?
        .ok_or(PageError::NotFound)?;
    let program = match t.program_id {
        Some(id) => programs::get(id).map_err(|e| failed("reading the program", e))?,
        None => None,
    };
    let contract = match t.contract_id {
        Some(id) => contract(id).map_err(|e| failed("reading the contract", e))?,
        None => None,
    };
    if !may_see_details(
        access,
        &t,
        program.as_ref(),
        contract.as_ref(),
        settings.restrict_tracking_details,
    ) {
        return Err(PageError::NotFound);
    }
    let items = tracking_items(t.id).map_err(|e| failed("reading the calculation", e))?;
    let got = match &contract {
        Some(c) => contract_items(c.id).map_err(|e| failed("reading the contract", e))?,
        None => Vec::new(),
    };
    let mut ids: Vec<i64> = items.iter().map(|(id, _, _)| *id).collect();
    ids.extend(got.iter().map(|(id, _)| *id));
    let types = statics::by_ids(&ids).unwrap_or_default();
    let donation_percent = if t.totals.after_tax > 0.0 && t.totals.donation > 0.0 {
        (t.totals.donation / t.totals.after_tax * 10_000.0).round() / 100.0
    } else {
        0.0
    };
    let mut page = Page::new(format!("Tracking {}", t.number));
    if let Some(p) = &program {
        page = page.description(format!("A calculation for {}.", p.display_name()));
    }
    let Some(c) = contract else {
        // No contract yet: the calculation, and how to make it.
        page = match &program {
            Some(p) => {
                let owner = calculator::owner_name(p);
                let locations = calculator::location_names(p.id)?;
                page.stats(calculator::summary(&owner, &t.totals, &t.number))
                    .card(calculator::instructions(
                        &owner,
                        &t.totals,
                        &t.number,
                        &p.expiration,
                    ))
                    .card(calculator::invoice(
                        &t.totals,
                        &settings,
                        p,
                        &locations,
                        donation_percent,
                    ))
            }
            None => page
                .stats(vec![Stat::new("Tracking number", t.number.clone())])
                .card(invoice(&t.totals)),
        };
        return Ok(page.table(calculation_table(&items, &types, None).title("Calculated items")));
    };
    let names = entities(&[c.issuer, c.assignee]);
    let now = Utc::now();
    let info = Card::new("Contract information")
        .field("Date issued", time_value(c.issued))
        .field("Issued from", entity(&names, c.issuer))
        .field("Issued to", entity(&names, c.assignee))
        .field(
            "Location",
            c.location.clone().unwrap_or_else(|| "Unknown".to_owned()),
        )
        .field(
            "Time pending",
            c.issued
                .map_or_else(String::new, |from| span(from, c.completed.unwrap_or(now))),
        )
        .field("Status", status_badge(&c.status))
        .field("Tracking #", c.title.clone())
        .field("Asking price", Value::Isk(c.price))
        .field("Volume", format!("{} m³", isk_text(c.volume)));
    page = page.card(info).card(invoice(&t.totals));
    let flags = flags(&[c.id]).map_err(|e| failed("reading flags", e))?;
    for f in flags.get(&c.id).into_iter().flatten() {
        page = page.card(
            Card::new(f.header.clone())
                .description(f.message.clone())
                .field("Check", badge(check_word(&f.tone), flag_tone(&f.tone))),
        );
    }
    let wanted = crate::sync::merged(&items.iter().map(|(t, q, _)| (*t, *q)).collect::<Vec<_>>());
    let have = crate::sync::merged(&got);
    let compare = c.items_read.then_some((&wanted, &have));
    let mut contract_table = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Quantity"),
        Column::text("Notes"),
    ]);
    for (type_id, quantity) in &got {
        let name = type_name(&types, *type_id);
        contract_table = contract_table.row(vec![
            item_type(*type_id, name.clone()).into(),
            Value::Number(*quantity),
            note_value(compare.and_then(|(w, h)| contract_note(&name, *type_id, w, h))),
        ]);
    }
    let contract_table = contract_table.empty(if c.items_read {
        "The contract has no items."
    } else {
        "The contract's items haven't been read yet."
    });
    Ok(page
        .tab(
            "Original calculation",
            vec![Section::Table(calculation_table(&items, &types, compare))],
        )
        .tab("Contract items", vec![Section::Table(contract_table)]))
}

/// A flag's word on its card.
fn check_word(tone: &str) -> &'static str {
    match tone {
        "danger" => "Problem",
        "warning" => "Check",
        "success" => "Good",
        "watch" => "Review",
        _ => "Note",
    }
}

/// The calculated items, with what the contract lacks once it's read.
fn calculation_table(
    items: &[(i64, i64, f64)],
    types: &HashMap<i64, statics::TypeInfo>,
    compare: Compare<'_>,
) -> Table {
    let mut table = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Quantity"),
        Column::numeric("Buy value"),
        Column::text("Notes"),
    ]);
    for (type_id, quantity, unit) in items {
        let name = type_name(types, *type_id);
        table = table.row(vec![
            item_type(*type_id, name.clone()).into(),
            Value::Number(*quantity),
            Value::Isk(*unit),
            note_value(compare.and_then(|(w, h)| calculation_note(&name, *type_id, w, h))),
        ]);
    }
    table.empty("No items.")
}

/// AA's invoice on the details page.
fn invoice(t: &Totals) -> Card {
    let mut card = Card::new("Invoice")
        .field("Price before expenses", Value::Isk(t.raw))
        .field("Program taxes", Value::Isk(t.taxes))
        .field("Hauling cost", Value::Isk(t.hauling));
    if t.donation > 0.0 {
        card = card.field("Donation", Value::Isk(t.donation));
    }
    card.field("Net price", Value::Isk(t.net.max(0.0)))
}

// ---- leaderboard -------------------------------------------------------------

/// "2026-10" as "October 2026".
fn month_label(month: &str) -> String {
    month_start(month).map_or_else(|| month.to_owned(), |d| d.format("%B %Y").to_string())
}

fn month_start(month: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").ok()
}

/// A program the viewer may see the leaderboard of: one they may use
/// (with basic access, as AA's view; `see_leaderboard` only lists the
/// link), or manage (B2: AA showed any program's).
fn leaderboard_program(access: &Access, id: i64) -> Result<Program, PageError> {
    let p = programs::get(id)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if (access.basic() && p.visible_to(access)) || p.editable_by(access) {
        Ok(p)
    } else {
        Err(PageError::NotFound)
    }
}

/// A program's leaderboard (AA's `leaderboard`): who sold the most to it,
/// month by month, by finished contracts.
pub fn leaderboard(access: &Access, id: i64, request: &Request) -> Result<Page, PageError> {
    let program = leaderboard_program(access, id)?;
    let rows = storage::query(
        "SELECT to_char(date_trunc('month', c.date_issued AT TIME ZONE 'UTC'), 'YYYY-MM'), \
                c.issuer_id, sum(c.price)::float8, sum(t.donation)::float8 \
         FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
         WHERE t.program_id = $1 AND c.status = 'finished' \
         GROUP BY 1, 2",
        &[program.id.into()],
    )
    .map_err(|e| failed("reading contracts", e))?;
    let mut by_month: BTreeMap<String, Vec<(i64, f64, f64)>> = BTreeMap::new();
    for r in &rows.rows {
        by_month
            .entry(text(r, 0))
            .or_default()
            .push((int(r, 1), float(r, 2), float(r, 3)));
    }
    let months: Vec<&String> = by_month.keys().collect();
    let asked = request.param("month");
    let shown = months
        .iter()
        .position(|m| m.as_str() == asked)
        .or_else(|| months.len().checked_sub(1));
    let page = Page::new(format!("{} leaderboard", program.display_name()))
        .description("Who sold the most to this program, by finished contracts, month by month.");
    let columns = vec![
        Column::text("Rank"),
        Column::text("Name"),
        Column::numeric("Contract total"),
        Column::numeric("Donation total"),
    ];
    let Some(at) = shown else {
        return Ok(page.table(Table::new(columns).empty("No data available.")));
    };
    let month = months[at].clone();
    let mut sellers = by_month.get(&month).cloned().unwrap_or_default();
    sellers.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    // The table's rows: the top sellers, and the month's totals.
    let shown = sellers.len().min(MAX_ROWS as usize - 1);
    let names = entities(&sellers[..shown].iter().map(|s| s.0).collect::<Vec<_>>());
    let path = format!("program/{}/leaderboard", program.id);
    let mut stats = vec![Stat::new("Month", month_label(&month))];
    if let Some(prev) = at.checked_sub(1).and_then(|i| months.get(i)) {
        stats.push(Stat::new(
            "Previous",
            link(month_label(prev), format!("{path}?month={prev}")),
        ));
    }
    if let Some(next) = months.get(at + 1) {
        stats.push(Stat::new(
            "Next",
            link(month_label(next), format!("{path}?month={next}")),
        ));
    }
    let (total, donations) = sellers
        .iter()
        .fold((0.0, 0.0), |(a, b), s| (a + s.1, b + s.2));
    stats.push(Stat::new("Sellers", Value::Number(sellers.len() as i64)));
    stats.push(Stat::new("Contract total", Value::Isk(total)));
    stats.push(Stat::new("Donation total", Value::Isk(donations)));
    let mut table = Table::new(columns).title(month_label(&month));
    for (rank, (seller, sold, donated)) in sellers[..shown].iter().enumerate() {
        let rank = rank + 1;
        let place: Value = match rank {
            1 => badge("1st", Tone::Accent).into(),
            2 => badge("2nd", Tone::Neutral).into(),
            3 => badge("3rd", Tone::Warning).into(),
            n => Value::Number(n as i64),
        };
        table = table.row(vec![
            place,
            entity(&names, *seller),
            Value::Isk(*sold),
            Value::Isk(*donated),
        ]);
    }
    table = table.row(vec![
        "".into(),
        "Total".into(),
        Value::Isk(total),
        Value::Isk(donations),
    ]);
    Ok(page.stats(stats).table(table))
}

// ---- performance ---------------------------------------------------------------

/// A program the viewer may see the performance of: AA's
/// `can_see_performance_test` (`see_performance` or a manager), and a
/// program they may use or manage (B2).
fn performance_program(access: &Access, id: i64) -> Result<Program, PageError> {
    let p = programs::get(id)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    let may = (access.can("see_performance") || access.manager())
        && (p.visible_to(access) || p.editable_by(access));
    if may { Ok(p) } else { Err(PageError::NotFound) }
}

/// AA's scale for a series: its mean month over a billion is shown in
/// billions, over a million in millions, else as it is.
fn scale(values: &[f64]) -> (f64, &'static str) {
    if values.is_empty() {
        return (1.0, "");
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if mean > 1e9 {
        (1e9, "Billions")
    } else if mean > 1e6 {
        (1e6, "Millions")
    } else {
        (1.0, "")
    }
}

/// A scaled value as AA shows it (3 decimals), or ISK unscaled.
fn scaled(v: f64, by: f64) -> Value {
    if by > 1.0 {
        Value::Text(format!("{:.3}", v / by))
    } else {
        Value::Text(isk_text(v))
    }
}

fn scaled_label(what: &str, label: &str) -> String {
    if label.is_empty() {
        format!("{what} (ISK)")
    } else {
        format!("{what} ({label} of ISK)")
    }
}

/// Every month from the first to the last ("YYYY-MM"), none skipped.
fn month_range(first: &str, last: &str) -> Vec<String> {
    let (Some(mut d), Some(end)) = (month_start(first), month_start(last)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while d <= end && out.len() < 1200 {
        out.push(format!("{:04}-{:02}", d.year(), d.month()));
        d = if d.month() == 12 {
            NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
        }
        .unwrap_or(end + chrono::Days::new(1));
    }
    out
}

/// A month's totals: bought ISK and contracts, donations and donating
/// contracts.
#[derive(Default, Clone, Copy)]
struct Month {
    bought: f64,
    contracts: i64,
    donations: f64,
    donating: i64,
}

/// The calculation's and the contract's quantities by type, to compare.
type Compare<'a> = Option<(&'a BTreeMap<i64, i64>, &'a BTreeMap<i64, i64>)>;

/// A CSV row: contract, issued, finished, seller, its price, and an
/// item's type, quantity and unit value.
type ExportRow = (i64, String, String, i64, f64, i64, i64, f64);

/// ISK (quantity × unit value, B8) and quantity of each type by month.
type ItemMonths = BTreeMap<(String, i64), (f64, i64)>;

fn performance_data(program: i64) -> Result<(BTreeMap<String, Month>, ItemMonths), PageError> {
    let rows = storage::query(
        "SELECT to_char(date_trunc('month', c.date_issued AT TIME ZONE 'UTC'), 'YYYY-MM'), \
                sum(c.price)::float8, count(*), sum(t.donation)::float8, \
                count(*) FILTER (WHERE t.donation > 0) \
         FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
         WHERE t.program_id = $1 AND c.status = 'finished' GROUP BY 1",
        &[program.into()],
    )
    .map_err(|e| failed("reading contracts", e))?;
    let months = rows
        .rows
        .iter()
        .map(|r| {
            (
                text(r, 0),
                Month {
                    bought: float(r, 1),
                    contracts: int(r, 2),
                    donations: float(r, 3),
                    donating: int(r, 4),
                },
            )
        })
        .collect();
    let rows = storage::query(
        "SELECT to_char(date_trunc('month', c.date_issued AT TIME ZONE 'UTC'), 'YYYY-MM'), \
                ti.type_id, sum(ti.quantity * ti.buy_value)::float8, sum(ti.quantity)::bigint \
         FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
         JOIN tracking_items ti ON ti.tracking_id = t.id \
         WHERE t.program_id = $1 AND c.status = 'finished' GROUP BY 1, 2",
        &[program.into()],
    )
    .map_err(|e| failed("reading items", e))?;
    let items = rows
        .rows
        .iter()
        .map(|r| ((text(r, 0), int(r, 1)), (float(r, 2), int(r, 3))))
        .collect();
    Ok((months, items))
}

/// One line of a series table: ISK and quantity by month, and their sums.
#[derive(Default, Clone)]
struct Series {
    by_month: BTreeMap<String, (f64, i64)>,
}

impl Series {
    fn add(&mut self, month: &str, isk: f64, quantity: i64) {
        let e = self.by_month.entry(month.to_owned()).or_default();
        e.0 += isk;
        e.1 += quantity;
    }

    fn isk(&self) -> f64 {
        self.by_month.values().map(|v| v.0).sum()
    }

    fn quantity(&self) -> i64 {
        self.by_month.values().map(|v| v.1).sum()
    }

    /// AA's `lastthree`: the last three months' ISK.
    fn last_three(&self, months: &[String]) -> f64 {
        months
            .iter()
            .rev()
            .take(3)
            .filter_map(|m| self.by_month.get(m))
            .map(|v| v.0)
            .sum()
    }

    /// Every month's ISK, zero-filled (for the scale).
    fn monthly(&self, months: &[String]) -> Vec<f64> {
        months
            .iter()
            .map(|m| self.by_month.get(m).map_or(0.0, |v| v.0))
            .collect()
    }
}

/// A program's performance (AA's `performance`): bought ISK and donations
/// per month, per item group and per item of a group (`?group=`), as
/// tables with AA's scaling, and the CSV.
pub fn performance(access: &Access, id: i64, request: &Request) -> Result<Page, PageError> {
    let program = performance_program(access, id)?;
    let (months_data, items) = performance_data(program.id)?;
    let path = format!("program/{}/performance", program.id);
    let mut page = Page::new(format!("{} performance", program.display_name()))
        .description("Finished contracts by month, item group and item.");
    let export = export_card(access, program.id);
    let (Some(first), Some(last)) = (months_data.keys().next(), months_data.keys().last()) else {
        let mut page = page.table(
            Table::new(vec![Column::text("Month"), Column::numeric("Bought (ISK)")])
                .title("By month")
                .empty("No finished contracts yet."),
        );
        if let Some(card) = export {
            page = page.card(card);
        }
        return Ok(page);
    };
    let months = month_range(first, last);
    let monthly: Vec<Month> = months
        .iter()
        .map(|m| months_data.get(m).copied().unwrap_or_default())
        .collect();
    let totals = monthly.iter().fold(Month::default(), |a, m| Month {
        bought: a.bought + m.bought,
        contracts: a.contracts + m.contracts,
        donations: a.donations + m.donations,
        donating: a.donating + m.donating,
    });
    page = page.stats(vec![
        Stat::new("Bought", Value::Isk(totals.bought)),
        Stat::new("Contracts", Value::Number(totals.contracts)),
        Stat::new("Donations", Value::Isk(totals.donations)),
        Stat::new("Donating contracts", Value::Number(totals.donating)),
    ]);
    // Overall: bought and donations by month, on the bought scale.
    let (by, label) = scale(&monthly.iter().map(|m| m.bought).collect::<Vec<_>>());
    let mut table = Table::new(vec![
        Column::text("Month"),
        Column::numeric(scaled_label("Bought", label)),
        Column::numeric("Contracts"),
        Column::numeric(scaled_label("Donations", label)),
        Column::numeric("Donating contracts"),
    ])
    .title("By month");
    for (month, m) in months.iter().zip(&monthly).rev() {
        table = table.row(vec![
            month_label(month).into(),
            scaled(m.bought, by),
            Value::Number(m.contracts),
            scaled(m.donations, by),
            Value::Number(m.donating),
        ]);
    }
    page = page.table(table);

    // Item groups.
    let type_ids: Vec<i64> = items.keys().map(|(_, t)| *t).collect();
    let types = statics::by_ids(&type_ids).map_err(|e| failed("reading item data", e))?;
    let group_of = |t: i64| -> (i64, String) {
        types.get(&t).map_or_else(
            || (0, "Unknown group".to_owned()),
            |i| (i.group_id, i.group_name.clone()),
        )
    };
    let mut groups: BTreeMap<i64, (String, Series)> = BTreeMap::new();
    for ((month, t), (isk, quantity)) in &items {
        let (gid, gname) = group_of(*t);
        groups
            .entry(gid)
            .or_insert_with(|| (gname, Series::default()))
            .1
            .add(month, *isk, *quantity);
    }
    let all_cells: Vec<f64> = groups
        .values()
        .flat_map(|(_, s)| s.monthly(&months))
        .collect();
    let (by, label) = scale(&all_cells);
    let mut ordered: Vec<(&i64, &(String, Series))> = groups.iter().collect();
    ordered.sort_by(|a, b| b.1.1.isk().total_cmp(&a.1.1.isk()).then(a.0.cmp(b.0)));
    let mut table = Table::new(vec![
        Column::text("Item group"),
        Column::numeric(scaled_label("Bought", label)),
        Column::numeric("Quantity"),
        Column::numeric(scaled_label("Last 3 months", label)),
    ])
    .title("By item group");
    for (gid, (name, series)) in &ordered {
        table = table.row(vec![
            link(name.clone(), format!("{path}?group={gid}")).into(),
            scaled(series.isk(), by),
            Value::Number(series.quantity()),
            scaled(series.last_three(&months), by),
        ]);
    }
    page = page.table(table).toolbar(
        Toolbar::new().filter(
            "group",
            "Item group",
            ordered
                .iter()
                .take(100)
                .map(|(gid, (name, _))| (gid.to_string(), name.clone()))
                .collect(),
        ),
    );

    // One group: its months, and its items.
    let chosen = request
        .param("group")
        .parse::<i64>()
        .ok()
        .and_then(|g| groups.get(&g).map(|v| (g, v)));
    if let Some((gid, (name, series))) = chosen {
        let (by, label) = scale(&series.monthly(&months));
        let mut table = Table::new(vec![
            Column::text("Month"),
            Column::numeric(scaled_label("Bought", label)),
            Column::numeric("Quantity"),
        ])
        .title(format!("{name} by month"));
        for m in months.iter().rev() {
            let (isk, quantity) = series.by_month.get(m).copied().unwrap_or_default();
            table = table.row(vec![
                month_label(m).into(),
                scaled(isk, by),
                Value::Number(quantity),
            ]);
        }
        page = page.table(table);
        let mut per_item: BTreeMap<i64, Series> = BTreeMap::new();
        for ((month, t), (isk, quantity)) in &items {
            if group_of(*t).0 == gid {
                per_item.entry(*t).or_default().add(month, *isk, *quantity);
            }
        }
        let cells: Vec<f64> = per_item.values().flat_map(|s| s.monthly(&months)).collect();
        let (by, label) = scale(&cells);
        let mut ordered: Vec<(&i64, &Series)> = per_item.iter().collect();
        ordered.sort_by(|a, b| b.1.isk().total_cmp(&a.1.isk()).then(a.0.cmp(b.0)));
        let mut table = Table::new(vec![
            Column::text("Item"),
            Column::numeric(scaled_label("Bought", label)),
            Column::numeric("Quantity"),
            Column::numeric(scaled_label("Last 3 months", label)),
        ])
        .title(format!("{name} items"));
        for (t, series) in ordered {
            table = table.row(vec![
                item_type(*t, type_name(&types, *t)).into(),
                scaled(series.isk(), by),
                Value::Number(series.quantity()),
                scaled(series.last_three(&months), by),
            ]);
        }
        page = page.table(table);
    }
    if let Some(card) = export {
        page = page.card(card);
    }
    Ok(page)
}

/// The CSV's name: one a program.
fn export_name(program: i64) -> String {
    format!("performance-{program}")
}

/// The export, for those who may download it (`see_performance`, the
/// file's permission): build it, and the last one built.
fn export_card(access: &Access, program: i64) -> Option<Card> {
    if !access.can("see_performance") {
        return None;
    }
    let name = export_name(program);
    let mut card = Card::new("Export").field(
        "Every finished contract's items, as CSV",
        action("Build CSV", "export"),
    );
    if let Some(file) = downloads::files().into_iter().find(|f| f.name == name) {
        card = card.field(
            format!("{} rows, built {}", file.rows, file.built_at),
            link("buyback.csv", format!("downloads/{name}")),
        );
    }
    Some(card)
}

/// AA's export columns (its "Contract ID" was the calculation's id; this
/// is the contract's, and "Object ISK" is the row's, B8).
const EXPORT_HEADER: [&str; 10] = [
    "Contract ID",
    "Date Issued",
    "Date Finished",
    "User ID",
    "Total ISK",
    "Object Category",
    "Object ID",
    "Object Name",
    "Object Quant",
    "Object ISK",
];

/// Builds a program's CSV: every finished contract's calculated items.
fn export(access: &Access, program_id: i64) -> Result<SubmitResult, PageError> {
    let program = performance_program(access, program_id)?;
    if !access.can("see_performance") {
        return Err(PageError::Forbidden);
    }
    let mut rows: Vec<ExportRow> = Vec::new();
    while rows.len() < MAX_EXPORT_ROWS {
        let chunk = storage::query(
            "SELECT c.contract_id, c.date_issued, c.date_completed, c.issuer_id, c.price::float8, \
                    ti.type_id, ti.quantity, ti.buy_value::float8 \
             FROM trackings t JOIN contracts c ON c.contract_id = t.contract_id \
             JOIN tracking_items ti ON ti.tracking_id = t.id \
             WHERE t.program_id = $1 AND c.status = 'finished' \
             ORDER BY c.date_issued, c.contract_id, ti.type_id, ti.quantity, ti.buy_value \
             LIMIT $2 OFFSET $3",
            &[
                program.id.into(),
                (READ_CHUNK as i64).into(),
                (rows.len() as i64).into(),
            ],
        )
        .map_err(|e| failed("reading contracts", e))?;
        let got = chunk.rows.len();
        rows.extend(chunk.rows.iter().map(|r| {
            (
                int(r, 0),
                when(r, 1).map(crate::rfc3339).unwrap_or_default(),
                when(r, 2).map(crate::rfc3339).unwrap_or_default(),
                int(r, 3),
                float(r, 4),
                int(r, 5),
                int(r, 6),
                float(r, 7),
            )
        }));
        if got < READ_CHUNK {
            break;
        }
    }
    let types = statics::by_ids(&rows.iter().map(|r| r.5).collect::<Vec<_>>())
        .map_err(|e| failed("reading item data", e))?;
    let name = export_name(program.id);
    let mut title = format!("Buyback performance: {}", program.display_name());
    title = title.chars().take(100).collect();
    let header: Vec<String> = EXPORT_HEADER.iter().map(|h| (*h).to_owned()).collect();
    let build = downloads::begin(&name, &title, "see_performance", &header)
        .map_err(|e| failed("starting the CSV", e))?;
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(
            |(contract, issued, finished, issuer, total, t, quantity, unit)| {
                let info = types.get(t);
                vec![
                    contract.to_string(),
                    issued.clone(),
                    finished.clone(),
                    issuer.to_string(),
                    format!("{total:.2}"),
                    info.map(|i| i.group_name.clone()).unwrap_or_default(),
                    t.to_string(),
                    info.map(|i| i.name.clone()).unwrap_or_default(),
                    quantity.to_string(),
                    format!("{:.2}", *quantity as f64 * unit),
                ]
            },
        )
        .collect();
    for part in cells.chunks(downloads::MAX_ROWS_PER_APPEND) {
        downloads::append(&name, build, part).map_err(|e| failed("writing the CSV", e))?;
    }
    downloads::finish(&name, build).map_err(|e| failed("finishing the CSV", e))?;
    log::info(format!(
        "performance CSV of program {} built by {} ({}): {} rows",
        program.id,
        access.viewer.main.name,
        access.viewer.main.id,
        cells.len()
    ));
    Ok(SubmitResult::Redirect(format!(
        "program/{}/performance",
        program.id
    )))
}

// ---- forms -----------------------------------------------------------------------

/// The statistics pages' actions: Refresh contracts (AA's
/// `manual_contract_sync`) and the performance CSV.
pub fn submit(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let path = s.request.path.as_str();
    let parts: Vec<&str> = path.split('/').collect();
    match (parts.as_slice(), s.form.as_str()) {
        (["program-stats" | "all-stats"], "refresh_contracts") => {
            if !access.manager() {
                return Err(PageError::Forbidden);
            }
            jobs::enqueue(NewJob::new("contracts").key("contracts"))
                .map_err(|e| failed("queuing", e))?;
            log::info(format!(
                "contracts queued by {} ({})",
                access.viewer.main.name, access.viewer.main.id
            ));
            Ok(SubmitResult::Redirect(path.to_owned()))
        }
        (["program", id, "performance"], "export") => export(access, crate::pages::id_of(id)?),
        _ => Err(PageError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)] // test code

    use super::*;

    #[test]
    fn scales_as_aa_does() {
        assert_eq!(scale(&[2e9, 4e9]), (1e9, "Billions"));
        assert_eq!(scale(&[2e6, 0.0]), (1.0, ""));
        assert_eq!(scale(&[5e6, 0.0]), (1e6, "Millions"));
        assert_eq!(scale(&[1000.0]), (1.0, ""));
        assert_eq!(scale(&[]), (1.0, ""));
        let shown = |v: Value| match v {
            Value::Text(t) => t,
            _ => panic!("not text"),
        };
        assert_eq!(shown(scaled(1_234_567_890.0, 1e9)), "1.235");
        assert_eq!(shown(scaled(1234.5, 1.0)), "1 234.50");
        assert_eq!(
            scaled_label("Bought", "Millions"),
            "Bought (Millions of ISK)"
        );
        assert_eq!(scaled_label("Bought", ""), "Bought (ISK)");
    }

    #[test]
    fn months_are_zero_filled() {
        assert_eq!(
            month_range("2025-11", "2026-02"),
            vec!["2025-11", "2025-12", "2026-01", "2026-02"]
        );
        assert_eq!(month_range("2026-02", "2026-02"), vec!["2026-02"]);
        assert!(month_range("nonsense", "2026-02").is_empty());
        assert_eq!(month_label("2026-10"), "October 2026");
    }

    #[test]
    fn last_three_months() {
        let months: Vec<String> = month_range("2026-01", "2026-05");
        let mut s = Series::default();
        s.add("2026-01", 100.0, 1);
        s.add("2026-03", 10.0, 1);
        s.add("2026-05", 1.0, 1);
        s.add("2026-05", 2.0, 2);
        assert_eq!(s.isk(), 113.0);
        assert_eq!(s.quantity(), 5);
        assert_eq!(s.last_three(&months), 13.0);
        assert_eq!(s.monthly(&months), vec![100.0, 0.0, 10.0, 0.0, 3.0]);
    }

    #[test]
    fn spans_in_two_units() {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert_eq!(
            span(t("2026-10-01T00:00:00Z"), t("2026-10-03T04:30:00Z")),
            "2d 4h"
        );
        assert_eq!(
            span(t("2026-10-01T00:00:00Z"), t("2026-10-01T04:13:00Z")),
            "4h 13m"
        );
        assert_eq!(
            span(t("2026-10-01T00:00:00Z"), t("2026-10-01T00:13:00Z")),
            "13m"
        );
    }

    #[test]
    fn mismatch_notes() {
        let wanted: BTreeMap<i64, i64> = [(1, 10), (2, 5)].into_iter().collect();
        let got: BTreeMap<i64, i64> = [(1, 10), (2, 4), (3, 1)].into_iter().collect();
        assert_eq!(calculation_note("A", 1, &wanted, &got), None);
        assert_eq!(
            calculation_note("B", 2, &wanted, &got),
            Some((
                "Quantity for B in the contract does not match the calculation".into(),
                Tone::Danger
            ))
        );
        let lacking: BTreeMap<i64, i64> = [(1, 10)].into_iter().collect();
        assert_eq!(
            calculation_note("B", 2, &wanted, &lacking),
            Some((
                "B is missing from the created contract".into(),
                Tone::Danger
            ))
        );
        assert_eq!(
            contract_note("C", 3, &wanted, &got),
            Some((
                "C is missing from the original calculation".into(),
                Tone::Warning
            ))
        );
        assert_eq!(
            contract_note("B", 2, &wanted, &got),
            Some((
                "Quantity for B in the calculation does not match the tracking".into(),
                Tone::Warning
            ))
        );
    }

    #[test]
    fn statuses_read_as_words() {
        let read = |v: Value| match v {
            Value::Badge(b) => (b.label, b.tone),
            _ => panic!("not a badge"),
        };
        assert_eq!(
            read(status_badge("finished_issuer")),
            ("Finished issuer".to_owned(), Tone::Success)
        );
        assert_eq!(
            read(status_badge("outstanding")),
            ("Outstanding".to_owned(), Tone::Warning)
        );
    }
}
