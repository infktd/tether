//! Reverse buyback (aa-buybackprogram's reverse programs): members buy
//! from a corporation hangar's stock.
//!
//! - **Programs** (`reverse`): the reverse programs the viewer may use
//!   (public ones for every pilot who can log in; else `basic_access` and
//!   the program's groups and states), each opening its stock picker.
//! - **The picker** (`reverse/<id>`): the program's stock with what's
//!   free after others' reservations, priced with its markup; Add puts an
//!   item in the pilot's cart (kept per account and program, since apps
//!   ship no JavaScript), and Check total price turns the cart into a
//!   request with a tracking number, reserving its items.
//! - **A request** (`reverse/tracking/<number>`): what to pay, how to make
//!   the contract (the buyer issues it to the manager, asking for the
//!   items), the item list to copy in EVE's multibuy format, and Release;
//!   once its contract comes, the contract and its checks.
//! - **Stock** (the `hangars` schedule, every 30 minutes, and Refresh
//!   stock): what the manager's corporation keeps in a hangar division at
//!   the program's structures, or in chosen containers there, from the
//!   host's `corporation-hangar-assets` (every page of the assets, read in
//!   the background; asked again a minute on until it's ready).
//! - **Contracts**: read with the normal ones (`sync::contracts`); one
//!   titled with a request's tracking number is kept, its requested items
//!   read once, checked against the request and flagged, and its status
//!   followed until it's finished or rejected (B23).
//!
//! AA's bugs fixed, its rules kept: hangar division 7 is CorpSAG7 (B13),
//! no zero-quantity rows (B18), the buyer's notes are stored (B19), the
//! card links to the reverse tracking page (B21).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Duration, Utc};
use serde_json::{Value as Json, json};
use tether_plugin_sdk::esi::{self, Character, Subject};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, CodeBlock, Column, Field, Form, Page, PageError, Request, Section, SettingsForm,
    SettingsGroup, Stat, Submission, SubmitResult, Table, Tone, Toolbar, Value, action, actions,
    add_owner, badge, discord, identity, item_type, link, log, time,
};

use crate::pages::id_of;
use crate::pricing::PriceType;
use crate::programs::{self, Location};
use crate::sync::Fetched;
use crate::{
    Access, Restrictions, Settings, boolean, failed, float, int, isk_text, json_ids, opt_int,
    opt_text, retry, settings, text, when,
};

/// The stock read's follow-up, while Tether still reads a corporation's
/// assets in the background.
const HANGARS_AGAIN: &str = "hangars_again";
/// The schedule (and Refresh stock's job).
const HANGARS: &str = "hangars";
/// Follow-ups a minute apart before the stock read gives up until the
/// next schedule.
const MAX_FOLLOW_UPS: i64 = 30;
/// Rows a table sends (the host pages them).
const MAX_ROWS: usize = 500;
/// Structures one stock read asks about (the host's limit).
const MAX_STRUCTURES: usize = 100;
/// Containers the editor offers (a settings group's 30 fields; the
/// form's 120 in all).
const MAX_CONTAINERS: usize = 30;
const EXPIRATIONS: &[&str] = &["1 Day", "3 Days", "1 Week", "2 Weeks", "4 Weeks"];
const PRICE_TYPES: &[&str] = &["Buy", "Sell", "Split"];

// ---- reverse programs --------------------------------------------------------

/// A reverse program.
#[derive(Debug, Clone)]
pub(crate) struct ReverseProgram {
    pub id: i64,
    pub name: String,
    pub tracking_prefill: String,
    pub owner_character: i64,
    pub owner_corporation: i64,
    pub manager_account: i64,
    pub is_corporation: bool,
    pub stock_source: String,
    pub hangar_division: Option<i64>,
    pub expiration: String,
    pub price_type: String,
    pub markup: i64,
    pub restricted_groups: Vec<i64>,
    pub restricted_states: Vec<String>,
    pub is_public: bool,
    pub notify_manager: bool,
    pub discord_channel: Option<String>,
    pub stock_synced_at: Option<String>,
}

const COLUMNS: &str = "id, name, tracking_prefill, owner_character, owner_corporation, \
    manager_account, is_corporation, stock_source, hangar_division, expiration, price_type, \
    markup, to_jsonb(restricted_groups)::text, to_jsonb(restricted_states)::text, is_public, \
    notify_manager, discord_channel, stock_synced_at";

fn json_list<T: serde::de::DeserializeOwned>(row: &[Db], i: usize) -> Vec<T> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default()
}

fn program(r: &[Db]) -> ReverseProgram {
    ReverseProgram {
        id: int(r, 0),
        name: text(r, 1),
        tracking_prefill: text(r, 2),
        owner_character: int(r, 3),
        owner_corporation: int(r, 4),
        manager_account: int(r, 5),
        is_corporation: boolean(r, 6),
        stock_source: text(r, 7),
        hangar_division: opt_int(r, 8),
        expiration: text(r, 9),
        price_type: text(r, 10),
        markup: int(r, 11),
        restricted_groups: json_list(r, 12),
        restricted_states: json_list(r, 13),
        is_public: boolean(r, 14),
        notify_manager: boolean(r, 15),
        discord_channel: opt_text(r, 16),
        stock_synced_at: when(r, 17).map(crate::rfc3339),
    }
}

fn get(id: i64) -> Result<Option<ReverseProgram>, storage::Error> {
    Ok(storage::query(
        &format!("SELECT {COLUMNS} FROM reverse_programs WHERE id = $1"),
        &[id.into()],
    )?
    .rows
    .first()
    .map(|r| program(r)))
}

fn all() -> Result<Vec<ReverseProgram>, storage::Error> {
    Ok(storage::query(
        &format!("SELECT {COLUMNS} FROM reverse_programs ORDER BY name, id"),
        &[],
    )?
    .rows
    .iter()
    .map(|r| program(r))
    .collect())
}

impl ReverseProgram {
    fn restrictions(&self) -> Restrictions {
        Restrictions {
            is_public: self.is_public,
            manager_account: self.manager_account,
            groups: self.restricted_groups.clone(),
            states: self.restricted_states.clone(),
        }
    }

    /// AA's reverse index: every program for those who manage them all,
    /// else the visibility rule (public: every pilot who can log in).
    fn visible_to(&self, access: &Access) -> bool {
        access.manage_all() || access.may_use(&self.restrictions())
    }

    fn editable_by(&self, access: &Access) -> bool {
        access.manages(self.manager_account, self.owner_character)
    }

    fn prefill(&self, global: &str) -> String {
        if self.tracking_prefill.trim().is_empty() {
            global.trim().to_owned()
        } else {
            self.tracking_prefill.trim().to_owned()
        }
    }

    /// Who contracts go to: the manager's corporation or character.
    fn owner_id(&self) -> i64 {
        if self.is_corporation {
            self.owner_corporation
        } else {
            self.owner_character
        }
    }

    /// AA's unit price: the market price by the program's price type, with
    /// its markup, to the cent. No taxes, static or NPC prices.
    fn unit_price(&self, price: Option<&crate::pricing::Price>) -> f64 {
        let base = price.map_or(0.0, |p| self.kind().pick(p.buy, p.sell));
        (base * (1.0 + self.markup as f64 / 100.0) * 100.0).round() / 100.0
    }

    fn kind(&self) -> PriceType {
        PriceType::parse(&self.price_type)
    }

    fn terms(&self) -> String {
        let mut terms = vec![format!(
            "{} price {}",
            self.kind().label(),
            markup(self.markup)
        )];
        if self.is_corporation {
            terms.push("contracts to the corporation".to_owned());
        }
        if self.is_public {
            terms.push("open to every pilot".to_owned());
        }
        terms.join(" · ")
    }
}

fn markup(m: i64) -> String {
    if m > 0 {
        format!("+{m}%")
    } else {
        format!("{m}%")
    }
}

fn visible(access: &Access, id: i64) -> Result<ReverseProgram, PageError> {
    let p = get(id)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if !p.visible_to(access) {
        return Err(PageError::NotFound);
    }
    Ok(p)
}

/// Names for ids (characters, corporations); unknown ones go without.
fn names_of(ids: &[i64]) -> HashMap<i64, String> {
    let mut ids: Vec<i64> = ids.iter().copied().filter(|i| *i > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = HashMap::new();
    for chunk in ids.chunks(1000) {
        if let Ok(names) = esi::names(chunk) {
            out.extend(names.into_iter().map(|n| (n.id, n.name)));
        }
    }
    out
}

fn name_or_id(names: &HashMap<i64, String>, id: i64) -> String {
    names.get(&id).cloned().unwrap_or_else(|| id.to_string())
}

/// Location names ("<system>: <name>"), by location id.
fn location_labels(locations: &[Location]) -> HashMap<i64, String> {
    let systems: Vec<i64> = locations.iter().filter_map(|l| l.system_id).collect();
    let names: HashMap<i64, String> = crate::statics::systems(&systems)
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    locations
        .iter()
        .map(|l| {
            (
                l.id,
                l.display(l.system_id.and_then(|s| names.get(&s)).map(String::as_str)),
            )
        })
        .collect()
}

fn program_places(id: i64) -> Result<Vec<Location>, storage::Error> {
    programs::program_locations(id, true)
}

fn place_names(id: i64) -> Result<Vec<String>, PageError> {
    let places = program_places(id).map_err(|e| failed("reading locations", e))?;
    let labels = location_labels(&places);
    Ok(places
        .iter()
        .filter_map(|l| labels.get(&l.id).cloned())
        .collect())
}

/// The reverse pages' own pages, as chips under the views bar.
fn chips(page: Page, access: &Access) -> Page {
    let mut page = page.link("Programs", "reverse");
    if access.basic() {
        page = page.link("My statistics", "reverse/stats");
    }
    if access.manager() {
        page = page.link("Program statistics", "reverse/program-stats");
    }
    page
}

fn off() -> Page {
    Page::new("Reverse buyback").card(
        Card::new("Reverse buyback is off")
            .description("An admin turned reverse buyback off in the app's Settings."),
    )
}

/// A unit price to the cent (tables abbreviate ISK to whole numbers).
fn unit_text(v: f64) -> Value {
    format!("{} ISK", isk_text(v)).into()
}

/// ISK to the whole number, as AA's "I will pay".
fn whole_isk(v: f64) -> String {
    let t = isk_text(v.round());
    t.strip_suffix(".00").map_or(t.clone(), str::to_owned)
}

fn tone(t: &str) -> Tone {
    match t {
        "danger" => Tone::Danger,
        "warning" => Tone::Warning,
        "success" => Tone::Success,
        _ => Tone::Neutral,
    }
}

fn status_badge(status: &str) -> Value {
    let t = match status {
        "finished" => Tone::Success,
        "outstanding" | "in_progress" => Tone::Warning,
        "rejected" | "deleted" | "failed" | "cancelled" => Tone::Danger,
        _ => Tone::Neutral,
    };
    badge(status.replace('_', " "), t).into()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---- pages -----------------------------------------------------------------

pub fn render(access: &Access, request: &Request) -> Result<Page, PageError> {
    let parts: Vec<&str> = request.path.split('/').collect();
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    match parts.as_slice() {
        ["manage", "reverse"] => manage_list(access, &settings),
        ["manage", "reverse", "new"] => editor(access, None, request, None, None),
        ["manage", "reverse", id] => editor(access, Some(id_of(id)?), request, None, None),
        ["reverse", ..] if !settings.reverse_enabled => Ok(off()),
        ["reverse"] => index(access),
        ["reverse", "stats"] => my_stats(access, &settings),
        ["reverse", "program-stats"] => program_stats(access),
        ["reverse", "tracking", number] => tracking_page(access, &settings, number, request),
        ["reverse", id] => picker(access, id_of(id)?, request),
        _ => Err(PageError::NotFound),
    }
}

pub fn submit(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let parts: Vec<&str> = s.request.path.split('/').collect();
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    match (parts.as_slice(), s.form.as_str()) {
        (["manage", "reverse"], "delete_reverse") => delete(access, s),
        (["manage", "reverse", ..], "refresh_stock") => {
            queue(access, HANGARS, &s.request.path, json!({ "all": true }))
        }
        (["manage", "reverse"], "refresh_contracts") => {
            queue(access, "contracts", "manage/reverse", json!({}))
        }
        (["manage", "reverse", "new"], "reverse_program") => save(access, None, s),
        (["manage", "reverse", id], "reverse_program") => save(access, Some(id_of(id)?), s),
        (["reverse", ..], _) if !settings.reverse_enabled => Err(PageError::NotFound),
        (["reverse", "stats"], "release") => release(access, s, Some("reverse/stats")),
        (["reverse", "program-stats"], "remove_request") => remove_request(access, s),
        (["reverse", "program-stats"], "refresh_contracts") => {
            queue(access, "contracts", "reverse/program-stats", json!({}))
        }
        (["reverse", "tracking", _], "release") => release(access, s, None),
        (["reverse", id], "add") => add_to_cart(access, id_of(id)?, s),
        (["reverse", id], "remove_cart") => remove_from_cart(access, id_of(id)?, s),
        (["reverse", id], "checkout") => checkout(access, &settings, id_of(id)?, s),
        _ => Err(PageError::NotFound),
    }
}

fn queue(access: &Access, job: &str, back: &str, payload: Json) -> Result<SubmitResult, PageError> {
    if !access.manager() {
        return Err(PageError::Forbidden);
    }
    jobs::enqueue(NewJob::new(job).key(job).payload(payload.to_string()))
        .map_err(|e| failed("queuing", e))?;
    log::info(format!(
        "reverse buyback: {job} queued by {} ({})",
        access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect(back.to_owned()))
}

/// AA's reverse index: the programs the viewer may buy from.
fn index(access: &Access) -> Result<Page, PageError> {
    let list: Vec<ReverseProgram> = all()
        .map_err(|e| failed("reading programs", e))?
        .into_iter()
        .filter(|p| p.visible_to(access))
        .collect();
    let names = names_of(
        &list
            .iter()
            .map(ReverseProgram::owner_id)
            .collect::<Vec<_>>(),
    );
    let edit = list.iter().any(|p| p.editable_by(access));
    let mut columns = vec![
        Column::text("Program"),
        Column::text("Manager"),
        Column::text("Locations"),
        Column::text("Prices"),
        Column::numeric("Item types"),
    ];
    if edit {
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns);
    for p in &list {
        let places = place_names(p.id)?;
        let places = if places.len() > 4 {
            format!("{} locations", places.len())
        } else {
            places.join(", ")
        };
        let mut row: Vec<Value> = vec![
            link(p.name.clone(), format!("reverse/{}", p.id)).into(),
            name_or_id(&names, p.owner_id()).into(),
            places.into(),
            p.terms().into(),
            Value::Number(crate::pages::count(
                "SELECT count(*) FROM hangar_stock WHERE program_id = $1",
                p.id,
            )),
        ];
        if edit {
            row.push(if p.editable_by(access) {
                link("Edit", format!("manage/reverse/{}", p.id)).into()
            } else {
                "".into()
            });
        }
        table = table.row(row);
    }
    Ok(chips(
        Page::new("Reverse buyback")
            .description(
                "Buy from corporation hangar stock: pick items, check the price, then contract the tracking number to the program's manager.",
            )
            .table(table.empty("No reverse programs you may use yet.")),
        access,
    ))
}

// ---- stock and the cart ------------------------------------------------------

/// A type in a program's stock, with what's free and its price.
#[derive(Debug, Clone)]
struct StockRow {
    type_id: i64,
    name: String,
    group: String,
    category: String,
    stock: i64,
    available: i64,
    unit: f64,
}

/// The program's stock with what others reserved taken off (requests
/// without a contract, or with one still outstanding), priced; prices
/// fetched for types never priced when `fetch` (a form post or a job).
fn stock_rows(p: &ReverseProgram, fetch: bool) -> Result<Vec<StockRow>, String> {
    let stock = storage::query(
        "SELECT type_id, quantity FROM hangar_stock WHERE program_id = $1",
        &[p.id.into()],
    )
    .map_err(|e| format!("reading the stock: {e:?}"))?;
    let reserved: HashMap<i64, i64> = storage::query(
        "SELECT i.type_id, sum(i.quantity)::bigint FROM reverse_tracking_items i \
         JOIN reverse_trackings t ON t.id = i.tracking_id \
         LEFT JOIN contracts c ON c.contract_id = t.contract_id \
         WHERE t.program_id = $1 AND (t.contract_id IS NULL OR c.status = 'outstanding') \
         GROUP BY i.type_id",
        &[p.id.into()],
    )
    .map_err(|e| format!("reading reservations: {e:?}"))?
    .rows
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect();
    let ids: Vec<i64> = stock.rows.iter().map(|r| int(r, 0)).collect();
    let info = crate::statics::by_ids(&ids).map_err(|e| format!("reading item data: {e:?}"))?;
    let prices = crate::prices::get(&ids, fetch)?;
    let mut rows: Vec<StockRow> = stock
        .rows
        .iter()
        .map(|r| {
            let id = int(r, 0);
            let t = info.get(&id);
            let quantity = int(r, 1);
            StockRow {
                type_id: id,
                name: t.map_or_else(|| id.to_string(), |t| t.name.clone()),
                group: t.map(|t| t.group_name.clone()).unwrap_or_default(),
                category: t.map(|t| t.category_name.clone()).unwrap_or_default(),
                stock: quantity,
                available: (quantity - reserved.get(&id).copied().unwrap_or(0)).max(0),
                unit: p.unit_price(prices.get(&id)),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

/// The viewer's cart in a program: (type, quantity), oldest first.
fn cart(account: i64, program_id: i64) -> Result<Vec<(i64, i64)>, PageError> {
    Ok(storage::query(
        "SELECT type_id, quantity FROM reverse_carts WHERE account_id = $1 AND program_id = $2 \
         ORDER BY type_id",
        &[account.into(), program_id.into()],
    )
    .map_err(|e| failed("reading the cart", e))?
    .rows
    .iter()
    .map(|r| (int(r, 0), int(r, 1)))
    .collect())
}

fn owner_name(p: &ReverseProgram) -> String {
    name_or_id(&names_of(&[p.owner_id()]), p.owner_id())
}

/// AA's reverse calculator: the stock to pick from, and the cart.
fn picker(access: &Access, id: i64, request: &Request) -> Result<Page, PageError> {
    let p = visible(access, id)?;
    let rows = stock_rows(&p, false).map_err(|e| failed("reading the stock", e))?;
    let owner = owner_name(&p);
    let mut categories: Vec<String> = rows
        .iter()
        .map(|r| r.category.clone())
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort();
    categories.dedup();
    let q = request.search().to_lowercase();
    let category = request.param("category");
    let shown: Vec<&StockRow> = rows
        .iter()
        .filter(|r| {
            q.is_empty()
                || r.name.to_lowercase().contains(&q)
                || r.group.to_lowercase().contains(&q)
        })
        .filter(|r| category.is_empty() || r.category == category)
        .collect();
    let mut stock = Table::new(vec![
        Column::text("Item"),
        Column::text("Group"),
        Column::text("Category"),
        Column::numeric("In stock"),
        Column::numeric("Available"),
        Column::numeric("Unit price"),
        Column::text(""),
    ])
    .title("Stock");
    let mut adds = false;
    for r in shown.iter().take(MAX_ROWS) {
        let add: Value = if r.available > 0 {
            adds = true;
            action("Add", "add")
                .field("type_id", r.type_id.to_string())
                .confirm(format!(
                    "How many {}? {} available at {} ISK each.",
                    r.name,
                    r.available,
                    isk_text(r.unit)
                ))
                .into()
        } else {
            badge("Reserved", Tone::Neutral).into()
        };
        stock = stock.row(vec![
            item_type(r.type_id, r.name.clone()).into(),
            r.group.clone().into(),
            r.category.clone().into(),
            Value::Number(r.stock),
            Value::Number(r.available),
            unit_text(r.unit),
            add,
        ]);
    }
    let empty = if rows.is_empty() {
        "No stock yet. It's read from the corporation's hangars every 30 minutes."
    } else {
        "No items match."
    };
    let places = place_names(p.id)?;
    let mut terms = Card::new("This program")
        .field(
            "Prices",
            format!(
                "Based on {} {} prices, {} markup",
                settings()
                    .map(|s| s.price_source_name)
                    .unwrap_or_else(|_| "Jita".to_owned()),
                p.price_type,
                markup(p.markup)
            ),
        )
        .field(
            "Contracts to",
            if p.is_corporation {
                format!("{owner} (the manager's corporation)")
            } else {
                owner.clone()
            },
        )
        .field("Locations", places.join(", "))
        .field("Expiration", p.expiration.clone());
    if p.is_public {
        terms = terms.field("Open to", "Every pilot who can log in.");
    }
    terms = terms.field(
        "Stock read",
        p.stock_synced_at
            .clone()
            .map_or_else(|| "Not yet".into(), time),
    );
    let mut toolbar = Toolbar::new().search("Search items and groups");
    if !categories.is_empty() {
        toolbar = toolbar.filter(
            "category",
            "Category",
            categories.iter().map(|c| (c.clone(), c.clone())).collect(),
        );
    }
    let mut page = Page::new(format!("Buy from {}", p.name))
        .description(format!("Managed by {owner}."))
        .toolbar(toolbar);
    if request.param("unavailable") == "1" {
        page = page.card(Card::new("Nothing was available").description(
            "Everything in your cart was reserved by others, so no request was made. Your cart is empty again.",
        ));
    }
    page = page.card(terms).table(stock.empty(empty));
    let items = cart(access.account(), p.id)?;
    if !items.is_empty() {
        let by_type: HashMap<i64, &StockRow> = rows.iter().map(|r| (r.type_id, r)).collect();
        let mut table = Table::new(vec![
            Column::text("Item"),
            Column::numeric("Quantity"),
            Column::numeric("Available"),
            Column::numeric("Price"),
            Column::numeric("Total"),
            Column::text(""),
        ])
        .title("Your cart");
        let mut total = 0.0;
        for (type_id, quantity) in &items {
            let row = by_type.get(type_id);
            let unit = row.map_or(0.0, |r| r.unit);
            total += unit * *quantity as f64;
            table = table.row(vec![
                item_type(
                    *type_id,
                    row.map_or_else(|| type_id.to_string(), |r| r.name.clone()),
                )
                .into(),
                Value::Number(*quantity),
                Value::Number(row.map_or(0, |r| r.available)),
                unit_text(unit),
                Value::Isk(unit * *quantity as f64),
                action("Remove", "remove_cart")
                    .field("type_id", type_id.to_string())
                    .tone(Tone::Danger)
                    .into(),
            ]);
        }
        page = page
            .stats(vec![Stat::new("Grand total", Value::Isk(total))])
            .table(table)
            .form(
                Form::new("checkout", "Check total price")
                    .title("Check out")
                    .description("Quantities above what's free after others' reservations are lowered. The items are then reserved for you.")
                    .field(Field::textarea("notes", "Notes for the manager", 1000)),
            );
    }
    if adds {
        // Opened by each row's Add, in a popup.
        page = page.form(
            Form::new("add", "Add to cart").field(
                Field::number("quantity", "Quantity")
                    .value("1")
                    .range(Some(1.0), None, true)
                    .required(),
            ),
        );
    }
    Ok(chips(page, access))
}

/// Back to the picker, keeping its search and filter.
fn back_to_picker(id: i64, request: &Request) -> String {
    let kept: Vec<String> = ["q", "category"]
        .iter()
        .filter(|k| !request.param(k).is_empty())
        .map(|k| format!("{k}={}", encode(request.param(k))))
        .collect();
    if kept.is_empty() {
        format!("reverse/{id}")
    } else {
        format!("reverse/{id}?{}", kept.join("&"))
    }
}

fn add_to_cart(access: &Access, id: i64, s: &Submission) -> Result<SubmitResult, PageError> {
    let p = visible(access, id)?;
    let type_id: i64 = id_of(s.value("type_id"))?;
    let quantity = s
        .value("quantity")
        .parse::<i64>()
        .ok()
        .filter(|q| *q > 0)
        .ok_or(PageError::NotFound)?;
    let rows = stock_rows(&p, false).map_err(|e| failed("reading the stock", e))?;
    let row = rows
        .iter()
        .find(|r| r.type_id == type_id)
        .ok_or(PageError::NotFound)?;
    let already = cart(access.account(), p.id)?
        .into_iter()
        .find(|(t, _)| *t == type_id)
        .map_or(0, |(_, q)| q);
    let wanted = already.saturating_add(quantity).min(row.available);
    if wanted > 0 {
        storage::execute(
            "INSERT INTO reverse_carts (account_id, program_id, type_id, quantity) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (account_id, program_id, type_id) DO UPDATE SET quantity = EXCLUDED.quantity",
            &[
                access.account().into(),
                p.id.into(),
                type_id.into(),
                wanted.into(),
            ],
        )
        .map_err(|e| failed("adding to the cart", e))?;
    }
    Ok(SubmitResult::Redirect(back_to_picker(p.id, &s.request)))
}

fn remove_from_cart(access: &Access, id: i64, s: &Submission) -> Result<SubmitResult, PageError> {
    let p = visible(access, id)?;
    storage::execute(
        "DELETE FROM reverse_carts WHERE account_id = $1 AND program_id = $2 AND type_id = $3",
        &[
            access.account().into(),
            p.id.into(),
            id_of(s.value("type_id"))?.into(),
        ],
    )
    .map_err(|e| failed("removing from the cart", e))?;
    Ok(SubmitResult::Redirect(back_to_picker(p.id, &s.request)))
}

/// AA's Check total price: the cart priced with the markup and clamped to
/// what's free, kept as a request with a tracking number (only rows with
/// something in them, B18; the buyer's notes too, B19), and the cart
/// emptied.
fn checkout(
    access: &Access,
    settings: &Settings,
    id: i64,
    s: &Submission,
) -> Result<SubmitResult, PageError> {
    let p = visible(access, id)?;
    let items = cart(access.account(), p.id)?;
    if items.is_empty() {
        return Ok(SubmitResult::Redirect(format!("reverse/{}", p.id)));
    }
    let rows = stock_rows(&p, true).map_err(|e| failed("pricing", e))?;
    let mut lines: Vec<Json> = Vec::new();
    let mut lowered: Vec<String> = Vec::new();
    let mut total = 0.0;
    for (type_id, wanted) in &items {
        let Some(row) = rows.iter().find(|r| r.type_id == *type_id) else {
            lowered.push(format!("{type_id}.{wanted}"));
            continue;
        };
        let quantity = (*wanted).min(row.available);
        if quantity < *wanted {
            lowered.push(format!("{type_id}.{wanted}"));
        }
        if quantity <= 0 {
            continue;
        }
        total += row.unit * quantity as f64;
        lines.push(json!({ "type_id": type_id, "quantity": quantity, "buy_value": row.unit }));
    }
    if lines.is_empty() {
        storage::execute(
            "DELETE FROM reverse_carts WHERE account_id = $1 AND program_id = $2",
            &[access.account().into(), p.id.into()],
        )
        .map_err(|e| failed("emptying the cart", e))?;
        return Ok(SubmitResult::Redirect(format!(
            "reverse/{}?unavailable=1",
            p.id
        )));
    }
    let number = crate::calculator::tracking_number(
        &p.prefill(&settings.tracking_prefill),
        "reverse_trackings",
        true,
    )
    .map_err(|e| failed("numbering the request", e))?;
    let notes: String = s.value("notes").trim().chars().take(1000).collect();
    let character = identity::acting().map_or(access.viewer.main.id, |c| c.id);
    storage::transaction(&[
        Statement::new(
            "INSERT INTO reverse_trackings (program_id, issuer_account, issuer_character, net_price, \
                 tracking_number, notes) VALUES ($1, $2, $3, $4, $5, nullif($6, ''))",
            vec![
                p.id.into(),
                access.account().into(),
                character.into(),
                total.into(),
                number.clone().into(),
                notes.into(),
            ],
        ),
        Statement::new(
            "INSERT INTO reverse_tracking_items (tracking_id, type_id, quantity, buy_value) \
             SELECT (SELECT id FROM reverse_trackings WHERE tracking_number = $2), type_id, quantity, buy_value \
             FROM jsonb_to_recordset($1::jsonb) AS x(type_id bigint, quantity bigint, buy_value numeric)",
            vec![Db::json(Json::Array(lines).to_string()), number.clone().into()],
        ),
        Statement::new(
            "DELETE FROM reverse_carts WHERE account_id = $1 AND program_id = $2",
            vec![access.account().into(), p.id.into()],
        ),
    ])
    .map_err(|e| failed("keeping the request", e))?;
    log::info(format!(
        "reverse request {number} made in program {} by {} ({})",
        p.id, access.viewer.main.name, access.viewer.main.id
    ));
    let mut to = format!("reverse/tracking/{}", encode(&number));
    // What was lowered, for the request's page: "<type>.<asked>" pairs.
    let mut list = String::new();
    for l in &lowered {
        if list.len() + l.len() + 1 > 120 {
            break;
        }
        if !list.is_empty() {
            list.push(',');
        }
        list.push_str(l);
    }
    if !list.is_empty() {
        to.push_str(&format!("?lowered={}", encode(&list)));
    }
    Ok(SubmitResult::Redirect(to))
}

// ---- a request ---------------------------------------------------------------

/// A reverse request (AA's ReverseTracking).
struct Tracking {
    id: i64,
    program_id: Option<i64>,
    contract_id: Option<i64>,
    issuer_account: Option<i64>,
    issuer_character: Option<i64>,
    net_price: f64,
    number: String,
    created_at: String,
    notes: Option<String>,
}

const TRACKING_COLUMNS: &str = "id, program_id, contract_id, issuer_account, issuer_character, \
    net_price::float8, tracking_number, created_at, notes";

fn tracking(r: &[Db]) -> Tracking {
    Tracking {
        id: int(r, 0),
        program_id: opt_int(r, 1),
        contract_id: opt_int(r, 2),
        issuer_account: opt_int(r, 3),
        issuer_character: opt_int(r, 4),
        net_price: float(r, 5),
        number: text(r, 6),
        created_at: when(r, 7).map(crate::rfc3339).unwrap_or_default(),
        notes: opt_text(r, 8).filter(|n| !n.trim().is_empty()),
    }
}

fn tracking_by(column: &str, value: Db) -> Result<Option<Tracking>, PageError> {
    Ok(storage::query(
        &format!("SELECT {TRACKING_COLUMNS} FROM reverse_trackings WHERE {column} = $1"),
        &[value],
    )
    .map_err(|e| failed("reading the request", e))?
    .rows
    .first()
    .map(|r| tracking(r)))
}

/// A request's items: (type, quantity, unit price), most first.
fn tracking_items(id: i64) -> Result<Vec<(i64, i64, f64)>, PageError> {
    Ok(storage::query(
        "SELECT type_id, quantity, buy_value::float8 FROM reverse_tracking_items \
         WHERE tracking_id = $1 ORDER BY quantity DESC, type_id",
        &[id.into()],
    )
    .map_err(|e| failed("reading the items", e))?
    .rows
    .iter()
    .map(|r| (int(r, 0), int(r, 1), float(r, 2)))
    .collect())
}

fn type_names(ids: &[i64]) -> HashMap<i64, String> {
    crate::statics::by_ids(ids)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, t)| (id, t.name))
        .collect()
}

/// A request: what to pay and how to contract it, its items to copy and
/// Release, until its contract comes; then the contract and its checks
/// (AA's reverse contract details).
fn tracking_page(
    access: &Access,
    settings: &Settings,
    number: &str,
    request: &Request,
) -> Result<Page, PageError> {
    let t = tracking_by("tracking_number", number.to_owned().into())?.ok_or(PageError::NotFound)?;
    let program = match t.program_id {
        Some(id) => get(id).map_err(|e| failed("reading the program", e))?,
        None => None,
    };
    let issuer = t.issuer_account == Some(access.account());
    let manager = access.manage_all() || program.as_ref().is_some_and(|p| p.editable_by(access));
    let allowed = if settings.restrict_tracking_details {
        issuer || manager
    } else {
        issuer || manager || access.basic()
    };
    if !allowed {
        return Err(PageError::NotFound);
    }
    let items = tracking_items(t.id)?;
    let mut ids: Vec<i64> = items.iter().map(|(i, _, _)| *i).collect();
    let lowered: Vec<(i64, i64)> = request
        .param("lowered")
        .split(',')
        .filter_map(|p| {
            let (t, q) = p.split_once('.')?;
            Some((t.parse().ok()?, q.parse().ok()?))
        })
        .collect();
    ids.extend(lowered.iter().map(|(t, _)| *t));
    let contract_items: Vec<(i64, i64)> = match t.contract_id {
        Some(c) => crate::sync::pairs(
            "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1",
            c,
        )
        .map_err(|e| failed("reading the contract's items", e))?,
        None => Vec::new(),
    };
    ids.extend(contract_items.iter().map(|(t, _)| *t));
    let names = type_names(&ids);
    let name = |id: i64| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let program_name = program
        .as_ref()
        .map_or_else(|| "Deleted program".to_owned(), |p| p.name.clone());
    let mut request_card = Card::new("Request")
        .field("Program", program_name)
        .field("Requested at", time(t.created_at.clone()))
        .field("Calculated price", Value::Isk(t.net_price));
    if let Some(n) = &t.notes {
        request_card = request_card.field("Buyer notes", n.clone());
    }
    let mut page = Page::new(format!("Request {}", t.number));
    match t.contract_id {
        None => {
            let owner = program.as_ref().map(owner_name).unwrap_or_default();
            page = page.stats(vec![
                Stat::new("Availability", owner.clone()),
                Stat::new("I will pay", Value::Isk(t.net_price.round())),
                Stat::new("Tracking number", t.number.clone()),
            ]);
            if !lowered.is_empty() {
                let mut card = Card::new("Some quantities were lowered");
                for (type_id, asked) in &lowered {
                    let got = items
                        .iter()
                        .find(|(i, _, _)| i == type_id)
                        .map_or(0, |(_, q, _)| *q);
                    card = card.field(
                        name(*type_id),
                        if got > 0 {
                            format!(
                                "Requested {asked} but only {got} available after reservations."
                            )
                        } else {
                            format!("Requested {asked} but none available after reservations.")
                        },
                    );
                }
                page = page.card(card);
            }
            page = page.card(Card::new("Reserved for you").description(
                if settings.purge_hours > 0 {
                    format!(
                        "The items below are reserved for you for {} hours. If no contract with your tracking number is found by then, the reservation is released automatically.",
                        settings.purge_hours
                    )
                } else {
                    "The items below are reserved for you until you release them or the contract is completed.".to_owned()
                },
            ));
            if let Some(p) = &program {
                page = page.card(instructions(p, &owner, t.net_price, &t.number));
            }
            let mut table = Table::new(vec![
                Column::text("Item"),
                Column::numeric("Quantity"),
                Column::numeric("Unit price"),
                Column::numeric("Total"),
            ])
            .title("Requested items");
            let mut list = String::new();
            for (type_id, quantity, unit) in &items {
                table = table.row(vec![
                    item_type(*type_id, name(*type_id)).into(),
                    Value::Number(*quantity),
                    unit_text(*unit),
                    Value::Isk(unit * *quantity as f64),
                ]);
                let line = format!("{} {quantity}\n", name(*type_id));
                if list.len() + line.len() <= 16_000 {
                    list.push_str(&line);
                }
            }
            page = page.table(table.empty("No items."));
            page = page.code(
                CodeBlock::new(list)
                    .title("Item list (EVE multibuy)")
                    .copy_label("Copy item list"),
            );
            page = page.card(request_card);
            if issuer {
                page = page.card(
                    Card::new("Changed your mind?").field(
                        "",
                        action("Release reserved items", "release")
                            .field("request", t.id.to_string())
                            .tone(Tone::Danger)
                            .confirm("The reserved items become available to others again, and this tracking number stops working."),
                    ),
                );
            }
        }
        Some(contract_id) => {
            page = contract_sections(
                page,
                contract_id,
                request_card,
                &items,
                &contract_items,
                &name,
            )?;
        }
    }
    Ok(chips(page, access))
}

/// AA's reverse instructions: the buyer issues the contract to the
/// manager, paying the price and asking for the items.
fn instructions(p: &ReverseProgram, owner: &str, total: f64, number: &str) -> Card {
    Card::new("How to create the contract")
        .field("1", "Open contracts in game and click Create Contract")
        .field("2", format!("Select Item Exchange and search for {owner}"))
        .field("3", "Tick Also request items from buyer")
        .field(
            "4",
            format!(
                "On the price page set I will pay to {} ISK",
                whole_isk(total)
            ),
        )
        .field("5", "Under I will receive, add every item listed below")
        .field(
            "6",
            format!("Set the contract description to your tracking number: {number}"),
        )
        .field(
            "7",
            format!("Set expiration to {} and submit", p.expiration),
        )
}

/// A matched request's contract: its facts, its flags, and the requested
/// items set against the contract's.
fn contract_sections(
    mut page: Page,
    contract_id: i64,
    request_card: Card,
    items: &[(i64, i64, f64)],
    contract_items: &[(i64, i64)],
    name: &dyn Fn(i64) -> String,
) -> Result<Page, PageError> {
    let rows = storage::query(
        "SELECT date_issued, issuer_id, assignee_id, location_name, status, title, \
                price::float8, volume::float8, items_read FROM contracts WHERE contract_id = $1",
        &[contract_id.into()],
    )
    .map_err(|e| failed("reading the contract", e))?;
    let Some(c) = rows.rows.first() else {
        return Ok(page.card(request_card));
    };
    let people = names_of(&[int(c, 1), int(c, 2)]);
    page = page.card(
        Card::new("Contract")
            .field(
                "Date issued",
                time(when(c, 0).map(crate::rfc3339).unwrap_or_default()),
            )
            .field("Issued from", name_or_id(&people, int(c, 1)))
            .field("Issued to", name_or_id(&people, int(c, 2)))
            .field(
                "Location",
                opt_text(c, 3).unwrap_or_else(|| "Unknown".to_owned()),
            )
            .field("Status", status_badge(&text(c, 4)))
            .field("Title", text(c, 5))
            .field("Price", Value::Isk(float(c, 6)))
            .field("Volume", format!("{} m³", isk_text(float(c, 7)))),
    );
    page = page.card(request_card);
    let flags = storage::query(
        "SELECT tone, header, message FROM contract_flags WHERE contract_id = $1 ORDER BY id",
        &[contract_id.into()],
    )
    .map_err(|e| failed("reading the contract's notes", e))?;
    if !flags.rows.is_empty() {
        let mut table =
            Table::new(vec![Column::text("Note"), Column::text("Details")]).title("Contract notes");
        for f in &flags.rows {
            table = table.row(vec![
                badge(text(f, 1), tone(&text(f, 0))).into(),
                text(f, 2).into(),
            ]);
        }
        page = page.table(table);
    }
    let wanted = crate::sync::merged(&items.iter().map(|(t, q, _)| (*t, *q)).collect::<Vec<_>>());
    let got = crate::sync::merged(contract_items);
    let mut requested = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Quantity"),
        Column::numeric("Unit price"),
        Column::text("Notes"),
    ]);
    for (type_id, quantity, unit) in items {
        let note: Value = match got.get(type_id) {
            None if boolean(c, 8) => badge(
                format!("{} is missing from the created contract", name(*type_id)),
                Tone::Danger,
            )
            .into(),
            Some(q) if q != wanted.get(type_id).unwrap_or(&0) => badge(
                format!(
                    "Quantity for {} in the contract does not match the request",
                    name(*type_id)
                ),
                Tone::Danger,
            )
            .into(),
            _ => "".into(),
        };
        requested = requested.row(vec![
            item_type(*type_id, name(*type_id)).into(),
            Value::Number(*quantity),
            unit_text(*unit),
            note,
        ]);
    }
    let mut in_contract = Table::new(vec![
        Column::text("Item"),
        Column::numeric("Quantity"),
        Column::text("Notes"),
    ]);
    for (type_id, quantity) in &got {
        let note: Value = match wanted.get(type_id) {
            None => badge(
                format!("{} is missing from the original request", name(*type_id)),
                Tone::Warning,
            )
            .into(),
            Some(q) if q != quantity => badge(
                format!(
                    "Quantity for {} in the request does not match the contract",
                    name(*type_id)
                ),
                Tone::Warning,
            )
            .into(),
            _ => "".into(),
        };
        in_contract = in_contract.row(vec![
            item_type(*type_id, name(*type_id)).into(),
            Value::Number(*quantity),
            note,
        ]);
    }
    let contract_empty = if boolean(c, 8) {
        "The contract asks for no items."
    } else {
        "The contract's items are read on the next contract sync."
    };
    Ok(page
        .tab(
            "Requested items",
            vec![Section::Table(requested.empty("No items."))],
        )
        .tab(
            "Contract items",
            vec![Section::Table(in_contract.empty(contract_empty))],
        ))
}

/// Releases the viewer's own request that has no contract yet (AA's
/// release_reverse_tracking): its items are free again.
fn release(access: &Access, s: &Submission, back: Option<&str>) -> Result<SubmitResult, PageError> {
    let t = tracking_by("id", id_of(s.value("request"))?.into())?.ok_or(PageError::NotFound)?;
    if t.issuer_account != Some(access.account()) {
        return Err(PageError::Forbidden);
    }
    if t.contract_id.is_some() {
        return Err(PageError::Failed(
            "This request already has a contract linked to it and can no longer be released."
                .to_owned(),
        ));
    }
    storage::execute(
        "DELETE FROM reverse_trackings WHERE id = $1 AND contract_id IS NULL",
        &[t.id.into()],
    )
    .map_err(|e| failed("releasing the request", e))?;
    log::info(format!(
        "reverse request {} released by {} ({})",
        t.number, access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect(back.map_or_else(
        || {
            t.program_id
                .map_or_else(|| "reverse".to_owned(), |p| format!("reverse/{p}"))
        },
        str::to_owned,
    )))
}

/// A manager removes a pending request of a program they manage (or of a
/// deleted program, with Manage all programs).
fn remove_request(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    if !access.manager() {
        return Err(PageError::Forbidden);
    }
    let t = tracking_by("id", id_of(s.value("request"))?.into())?.ok_or(PageError::NotFound)?;
    let program = match t.program_id {
        Some(id) => get(id).map_err(|e| failed("reading the program", e))?,
        None => None,
    };
    let may = match &program {
        Some(p) => p.editable_by(access),
        None => access.manage_all(),
    };
    if !may {
        return Err(PageError::Forbidden);
    }
    if t.contract_id.is_some() {
        return Err(PageError::Failed(
            "Only requests without a linked contract can be removed.".to_owned(),
        ));
    }
    storage::execute(
        "DELETE FROM reverse_trackings WHERE id = $1 AND contract_id IS NULL",
        &[t.id.into()],
    )
    .map_err(|e| failed("removing the request", e))?;
    log::info(format!(
        "reverse request {} removed by {} ({})",
        t.number, access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("reverse/program-stats".to_owned()))
}

// ---- statistics --------------------------------------------------------------

/// A request with its contract, for the statistics' lists.
struct Contracted {
    number: String,
    program: String,
    contract_id: i64,
    status: String,
    price: f64,
    issuer_id: i64,
    date_issued: String,
}

/// Requests whose contract matches `filter` (on `c`, with `$1` a JSON
/// list of ids), not expired yet (AA's lists hide expired contracts).
fn contracted(filter: &str, ids: &[i64]) -> Result<Vec<Contracted>, PageError> {
    Ok(storage::query(
        &format!(
            "SELECT t.tracking_number, coalesce(p.name, 'Deleted program'), c.contract_id, c.status, \
                    c.price::float8, c.issuer_id, c.date_issued \
             FROM reverse_trackings t JOIN contracts c ON c.contract_id = t.contract_id \
             LEFT JOIN reverse_programs p ON p.id = t.program_id \
             WHERE {filter} IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint) \
               AND c.date_expired >= now() \
             ORDER BY c.date_issued DESC LIMIT 2000"
        ),
        &[json_ids(ids)],
    )
    .map_err(|e| failed("reading contracts", e))?
    .rows
    .iter()
    .map(|r| Contracted {
        number: text(r, 0),
        program: text(r, 1),
        contract_id: int(r, 2),
        status: text(r, 3),
        price: float(r, 4),
        issuer_id: int(r, 5),
        date_issued: when(r, 6).map(crate::rfc3339).unwrap_or_default(),
    })
    .collect())
}

/// Each contract's flag headers, as badges.
fn flag_badges(contract_ids: &[i64]) -> Result<HashMap<i64, Vec<(String, String)>>, PageError> {
    let mut out: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    if contract_ids.is_empty() {
        return Ok(out);
    }
    let rows = storage::query(
        "SELECT contract_id, tone, header FROM contract_flags \
         WHERE contract_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint) ORDER BY id",
        &[json_ids(contract_ids)],
    )
    .map_err(|e| failed("reading notes", e))?;
    for r in &rows.rows {
        out.entry(int(r, 0))
            .or_default()
            .push((text(r, 1), text(r, 2)));
    }
    Ok(out)
}

fn notes_value(flags: &HashMap<i64, Vec<(String, String)>>, contract_id: i64) -> Value {
    match flags.get(&contract_id) {
        Some(list) if !list.is_empty() => {
            let worst = ["danger", "warning", "success"]
                .into_iter()
                .find(|t| list.iter().any(|(tone, _)| tone == t))
                .unwrap_or("info");
            badge(
                list.iter()
                    .map(|(_, h)| h.as_str())
                    .collect::<Vec<_>>()
                    .join(" · "),
                tone(worst),
            )
            .into()
        }
        _ => "".into(),
    }
}

/// Pending requests' items, "Name × qty", by request.
fn items_text(tracking_ids: &[i64]) -> Result<HashMap<i64, String>, PageError> {
    let mut out = HashMap::new();
    if tracking_ids.is_empty() {
        return Ok(out);
    }
    let rows = storage::query(
        "SELECT tracking_id, type_id, quantity FROM reverse_tracking_items \
         WHERE tracking_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint) \
         ORDER BY tracking_id, quantity DESC",
        &[json_ids(tracking_ids)],
    )
    .map_err(|e| failed("reading items", e))?;
    let names = type_names(&rows.rows.iter().map(|r| int(r, 1)).collect::<Vec<_>>());
    let mut lists: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for r in &rows.rows {
        let t = int(r, 1);
        lists.entry(int(r, 0)).or_default().push(format!(
            "{} × {}",
            names.get(&t).cloned().unwrap_or_else(|| t.to_string()),
            int(r, 2)
        ));
    }
    for (id, list) in lists {
        out.insert(id, list.join(", "));
    }
    Ok(out)
}

fn contracts_table(
    rows: &[&Contracted],
    flags: &HashMap<i64, Vec<(String, String)>>,
    buyers: Option<&HashMap<i64, String>>,
    empty: &str,
) -> Table {
    let mut columns = vec![Column::text("Program"), Column::text("Tracking #")];
    if buyers.is_some() {
        columns.push(Column::text("Buyer"));
    }
    columns.extend([
        Column::text("Status"),
        Column::text("Date issued"),
        Column::numeric("Price"),
        Column::text("Notes"),
    ]);
    let mut table = Table::new(columns);
    for c in rows {
        let mut row: Vec<Value> = vec![
            c.program.clone().into(),
            link(
                c.number.clone(),
                format!("reverse/tracking/{}", encode(&c.number)),
            )
            .into(),
        ];
        if let Some(names) = buyers {
            row.push(name_or_id(names, c.issuer_id).into());
        }
        row.extend([
            status_badge(&c.status),
            time(c.date_issued.clone()),
            Value::Isk(c.price),
            notes_value(flags, c.contract_id),
        ]);
        table = table.row(row);
    }
    table.empty(empty)
}

/// AA's reverse My Statistics: the viewer's requests and their contracts.
fn my_stats(access: &Access, settings: &Settings) -> Result<Page, PageError> {
    if !access.basic() {
        return Err(PageError::Forbidden);
    }
    let list = contracted("c.issuer_id", &access.character_ids())?;
    let flags = flag_badges(&list.iter().map(|c| c.contract_id).collect::<Vec<_>>())?;
    let outstanding: Vec<&Contracted> = list.iter().filter(|c| c.status == "outstanding").collect();
    let finished: Vec<&Contracted> = list.iter().filter(|c| c.status == "finished").collect();
    let pending = storage::query(
        "SELECT t.id, t.tracking_number, coalesce(p.name, 'Deleted program'), t.created_at, \
                t.net_price::float8 \
         FROM reverse_trackings t LEFT JOIN reverse_programs p ON p.id = t.program_id \
         WHERE t.issuer_account = $1 AND t.contract_id IS NULL ORDER BY t.created_at DESC, t.id DESC",
        &[access.account().into()],
    )
    .map_err(|e| failed("reading requests", e))?;
    let items = items_text(&pending.rows.iter().map(|r| int(r, 0)).collect::<Vec<_>>())?;
    let mut pending_table = Table::new(vec![
        Column::text("Program"),
        Column::text("Tracking #"),
        Column::text("Requested"),
        Column::numeric("Price"),
        Column::text("Items"),
        Column::text(""),
    ])
    .title("Pending buy requests");
    for r in pending.rows.iter().take(MAX_ROWS) {
        let number = text(r, 1);
        pending_table = pending_table.row(vec![
            text(r, 2).into(),
            link(
                number.clone(),
                format!("reverse/tracking/{}", encode(&number)),
            )
            .into(),
            time(when(r, 3).map(crate::rfc3339).unwrap_or_default()),
            Value::Isk(float(r, 4)),
            items.get(&int(r, 0)).cloned().unwrap_or_default().into(),
            action("Release", "release")
                .field("request", int(r, 0).to_string())
                .tone(Tone::Danger)
                .confirm("The reserved items become available to others again.")
                .into(),
        ]);
    }
    let notice = if settings.purge_hours > 0 {
        format!(
            "Requests without a contract keep their items reserved for {} hours, then they're released.",
            settings.purge_hours
        )
    } else {
        "Requests without a contract keep their items reserved until you release them.".to_owned()
    };
    let total = |l: &[&Contracted]| l.iter().map(|c| c.price).sum::<f64>();
    let page = Page::new("My reverse statistics")
        .description(notice)
        .stats(vec![
            Stat::new(
                "Outstanding requests",
                Value::Number(outstanding.len() as i64),
            ),
            Stat::new("Outstanding value", Value::Isk(total(&outstanding))),
            Stat::new("Completed requests", Value::Number(finished.len() as i64)),
            Stat::new("Total spent", Value::Isk(total(&finished))),
        ])
        .table(pending_table.empty("No pending requests."))
        .tab(
            "Outstanding",
            vec![Section::Table(contracts_table(
                &outstanding,
                &flags,
                None,
                "No outstanding contracts.",
            ))],
        )
        .tab(
            "Finished",
            vec![Section::Table(contracts_table(
                &finished,
                &flags,
                None,
                "No finished contracts.",
            ))],
        );
    Ok(chips(page, access))
}

/// AA's reverse My Program Statistics: contracts to the viewer's
/// characters or corporations, and pending requests of their programs.
fn program_stats(access: &Access) -> Result<Page, PageError> {
    if !access.manager() {
        return Err(PageError::Forbidden);
    }
    let mut targets = access.character_ids();
    targets.extend(access.corporation_ids());
    let list = contracted("c.assignee_id", &targets)?;
    let flags = flag_badges(&list.iter().map(|c| c.contract_id).collect::<Vec<_>>())?;
    let buyers = names_of(&list.iter().map(|c| c.issuer_id).collect::<Vec<_>>());
    let outstanding: Vec<&Contracted> = list.iter().filter(|c| c.status == "outstanding").collect();
    let finished: Vec<&Contracted> = list.iter().filter(|c| c.status == "finished").collect();
    let programs: HashMap<i64, ReverseProgram> = all()
        .map_err(|e| failed("reading programs", e))?
        .into_iter()
        .map(|p| (p.id, p))
        .collect();
    let pending: Vec<Tracking> = storage::query(
        &format!(
            "SELECT {TRACKING_COLUMNS} FROM reverse_trackings WHERE contract_id IS NULL \
             ORDER BY created_at DESC, id DESC"
        ),
        &[],
    )
    .map_err(|e| failed("reading requests", e))?
    .rows
    .iter()
    .map(|r| tracking(r))
    .filter(|t| match t.program_id.and_then(|id| programs.get(&id)) {
        Some(p) => p.editable_by(access),
        None => access.manage_all(),
    })
    .collect();
    let requesters = names_of(
        &pending
            .iter()
            .filter_map(|t| t.issuer_character)
            .collect::<Vec<_>>(),
    );
    let items = items_text(&pending.iter().map(|t| t.id).collect::<Vec<_>>())?;
    let mut pending_table = Table::new(vec![
        Column::text("Program"),
        Column::text("Tracking #"),
        Column::text("Requested by"),
        Column::text("Requested at"),
        Column::numeric("Reserved value"),
        Column::text("Items"),
        Column::text(""),
    ]);
    for t in pending.iter().take(MAX_ROWS) {
        pending_table = pending_table.row(vec![
            t.program_id
                .and_then(|id| programs.get(&id))
                .map_or_else(|| "Deleted program".to_owned(), |p| p.name.clone())
                .into(),
            link(t.number.clone(), format!("reverse/tracking/{}", encode(&t.number))).into(),
            t.issuer_character
                .map_or_else(|| "Guest".to_owned(), |c| name_or_id(&requesters, c))
                .into(),
            time(t.created_at.clone()),
            Value::Isk(t.net_price),
            items.get(&t.id).cloned().unwrap_or_default().into(),
            action("Remove", "remove_request")
                .field("request", t.id.to_string())
                .tone(Tone::Danger)
                .confirm("This action cannot be undone. The items reserved by this request will be released back to stock.")
                .into(),
        ]);
    }
    let total = |l: &[&Contracted]| l.iter().map(|c| c.price).sum::<f64>();
    let page = Page::new("Reverse program statistics")
        .stats(vec![
            Stat::new("Outstanding", Value::Number(outstanding.len() as i64)),
            Stat::new("Outstanding value", Value::Isk(total(&outstanding))),
            Stat::new("Completed", Value::Number(finished.len() as i64)),
            Stat::new("Total sold", Value::Isk(total(&finished))),
            Stat::new("Pending requests", Value::Number(pending.len() as i64)),
        ])
        .card(Card::new("Contracts").field("", action("Refresh contracts", "refresh_contracts")))
        .tab(
            "Outstanding",
            vec![Section::Table(contracts_table(
                &outstanding,
                &flags,
                Some(&buyers),
                "No outstanding contracts.",
            ))],
        )
        .tab(
            "Finished",
            vec![Section::Table(contracts_table(
                &finished,
                &flags,
                Some(&buyers),
                "No finished contracts.",
            ))],
        )
        .tab(
            "Pending requests",
            vec![Section::Table(pending_table.empty("No pending requests."))],
        );
    Ok(chips(page, access))
}

// ---- managing reverse programs -----------------------------------------------

/// The data sources the viewer may make a program's manager.
fn owners(access: &Access) -> Vec<Character> {
    let mine = access.character_ids();
    esi::data_sources()
        .into_iter()
        .filter(|c| access.manage_all() || mine.contains(&c.id))
        .collect()
}

fn division_names(corporation: i64) -> Result<HashMap<i64, String>, PageError> {
    Ok(storage::query(
        "SELECT division, name FROM hangar_divisions WHERE corporation_id = $1",
        &[corporation.into()],
    )
    .map_err(|e| failed("reading divisions", e))?
    .rows
    .iter()
    .map(|r| (int(r, 0), text(r, 1)))
    .collect())
}

fn division_label(names: &HashMap<i64, String>, n: i64) -> String {
    const ORDINAL: [&str; 7] = ["1st", "2nd", "3rd", "4th", "5th", "6th", "7th"];
    names.get(&n).cloned().unwrap_or_else(|| {
        format!(
            "{} Division",
            ORDINAL
                .get(usize::try_from(n - 1).unwrap_or(0))
                .copied()
                .unwrap_or("1st")
        )
    })
}

fn manage_list(access: &Access, settings: &Settings) -> Result<Page, PageError> {
    let list: Vec<ReverseProgram> = all()
        .map_err(|e| failed("reading programs", e))?
        .into_iter()
        .filter(|p| p.editable_by(access))
        .collect();
    let names = names_of(&list.iter().map(|p| p.owner_character).collect::<Vec<_>>());
    let mut table = Table::new(vec![
        Column::text("Program"),
        Column::text("Manager"),
        Column::text("Stock source"),
        Column::numeric("Item types"),
        Column::text("Stock read"),
        Column::numeric("Pending requests"),
        Column::text(""),
    ]);
    for p in &list {
        let source = if p.stock_source == "containers" {
            format!(
                "{} containers",
                crate::pages::count(
                    "SELECT count(*) FROM reverse_program_containers WHERE program_id = $1",
                    p.id
                )
            )
        } else {
            p.hangar_division.map_or_else(
                || "No division".to_owned(),
                |d| division_label(&division_names(p.owner_corporation).unwrap_or_default(), d),
            )
        };
        table = table.row(vec![
            link(p.name.clone(), format!("manage/reverse/{}", p.id)).into(),
            name_or_id(&names, p.owner_character).into(),
            source.into(),
            Value::Number(crate::pages::count(
                "SELECT count(*) FROM hangar_stock WHERE program_id = $1",
                p.id,
            )),
            p.stock_synced_at
                .clone()
                .map_or_else(|| "Not yet".into(), time),
            Value::Number(crate::pages::count(
                "SELECT count(*) FROM reverse_trackings WHERE program_id = $1 AND contract_id IS NULL",
                p.id,
            )),
            action("Delete", "delete_reverse")
                .field("program", p.id.to_string())
                .tone(Tone::Danger)
                .confirm("The program, its stock and its pilots' carts go; its requests stay, no longer matched.")
                .into(),
        ]);
    }
    let mut page = Page::new("Reverse programs")
        .description("Sell corporation hangar stock to members. Stock is read from the manager's corporation every 30 minutes; its character needs the Director role.")
        .button("New reverse program", "manage/reverse/new");
    if !settings.reverse_enabled {
        page = page.card(Card::new("Reverse buyback is off").description(
            "Members don't see these programs until an admin turns reverse buyback on in Settings.",
        ));
    }
    if owners(access).is_empty() {
        page = page.card(
            Card::new("Add yourself as a manager")
                .description("A program's manager is a character of yours added as a data source.")
                .field("", add_owner("Add a manager")),
        );
    }
    Ok(page
        .table(
            table
                .title("Your reverse programs")
                .empty("No reverse programs yet: create one."),
        )
        .card(Card::new("Background updates").field(
            "",
            actions(vec![
                action("Refresh stock", "refresh_stock"),
                action("Refresh contracts", "refresh_contracts"),
            ]),
        )))
}

/// What the editor shows: a program's values, the defaults, or what was
/// posted (with what to fix).
#[derive(Debug, Clone)]
struct Draft {
    name: String,
    owner: i64,
    is_corporation: bool,
    tracking_prefill: String,
    expiration: String,
    price_type: String,
    markup: String,
    stock_source: String,
    hangar_division: String,
    containers: Vec<i64>,
    locations: Vec<i64>,
    is_public: bool,
    states: String,
    groups: Vec<i64>,
    notify_manager: bool,
    discord_channel: String,
}

fn ticked(s: &Submission, prefix: &str) -> Vec<i64> {
    s.values
        .iter()
        .filter(|(n, v)| n.starts_with(prefix) && v == "true")
        .filter_map(|(n, _)| n.trim_start_matches(prefix).parse().ok())
        .collect()
}

impl Draft {
    fn new(owner: i64) -> Self {
        Self {
            name: String::new(),
            owner,
            is_corporation: false,
            tracking_prefill: String::new(),
            expiration: "2 Weeks".to_owned(),
            price_type: "Sell".to_owned(),
            markup: "0".to_owned(),
            stock_source: "division".to_owned(),
            hangar_division: String::new(),
            containers: Vec::new(),
            locations: Vec::new(),
            is_public: false,
            states: String::new(),
            groups: Vec::new(),
            notify_manager: false,
            discord_channel: String::new(),
        }
    }

    fn of(p: &ReverseProgram) -> Result<Self, PageError> {
        let locations = program_places(p.id)
            .map_err(|e| failed("reading locations", e))?
            .iter()
            .map(|l| l.id)
            .collect();
        let containers = storage::query(
            "SELECT item_id FROM reverse_program_containers WHERE program_id = $1",
            &[p.id.into()],
        )
        .map_err(|e| failed("reading containers", e))?
        .rows
        .iter()
        .map(|r| int(r, 0))
        .collect();
        Ok(Self {
            name: p.name.clone(),
            owner: p.owner_character,
            is_corporation: p.is_corporation,
            tracking_prefill: p.tracking_prefill.clone(),
            expiration: p.expiration.clone(),
            price_type: p.price_type.clone(),
            markup: p.markup.to_string(),
            stock_source: p.stock_source.clone(),
            hangar_division: p.hangar_division.map(|d| d.to_string()).unwrap_or_default(),
            containers,
            locations,
            is_public: p.is_public,
            states: p.restricted_states.join(", "),
            groups: p.restricted_groups.clone(),
            notify_manager: p.notify_manager,
            discord_channel: p.discord_channel.clone().unwrap_or_default(),
        })
    }

    fn posted(s: &Submission) -> Self {
        Self {
            name: s.value("name").trim().to_owned(),
            owner: s.value("owner").parse().unwrap_or(0),
            is_corporation: s.checked("is_corporation"),
            tracking_prefill: s.value("tracking_prefill").trim().to_owned(),
            expiration: s.value("expiration").to_owned(),
            price_type: s.value("price_type").to_owned(),
            markup: s.value("markup").to_owned(),
            stock_source: s.value("stock_source").to_owned(),
            hangar_division: s.value("hangar_division").to_owned(),
            containers: ticked(s, "box_"),
            locations: ticked(s, "loc_"),
            is_public: s.checked("is_public"),
            states: s.value("restricted_states").to_owned(),
            groups: ticked(s, "group_"),
            notify_manager: s.checked("notify_manager"),
            discord_channel: s.value("discord_channel").to_owned(),
        }
    }
}

fn choices(list: &[&str]) -> Vec<(String, String)> {
    list.iter()
        .map(|s| ((*s).to_owned(), (*s).to_owned()))
        .collect()
}

/// The program being edited, if it's the viewer's to edit.
fn editable(access: &Access, id: Option<i64>) -> Result<Option<ReverseProgram>, PageError> {
    match id {
        Some(id) => {
            let p = get(id)
                .map_err(|e| failed("reading the program", e))?
                .ok_or(PageError::NotFound)?;
            if !p.editable_by(access) {
                return Err(PageError::NotFound);
            }
            Ok(Some(p))
        }
        None => Ok(None),
    }
}

/// The managers the editor offers: the viewer's data sources, and the
/// program's own manager for an admin editing someone else's.
fn editor_owners(access: &Access, program: Option<&ReverseProgram>) -> Vec<Character> {
    let mut list = owners(access);
    if let Some(p) = program
        && !list.iter().any(|c| c.id == p.owner_character)
    {
        list.extend(
            esi::data_sources()
                .into_iter()
                .filter(|c| c.id == p.owner_character),
        );
    }
    list
}

/// The locations the editor offers (whoever the manager): every one for
/// admins of all programs, else the viewer's and their managers', and the
/// program's own.
fn editor_locations(
    access: &Access,
    managers: &[Character],
    selected: &[i64],
) -> Result<Vec<Location>, PageError> {
    Ok(programs::locations()
        .map_err(|e| failed("reading locations", e))?
        .into_iter()
        .filter(|l| {
            access.manage_all()
                || l.created_by == access.account()
                || managers.iter().any(|c| c.id == l.owner_character)
                || selected.contains(&l.id)
        })
        .take(30)
        .collect())
}

/// A container the editor offers.
struct Box_ {
    item_id: i64,
    corporation_id: i64,
    label: String,
    structure_id: Option<i64>,
}

/// The managers' corporations' containers at the offered locations.
fn editor_containers(corporations: &[i64], locations: &[Location]) -> Result<Vec<Box_>, PageError> {
    let structures: Vec<i64> = locations.iter().filter_map(|l| l.structure_id).collect();
    if structures.is_empty() || corporations.is_empty() {
        return Ok(Vec::new());
    }
    let rows = storage::query(
        "SELECT item_id, corporation_id, name, type_id, structure_id, location_flag FROM containers \
         WHERE corporation_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint) \
           AND structure_id IN (SELECT jsonb_array_elements_text($2::jsonb)::bigint) \
         ORDER BY name, item_id LIMIT $3",
        &[
            json_ids(corporations),
            json_ids(&structures),
            (MAX_CONTAINERS as i64).into(),
        ],
    )
    .map_err(|e| failed("reading containers", e))?;
    let types = type_names(&rows.rows.iter().map(|r| int(r, 3)).collect::<Vec<_>>());
    let labels = location_labels(locations);
    let place_of = |structure: Option<i64>| -> String {
        locations
            .iter()
            .find(|l| l.structure_id.is_some() && l.structure_id == structure)
            .and_then(|l| labels.get(&l.id).cloned())
            .unwrap_or_default()
    };
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            let t = int(r, 3);
            Box_ {
                item_id: int(r, 0),
                corporation_id: int(r, 1),
                // AA's "<name> (<type name>) #<item id>", and where it is.
                label: format!(
                    "{} ({}) #{} at {}, {}",
                    text(r, 2),
                    types.get(&t).cloned().unwrap_or_else(|| t.to_string()),
                    int(r, 0),
                    place_of(opt_int(r, 4)),
                    text(r, 5),
                ),
                structure_id: opt_int(r, 4),
            }
        })
        .collect())
}

/// The reverse program editor (AA's ReverseProgramForm). Its fields don't
/// depend on the manager chosen, so a post always matches the page: every
/// manager's containers are offered, and checked against the one chosen
/// on save.
fn editor(
    access: &Access,
    id: Option<i64>,
    request: &Request,
    draft: Option<Draft>,
    problem: Option<String>,
) -> Result<Page, PageError> {
    let program = editable(access, id)?;
    let title = program.as_ref().map_or_else(
        || "New reverse program".to_owned(),
        |p| format!("Edit {}", p.name),
    );
    let managers = editor_owners(access, program.as_ref());
    if managers.is_empty() {
        return Ok(Page::new(title).card(
            Card::new("Add yourself as a manager first")
                .description("A program's manager is a character of yours added as a data source.")
                .field("", add_owner("Add a manager")),
        ));
    }
    let draft = match (draft, &program) {
        (Some(d), _) => d,
        (None, Some(p)) => Draft::of(p)?,
        (None, None) => Draft::new(
            request
                .param("owner")
                .parse()
                .ok()
                .filter(|o| managers.iter().any(|c| c.id == *o))
                .unwrap_or(managers[0].id),
        ),
    };
    let locations = editor_locations(access, &managers, &draft.locations)?;
    if locations.is_empty() {
        return Ok(Page::new(title).card(
            Card::new("Add a location first")
                .description("A reverse program sells at one or more locations: add them under Manage › Locations, with their station or structure id.")
                .field("", link("Locations", "manage/locations")),
        ));
    }
    let owner_corp = managers
        .iter()
        .find(|c| c.id == draft.owner)
        .map_or(managers[0].corporation_id, |c| c.corporation_id);
    let divisions = division_names(owner_corp)?;
    let mut corporations: Vec<i64> = managers.iter().map(|c| c.corporation_id).collect();
    corporations.sort_unstable();
    corporations.dedup();
    let boxes = editor_containers(&corporations, &locations)?;
    let labels = location_labels(&locations);
    let corp_names = if corporations.len() > 1 {
        names_of(&corporations)
    } else {
        HashMap::new()
    };

    let general = SettingsGroup::new("Program")
        .field(
            Field::text("name", "Name", 64)
                .value(draft.name.clone())
                .required(),
        )
        .field(
            Field::select(
                "owner",
                "Manager",
                managers
                    .iter()
                    .map(|c| (c.id.to_string(), c.name.clone()))
                    .collect(),
            )
            .value(draft.owner.to_string())
            .help("Stock comes from this character's corporation; contracts go to it or the corporation."),
        )
        .field(Field::checkbox(
            "is_corporation",
            "Use corporation as contract issuer",
            draft.is_corporation,
        ))
        .field(
            Field::text("tracking_prefill", "Tracking prefix", 16)
                .value(draft.tracking_prefill.clone())
                .help("Empty: the Settings' prefix."),
        )
        .field(
            Field::select("expiration", "Contract expiration", choices(EXPIRATIONS))
                .value(draft.expiration.clone()),
        )
        .field(
            Field::select("price_type", "Price type", choices(PRICE_TYPES))
                .value(draft.price_type.clone()),
        )
        .field(
            Field::number("markup", "Sell markup %")
                .value(draft.markup.clone())
                .range(Some(-100.0), Some(100.0), true)
                .help("Added to the market price; negative for a discount."),
        );
    let mut division_choices = vec![(String::new(), "None".to_owned())];
    division_choices.extend((1..=7).map(|n| (n.to_string(), division_label(&divisions, n))));
    let stock = SettingsGroup::new("Stock")
        .description(if boxes.is_empty() {
            "Stock is read from the manager's corporation hangars at the program's locations. No containers are loaded yet: Load containers reads them (the manager's character needs the Director role)."
        } else {
            "Stock is read from the manager's corporation hangars at the program's locations: a whole hangar division, or only what's directly in the containers chosen."
        })
        .field(
            Field::select(
                "stock_source",
                "Stock source",
                vec![
                    ("division".to_owned(), "Whole hangar division".to_owned()),
                    ("containers".to_owned(), "Selected containers only".to_owned()),
                ],
            )
            .value(draft.stock_source.clone()),
        )
        .field(
            Field::select("hangar_division", "Hangar division", division_choices)
                .value(draft.hangar_division.clone())
                .help("For a whole hangar division. Items in containers there don't count."),
        );
    let mut form = SettingsForm::new("reverse_program")
        .group(general)
        .group(stock);
    if !boxes.is_empty() {
        let mut group = SettingsGroup::new("Containers")
            .description("For selected containers only: what's directly in each counts.");
        for b in &boxes {
            let label = match corp_names.get(&b.corporation_id) {
                Some(corp) => format!("{} ({corp})", b.label),
                None => b.label.clone(),
            };
            group = group.field(Field::checkbox(
                format!("box_{}", b.item_id),
                label,
                draft.containers.contains(&b.item_id),
            ));
        }
        form = form.group(group);
    }
    let mut places = SettingsGroup::new("Locations")
        .description("Where the stock is and contracts are made: at least one. Only locations with a station or structure id have stock.");
    for l in &locations {
        let mut label = labels.get(&l.id).cloned().unwrap_or_else(|| l.name.clone());
        if l.structure_id.is_none() {
            label.push_str(" (no station or structure id)");
        }
        places = places.field(Field::checkbox(
            format!("loc_{}", l.id),
            label,
            draft.locations.contains(&l.id),
        ));
    }
    let mut who = SettingsGroup::new("Who may use it")
        .field(Field::checkbox(
            "is_public",
            "Every pilot who can log in",
            draft.is_public,
        ))
        .field(
            Field::text("restricted_states", "Only these states", 500)
                .value(draft.states.clone())
                .help("State names, comma-separated. Empty: any state."),
        );
    for g in identity::all_groups().iter().take(28) {
        who = who.field(Field::checkbox(
            format!("group_{}", g.id),
            format!("Group: {}", g.name),
            draft.groups.contains(&g.id),
        ));
    }
    let mut channel_choices = vec![(String::new(), "Not posted".to_owned())];
    channel_choices.extend(
        discord::channels()
            .iter()
            .map(|c| (c.id.clone(), format!("#{}", c.name))),
    );
    let notices = SettingsGroup::new("Notices")
        .field(Field::checkbox(
            "notify_manager",
            "Tell the manager of new requests",
            draft.notify_manager,
        ))
        .field(
            Field::select("discord_channel", "Post new requests to", channel_choices)
                .value(draft.discord_channel.clone())
                .help("One of the app's channels (Administration › Apps › Buyback)."),
        );
    let mut page = Page::new(title);
    if let Some(why) = problem {
        page = page.card(Card::new("Not saved").description(why));
    }
    Ok(page
        .settings(form.group(places).group(who).group(notices))
        .card(Card::new("Containers").field("", action("Load containers", "refresh_stock"))))
}

fn save(access: &Access, id: Option<i64>, s: &Submission) -> Result<SubmitResult, PageError> {
    let existing = editable(access, id)?;
    let draft = Draft::posted(s);
    let again = |why: &str| -> Result<SubmitResult, PageError> {
        Ok(SubmitResult::Page(editor(
            access,
            id,
            &s.request,
            Some(draft.clone()),
            Some(why.to_owned()),
        )?))
    };
    let managers = editor_owners(access, existing.as_ref());
    let Some(owner) = managers.iter().find(|c| c.id == draft.owner).cloned() else {
        return again("The manager must be one of your data sources.");
    };
    if draft.name.is_empty() {
        return again("Give the program a name.");
    }
    if draft.tracking_prefill.len() > 16
        || !draft
            .tracking_prefill
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return again("Only letters, numbers, dots, dashes and underscores are allowed.");
    }
    if draft.locations.is_empty() {
        return again("Pick at least one location.");
    }
    let locations = editor_locations(access, &managers, &draft.locations)?;
    let chosen: Vec<&Location> = locations
        .iter()
        .filter(|l| draft.locations.contains(&l.id))
        .collect();
    let structures: Vec<i64> = chosen.iter().filter_map(|l| l.structure_id).collect();
    let division = draft
        .hangar_division
        .parse::<i64>()
        .ok()
        .filter(|d| (1..=7).contains(d));
    let containers_mode = draft.stock_source == "containers";
    // AA's ReverseProgram.clean() and ReverseProgramForm.clean().
    if !containers_mode && division.is_none() {
        return again(
            "Stock source is a whole hangar division but no hangar division is selected.",
        );
    }
    if containers_mode {
        if structures.is_empty() {
            return again(
                "Selected containers only needs at least one location with a structure ID set; containers are matched by structure.",
            );
        }
        if draft.containers.is_empty() {
            return again(
                "Select at least one container, or change the stock source to a whole hangar division.",
            );
        }
        let mut corporations: Vec<i64> = managers.iter().map(|c| c.corporation_id).collect();
        corporations.sort_unstable();
        corporations.dedup();
        let boxes = editor_containers(&corporations, &locations)?;
        for item in &draft.containers {
            let Some(b) = boxes.iter().find(|b| b.item_id == *item) else {
                return again("A container chosen is gone: load containers again.");
            };
            if b.corporation_id != owner.corporation_id {
                return again("All containers must belong to the selected manager.");
            }
            if !b.structure_id.is_some_and(|s| structures.contains(&s)) {
                return again("Each container must be at one of the program's locations.");
            }
        }
    }
    let pick = |v: &str, list: &[&str], default: &str| -> String {
        if list.contains(&v) {
            v.to_owned()
        } else {
            default.to_owned()
        }
    };
    let states: Vec<String> = draft
        .states
        .split(',')
        .map(|x| x.trim().to_owned())
        .filter(|x| !x.is_empty())
        .collect();
    let channel = Some(draft.discord_channel.clone())
        .filter(|c| discord::channels().iter().any(|a| &a.id == c));
    let manager_account = existing
        .as_ref()
        .map_or(access.account(), |p| p.manager_account);
    let values: Vec<Db> = vec![
        draft.name.chars().take(64).collect::<String>().into(),
        draft.tracking_prefill.clone().into(),
        owner.id.into(),
        owner.corporation_id.into(),
        manager_account.into(),
        draft.is_corporation.into(),
        (if containers_mode {
            "containers"
        } else {
            "division"
        })
        .into(),
        division.into(),
        pick(&draft.expiration, EXPIRATIONS, "2 Weeks").into(),
        pick(&draft.price_type, PRICE_TYPES, "Sell").into(),
        draft
            .markup
            .parse::<i64>()
            .unwrap_or(0)
            .clamp(-100, 100)
            .into(),
        crate::json_ids(&draft.groups),
        Db::json(serde_json::to_string(&states).unwrap_or_else(|_| "[]".into())),
        draft.is_public.into(),
        draft.notify_manager.into(),
        channel.into(),
    ];
    let columns = [
        "name",
        "tracking_prefill",
        "owner_character",
        "owner_corporation",
        "manager_account",
        "is_corporation",
        "stock_source",
        "hangar_division",
        "expiration",
        "price_type",
        "markup",
        "restricted_groups",
        "restricted_states",
        "is_public",
        "notify_manager",
        "discord_channel",
    ];
    let placeholders: Vec<String> = (1..=values.len())
        .map(|i| match columns[i - 1] {
            "restricted_groups" => {
                format!("ARRAY(SELECT jsonb_array_elements_text(${i}::jsonb)::bigint)")
            }
            "restricted_states" => format!("ARRAY(SELECT jsonb_array_elements_text(${i}::jsonb))"),
            _ => format!("${i}"),
        })
        .collect();
    let program_id = match &existing {
        Some(p) => {
            let sets: Vec<String> = columns
                .iter()
                .zip(&placeholders)
                .map(|(c, p)| format!("{c} = {p}"))
                .collect();
            let mut params = values;
            params.push(p.id.into());
            storage::execute(
                &format!(
                    "UPDATE reverse_programs SET {} WHERE id = ${}",
                    sets.join(", "),
                    params.len()
                ),
                &params,
            )
            .map_err(|e| failed("saving the program", e))?;
            p.id
        }
        None => storage::query(
            &format!(
                "INSERT INTO reverse_programs ({}) VALUES ({}) RETURNING id",
                columns.join(", "),
                placeholders.join(", ")
            ),
            &values,
        )
        .map_err(|e| failed("creating the program", e))?
        .rows
        .first()
        .map_or(0, |r| int(r, 0)),
    };
    let kept_boxes: Vec<i64> = if containers_mode {
        draft.containers.clone()
    } else {
        Vec::new()
    };
    storage::transaction(&[
        Statement::new(
            "DELETE FROM reverse_program_locations WHERE program_id = $1",
            vec![program_id.into()],
        ),
        Statement::new(
            "INSERT INTO reverse_program_locations (program_id, location_id) \
             SELECT $1, l.id FROM locations l \
             WHERE l.id IN (SELECT jsonb_array_elements_text($2::jsonb)::bigint)",
            vec![
                program_id.into(),
                json_ids(&chosen.iter().map(|l| l.id).collect::<Vec<_>>()),
            ],
        ),
        Statement::new(
            "DELETE FROM reverse_program_containers WHERE program_id = $1",
            vec![program_id.into()],
        ),
        Statement::new(
            "INSERT INTO reverse_program_containers (program_id, item_id) \
             SELECT $1, x::bigint FROM jsonb_array_elements_text($2::jsonb) AS x",
            vec![program_id.into(), json_ids(&kept_boxes)],
        ),
    ])
    .map_err(|e| failed("saving the program's locations", e))?;
    // AA queues the stock read on save.
    if let Err(err) = jobs::enqueue(
        NewJob::new(HANGARS)
            .key(HANGARS)
            .payload(json!({}).to_string()),
    ) {
        log::warn(format!("the stock read wasn't queued: {err:?}"));
    }
    log::info(format!(
        "reverse program {program_id} {} by {} ({})",
        if existing.is_some() {
            "updated"
        } else {
            "created"
        },
        access.viewer.main.name,
        access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("manage/reverse".to_owned()))
}

fn delete(access: &Access, s: &Submission) -> Result<SubmitResult, PageError> {
    let p = get(id_of(s.value("program"))?)
        .map_err(|e| failed("reading the program", e))?
        .ok_or(PageError::NotFound)?;
    if !p.editable_by(access) {
        return Err(PageError::Forbidden);
    }
    storage::execute("DELETE FROM reverse_programs WHERE id = $1", &[p.id.into()])
        .map_err(|e| failed("deleting the program", e))?;
    log::info(format!(
        "reverse program {} ({}) deleted by {} ({})",
        p.name, p.id, access.viewer.main.name, access.viewer.main.id
    ));
    Ok(SubmitResult::Redirect("manage/reverse".to_owned()))
}

// ---- the stock read ----------------------------------------------------------

/// A row of `corporation-hangar-assets`' stock.
struct Held {
    structure_id: i64,
    flag: String,
    container: Option<i64>,
    type_id: i64,
    quantity: i64,
}

/// A container `corporation-hangar-assets` lists.
struct Holder {
    item_id: i64,
    type_id: i64,
    structure_id: i64,
    flag: String,
}

/// The stock of a program from a read (AA's `update_hangar_stock_esi`):
/// what's directly in its chosen containers at its structures, or loose
/// in its hangar division there (division 7 is CorpSAG7, B13), container
/// types left out; with the chosen containers skipped, and why.
fn program_stock(
    p: &ReverseProgram,
    places: &[i64],
    selected: &[i64],
    held: &[Held],
    holders: &[Holder],
    container_types: &HashSet<i64>,
) -> (BTreeMap<i64, i64>, Vec<String>) {
    let mut stock: BTreeMap<i64, i64> = BTreeMap::new();
    let mut skipped = Vec::new();
    if p.stock_source == "containers" {
        for item in selected {
            let Some(h) = holders.iter().find(|h| h.item_id == *item) else {
                skipped.push(format!("container {item} wasn't found in the hangars"));
                continue;
            };
            if !places.is_empty() && !places.contains(&h.structure_id) {
                skipped.push(format!("container {item} isn't at one of its locations"));
                continue;
            }
            for r in held.iter().filter(|r| r.container == Some(*item)) {
                *stock.entry(r.type_id).or_default() += r.quantity;
            }
        }
    } else if let Some(flag) = p.hangar_division.map(|d| format!("CorpSAG{d}")) {
        for r in held
            .iter()
            .filter(|r| r.container.is_none() && r.flag == flag && places.contains(&r.structure_id))
        {
            *stock.entry(r.type_id).or_default() += r.quantity;
        }
    }
    stock.retain(|t, q| *q > 0 && !container_types.contains(t));
    (stock, skipped)
}

/// The `hangars` schedule (every 30 minutes), Refresh stock and its
/// follow-ups: each manager's hangars read, their containers kept, and
/// each reverse program's stock replaced. A scheduled run reads managers
/// with reverse programs; Refresh stock (`{"all": true}`) also those with
/// locations, so containers can be chosen for a new program.
pub fn sync_all(job: &Job) -> Result<(), JobError> {
    let payload: Json = serde_json::from_str(&job.payload).unwrap_or(Json::Null);
    let everyone = payload["all"].as_bool().unwrap_or(false);
    let tries = payload["tries"].as_i64().unwrap_or(0);
    let list = all().map_err(|e| retry("reading programs", e))?;
    let locations = programs::locations().map_err(|e| retry("reading locations", e))?;
    let mut places: HashMap<i64, Vec<i64>> = HashMap::new();
    for p in &list {
        let ids: Vec<i64> = program_places(p.id)
            .map_err(|e| retry("reading locations", e))?
            .iter()
            .filter_map(|l| l.structure_id)
            .collect();
        places.insert(p.id, ids);
    }
    let mut waiting = false;
    let mut priced: Vec<i64> = Vec::new();
    for source in esi::data_sources() {
        let mine: Vec<&ReverseProgram> = list
            .iter()
            .filter(|p| p.owner_character == source.id)
            .collect();
        if mine.is_empty() && !everyone {
            continue;
        }
        let mut structures: Vec<i64> = mine
            .iter()
            .flat_map(|p| places.get(&p.id).cloned().unwrap_or_default())
            .collect();
        if everyone {
            structures.extend(
                locations
                    .iter()
                    .filter(|l| l.owner_character == source.id)
                    .filter_map(|l| l.structure_id),
            );
        }
        structures.sort_unstable();
        structures.dedup();
        structures.truncate(MAX_STRUCTURES);
        if structures.is_empty() {
            for p in &mine {
                replace_stock(p.id, &BTreeMap::new())?;
            }
            continue;
        }
        let answer = match esi::get(
            "corporation-hangar-assets",
            Subject::DataSource(source.id),
            &[("structure_ids".to_owned(), crate::id_list(&structures))],
            None,
        ) {
            Ok(r) => serde_json::from_str::<Json>(&r.body).unwrap_or(Json::Null),
            Err(esi::Error::Unavailable) => {
                waiting = true;
                continue;
            }
            Err(err) => {
                log::warn(format!(
                    "{} ({})'s hangars weren't read: {}",
                    source.name,
                    source.id,
                    esi::describe(&err)
                ));
                continue;
            }
        };
        let held: Vec<Held> = answer["stock"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(Held {
                    structure_id: r["structure_id"].as_i64()?,
                    flag: r["location_flag"].as_str().unwrap_or_default().to_owned(),
                    container: r["container_id"].as_i64(),
                    type_id: r["type_id"].as_i64()?,
                    quantity: r["quantity"].as_i64().unwrap_or(0),
                })
            })
            .collect();
        let listed: Vec<Holder> = answer["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(Holder {
                    item_id: r["item_id"].as_i64()?,
                    type_id: r["type_id"].as_i64()?,
                    structure_id: r["structure_id"].as_i64()?,
                    flag: r["location_flag"].as_str().unwrap_or_default().to_owned(),
                })
            })
            .collect();
        let mut ids: Vec<i64> = held.iter().map(|h| h.type_id).collect();
        ids.extend(listed.iter().map(|h| h.type_id));
        let info = crate::statics::by_ids(&ids).map_err(|e| retry("reading item data", e))?;
        // AA's containers: assembled items whose group says container.
        let holders: Vec<Holder> = listed
            .into_iter()
            .filter(|h| {
                info.get(&h.type_id)
                    .is_some_and(|t| t.group_name.to_lowercase().contains("container"))
            })
            .collect();
        let container_types: HashSet<i64> = holders.iter().map(|h| h.type_id).collect();
        keep_containers(&source, &holders, &structures, &info)?;
        for p in &mine {
            let selected: Vec<i64> = storage::query(
                "SELECT item_id FROM reverse_program_containers WHERE program_id = $1",
                &[p.id.into()],
            )
            .map_err(|e| retry("reading containers", e))?
            .rows
            .iter()
            .map(|r| int(r, 0))
            .collect();
            let (stock, skipped) = program_stock(
                p,
                places.get(&p.id).map_or(&[][..], Vec::as_slice),
                &selected,
                &held,
                &holders,
                &container_types,
            );
            for why in skipped {
                log::warn(format!("reverse program {}: {why}", p.id));
            }
            priced.extend(stock.keys());
            replace_stock(p.id, &stock)?;
        }
    }
    // Prices for the picker (a page can't fetch them).
    if !priced.is_empty()
        && let Err(err) = crate::prices::get(&priced, true)
    {
        log::warn(format!("stock prices weren't read: {err}"));
    }
    if waiting && tries < MAX_FOLLOW_UPS {
        jobs::enqueue(
            NewJob::new(HANGARS)
                .key(HANGARS_AGAIN)
                .payload(json!({ "all": everyone, "tries": tries + 1 }).to_string())
                .at(crate::rfc3339(Utc::now() + Duration::minutes(1))),
        )
        .map_err(|e| retry("queuing the stock read", e))?;
    }
    Ok(())
}

/// The corporation's containers at the structures read, named (AA's
/// `_sync_hangar_containers`); those gone from there are forgotten.
fn keep_containers(
    source: &Character,
    holders: &[Holder],
    structures: &[i64],
    info: &HashMap<i64, crate::statics::TypeInfo>,
) -> Result<(), JobError> {
    let mut names: HashMap<i64, String> = HashMap::new();
    let ids: Vec<i64> = holders.iter().map(|h| h.item_id).collect();
    for chunk in ids.chunks(1000) {
        match esi::get(
            "corporation-asset-names",
            Subject::DataSource(source.id),
            &[("item_ids".to_owned(), crate::id_list(chunk))],
            None,
        ) {
            Ok(r) => {
                let list: Json = serde_json::from_str(&r.body).unwrap_or(Json::Null);
                for n in list.as_array().into_iter().flatten() {
                    if let (Some(id), Some(name)) = (n["item_id"].as_i64(), n["name"].as_str()) {
                        names.insert(id, name.to_owned());
                    }
                }
            }
            Err(err) => log::info(format!(
                "container names weren't read: {}",
                esi::describe(&err)
            )),
        }
    }
    let rows: Vec<Json> = holders
        .iter()
        .map(|h| {
            let type_name = info
                .get(&h.type_id)
                .map_or_else(|| h.type_id.to_string(), |t| t.name.clone());
            let name = names
                .get(&h.item_id)
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty() && n != "None")
                .unwrap_or_else(|| format!("Unnamed {type_name}"));
            json!({
                "item_id": h.item_id, "name": name, "type_id": h.type_id,
                "structure_id": h.structure_id, "location_flag": h.flag,
            })
        })
        .collect();
    storage::transaction(&[
        Statement::new(
            "DELETE FROM containers WHERE corporation_id = $1 \
             AND (structure_id IS NULL OR structure_id IN (SELECT jsonb_array_elements_text($2::jsonb)::bigint)) \
             AND item_id NOT IN (SELECT jsonb_array_elements_text($3::jsonb)::bigint)",
            vec![
                source.corporation_id.into(),
                json_ids(structures),
                json_ids(&ids),
            ],
        ),
        Statement::new(
            "INSERT INTO containers (item_id, corporation_id, name, type_id, structure_id, location_flag, updated_at) \
             SELECT item_id, $1, name, type_id, structure_id, location_flag, now() \
             FROM jsonb_to_recordset($2::jsonb) \
                  AS x(item_id bigint, name text, type_id bigint, structure_id bigint, location_flag text) \
             ON CONFLICT (item_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                  name = EXCLUDED.name, type_id = EXCLUDED.type_id, \
                  structure_id = EXCLUDED.structure_id, location_flag = EXCLUDED.location_flag, \
                  updated_at = now()",
            vec![
                source.corporation_id.into(),
                Db::json(Json::Array(rows).to_string()),
            ],
        ),
    ])
    .map_err(|e| retry("storing containers", e))?;
    Ok(())
}

fn replace_stock(program_id: i64, stock: &BTreeMap<i64, i64>) -> Result<(), JobError> {
    let rows: Vec<Json> = stock
        .iter()
        .map(|(t, q)| json!({ "type_id": t, "quantity": q }))
        .collect();
    storage::transaction(&[
        Statement::new(
            "DELETE FROM hangar_stock WHERE program_id = $1",
            vec![program_id.into()],
        ),
        Statement::new(
            "INSERT INTO hangar_stock (program_id, type_id, quantity, updated_at) \
             SELECT $1, type_id, quantity, now() FROM jsonb_to_recordset($2::jsonb) \
                  AS x(type_id bigint, quantity bigint)",
            vec![program_id.into(), Db::json(Json::Array(rows).to_string())],
        ),
        Statement::new(
            "UPDATE reverse_programs SET stock_synced_at = now() WHERE id = $1",
            vec![program_id.into()],
        ),
    ])
    .map_err(|e| retry("storing the stock", e))?;
    Ok(())
}

// ---- contracts ---------------------------------------------------------------

/// Reverse matching (AA's `update_reverse_contracts_esi`): each request
/// whose contract isn't settled, against the contracts read (a title
/// containing its tracking number). A finished or rejected contract
/// isn't read again (B23); its requested items are read once.
pub fn match_contracts(fetched: &[Fetched], matched: &mut HashSet<i64>) -> Result<(), JobError> {
    let requests = storage::query(
        "SELECT t.id, t.tracking_number, t.contract_id FROM reverse_trackings t \
         LEFT JOIN contracts c ON c.contract_id = t.contract_id \
         WHERE t.program_id IS NOT NULL \
           AND (t.contract_id IS NULL OR c.status NOT IN ('finished', 'rejected')) \
           AND t.created_at > now() - interval '60 days'",
        &[],
    )
    .map_err(|e| retry("reading requests", e))?;
    for r in &requests.rows {
        let (id, number, linked) = (int(r, 0), text(r, 1), opt_int(r, 2));
        let Some(c) = fetched.iter().find(|c| c.title.contains(&number)) else {
            continue;
        };
        matched.insert(c.contract_id);
        crate::sync::upsert(c, false, true)?;
        if linked != Some(c.contract_id) {
            storage::execute(
                "UPDATE reverse_trackings SET contract_id = $1 WHERE id = $2",
                &[c.contract_id.into(), id.into()],
            )
            .map_err(|e| retry("linking a contract", e))?;
        }
        // Its items, checks and notices come once its items are read.
    }
    Ok(())
}

/// A reverse contract's checks against its request, once its requested
/// items are read (AA's `_set_reverse_contract_notifications`), then the
/// manager's notice and the program's card.
pub fn checks_and_notices(contract_id: i64) -> Result<(), JobError> {
    let rows = storage::query(
        "SELECT t.id, t.tracking_number, t.net_price::float8, t.notes, t.issuer_account, \
                c.price::float8, c.title, c.start_location_id, c.assignee_id, \
                p.id, p.is_corporation, p.owner_corporation, p.owner_character \
         FROM reverse_trackings t JOIN contracts c ON c.contract_id = t.contract_id \
         JOIN reverse_programs p ON p.id = t.program_id WHERE t.contract_id = $1",
        &[contract_id.into()],
    )
    .map_err(|e| retry("reading the request", e))?;
    let Some(r) = rows.rows.first() else {
        return Ok(());
    };
    let (tracking, number, net, notes, issuer) = (
        int(r, 0),
        text(r, 1),
        float(r, 2),
        opt_text(r, 3),
        opt_int(r, 4),
    );
    let (price, title, start, assignee) = (float(r, 5), text(r, 6), opt_int(r, 7), int(r, 8));
    let (program_id, is_corp, owner_corp, owner_char) =
        (int(r, 9), boolean(r, 10), int(r, 11), int(r, 12));
    let mut flags: Vec<(&str, &str, String)> = Vec::new();
    let (calc, paid) = (net.trunc(), price.trunc());
    if calc >= 0.0 && paid < calc {
        flags.push((
            "warning",
            "Low payment",
            "Player is paying less than the calculated price for this request.".into(),
        ));
    } else if calc >= 0.0 && paid > calc {
        flags.push((
            "success",
            "High payment",
            "Player is paying more than the calculated price for this request.".into(),
        ));
    }
    let wanted = crate::sync::merged(&crate::sync::pairs(
        "SELECT type_id, quantity FROM reverse_tracking_items WHERE tracking_id = $1",
        tracking,
    )?);
    let got = crate::sync::merged(&crate::sync::pairs(
        "SELECT type_id, quantity FROM contract_items WHERE contract_id = $1",
        contract_id,
    )?);
    if wanted != got {
        flags.push((
            "danger",
            "Item mismatch",
            "Items in the contract do not match the requested items from the tracking number."
                .into(),
        ));
    }
    let places: Vec<i64> = program_places(program_id)
        .map_err(|e| retry("reading locations", e))?
        .iter()
        .filter_map(|l| l.structure_id)
        .collect();
    if !places.is_empty() && !start.is_some_and(|s| places.contains(&s)) {
        flags.push((
            "danger",
            "Location mismatch",
            "Contract location does not match program location.".into(),
        ));
    }
    if assignee == owner_corp && !is_corp {
        flags.push((
            "warning",
            "Receiver mismatch",
            "Contract is assigned to the corporation instead of the manager character.".into(),
        ));
    } else if assignee != owner_corp && is_corp {
        flags.push((
            "warning",
            "Receiver mismatch",
            "Contract is assigned to the character instead of the corporation.".into(),
        ));
    }
    if !title.contains(&number) {
        flags.push((
            "warning",
            "Title variation",
            format!("The contract description should be: '{number}', instead it is: '{title}'"),
        ));
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        flags.push(("info", "Note from buyer", n));
    }
    if issuer.is_none() {
        flags.push((
            "info",
            "Public submission",
            "This request was submitted by an unauthenticated public user.".into(),
        ));
    }
    let statements: Vec<Statement> = flags
        .iter()
        .map(|(tone, header, message)| {
            Statement::new(
                "INSERT INTO contract_flags (contract_id, tone, header, message) VALUES ($1, $2, $3, $4)",
                vec![
                    contract_id.into(),
                    (*tone).into(),
                    (*header).into(),
                    message.clone().into(),
                ],
            )
        })
        .collect();
    let mut all_statements = vec![Statement::new(
        "DELETE FROM contract_flags WHERE contract_id = $1",
        vec![contract_id.into()],
    )];
    all_statements.extend(statements);
    storage::transaction(&all_statements).map_err(|e| retry("flagging the contract", e))?;
    crate::sync::new_contract_notice(program_id, contract_id, &number, owner_char, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(source: &str, division: Option<i64>) -> ReverseProgram {
        ReverseProgram {
            id: 1,
            name: "Stock".into(),
            tracking_prefill: String::new(),
            owner_character: 1,
            owner_corporation: 2,
            manager_account: 3,
            is_corporation: false,
            stock_source: source.into(),
            hangar_division: division,
            expiration: "2 Weeks".into(),
            price_type: "Sell".into(),
            markup: 10,
            restricted_groups: Vec::new(),
            restricted_states: Vec::new(),
            is_public: false,
            notify_manager: false,
            discord_channel: None,
            stock_synced_at: None,
        }
    }

    fn held(structure: i64, flag: &str, container: Option<i64>, t: i64, q: i64) -> Held {
        Held {
            structure_id: structure,
            flag: flag.into(),
            container,
            type_id: t,
            quantity: q,
        }
    }

    /// Division 7 is CorpSAG7 (AA read "Hangar", B13); only loose items at
    /// the program's structures count; container types are left out.
    #[test]
    fn a_division_counts_its_loose_items_at_the_programs_structures() {
        let rows = vec![
            held(10, "CorpSAG7", None, 34, 100),
            held(10, "CorpSAG7", None, 34, 50),
            held(10, "CorpSAG7", Some(99), 35, 7),
            held(10, "CorpSAG1", None, 36, 5),
            held(11, "CorpSAG7", None, 37, 5),
            held(10, "CorpSAG7", None, 17366, 1),
        ];
        let holders = vec![Holder {
            item_id: 99,
            type_id: 17366,
            structure_id: 10,
            flag: "CorpSAG7".into(),
        }];
        let types: HashSet<i64> = [17366].into_iter().collect();
        let (stock, _) = program_stock(
            &program("division", Some(7)),
            &[10],
            &[],
            &rows,
            &holders,
            &types,
        );
        assert_eq!(stock.into_iter().collect::<Vec<_>>(), vec![(34, 150)]);
    }

    /// Containers mode: what's directly in each chosen container at the
    /// program's structures; one elsewhere is skipped.
    #[test]
    fn chosen_containers_count_what_they_hold() {
        let rows = vec![
            held(10, "CorpSAG2", Some(99), 35, 7),
            held(10, "CorpSAG2", Some(98), 36, 3),
            held(11, "CorpSAG2", Some(97), 37, 4),
            held(10, "CorpSAG2", None, 38, 1),
        ];
        let holder = |item_id, structure_id| Holder {
            item_id,
            type_id: 17366,
            structure_id,
            flag: "CorpSAG2".into(),
        };
        let holders = vec![holder(99, 10), holder(98, 10), holder(97, 11)];
        let (stock, skipped) = program_stock(
            &program("containers", None),
            &[10],
            &[99, 97, 12345],
            &rows,
            &holders,
            &HashSet::new(),
        );
        assert_eq!(stock.into_iter().collect::<Vec<_>>(), vec![(35, 7)]);
        assert_eq!(skipped.len(), 2, "{skipped:?}");
    }

    #[test]
    fn prices_take_the_markup_to_the_cent() {
        let price = crate::pricing::Price {
            buy: 4.0,
            sell: 5.555,
            age_hours: 0.0,
        };
        let mut p = program("division", Some(1));
        assert_eq!(p.unit_price(Some(&price)), 6.11);
        p.price_type = "Split".into();
        p.markup = -50;
        assert_eq!(p.unit_price(Some(&price)), 2.39);
        assert_eq!(p.unit_price(None), 0.0);
    }

    #[test]
    fn whole_isk_drops_the_cents() {
        assert_eq!(whole_isk(1_234_567.5), "1 234 568");
    }
}
