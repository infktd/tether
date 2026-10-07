//! Blueprints: aa-blueprints, AA's Blueprint Library (Jay, 2026-10-04).
//!
//! - **Owners**: corporations, through a Director's character added with
//!   Add data source by `add_corporate_blueprint_owner` holders; and pilots'
//!   own characters, registered for the app and added on Owners by
//!   `add_personal_blueprint_owner` holders. Their blueprints are read
//!   every 3 hours, running jobs every hour, places every 12 (`sync`).
//! - **Library** (`basic_access`): the blueprints of owners in the
//!   viewer's corporations, or alliances with `view_alliance_blueprints`;
//!   where each is for `view_blueprint_locations`, its running job for
//!   `view_industry_jobs`.
//! - **Requests** (`request_blueprints`): a request for copies of one,
//!   with runs per copy. **Open requests** (`manage_requests`): those a
//!   builder may fulfil (the owner's corporation is one of theirs, or the
//!   owner is their own character) and those they took: In progress,
//!   Fulfilled, Re-open, Cancel. The pilot hears each step in Tether's
//!   notifications; builders get new requests on Discord (a notice to
//!   every approver, as AA sends, would reach other corporations').

mod sync;

use tether_plugin_sdk::discord::{self, Embed, Image, Mention};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::notify::{self, Level};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    CardGrid, Column, Field, Form, Page, PageError, Plugin, Request, SettingsForm, SettingsGroup,
    Stat, Submission, SubmitResult, Table, Tone, Toolbar, Value, action, actions, badge, character,
    corporation, item_type, log, time,
};

const SYNC_BLUEPRINTS: &str = "sync_blueprints";
const SYNC_JOBS: &str = "sync_jobs";
const SYNC_PLACES: &str = "sync_places";
/// Places read again a minute later, for owners a places run couldn't
/// read yet (`sync::places_again`).
const PLACES_AGAIN: &str = "places_again";
/// Rows a table lists (Tether pages them 25 at a time).
const LISTED: i64 = 500;
/// Open requests one pilot may have.
const MAX_OPEN_REQUESTS: i64 = 50;

struct Blueprints;

impl Plugin for Blueprints {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = Access::of(&viewer);
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => library(&access, request.search()),
            ["requests"] => my_requests(&access),
            ["open"] => open_requests(&access),
            ["owners"] => owners_page(&access),
            ["settings"] => settings_page(),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = Access::of(&viewer);
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        match (parts.as_slice(), submission.form.as_str()) {
            ([""], "request") => request_copy(
                &access,
                number(submission.value("item"))?,
                submission.value("runs"),
            ),
            (["requests"], "cancel_own") => cancel_own(&access, &submission),
            (["open"], "mark") => mark(&access, &submission),
            (["owners"], "add_owner" | "remove_owner") => personal_owner(&access, &submission),
            (["settings"], "settings") => save_settings(&access, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            SYNC_BLUEPRINTS => sync::blueprints(),
            SYNC_JOBS => sync::jobs(),
            SYNC_PLACES => sync::places(),
            PLACES_AGAIN => sync::places_again(&job.payload),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Blueprints);

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

fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
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

fn flag(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or(false)
}

fn number(text: &str) -> Result<i64, PageError> {
    text.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

fn ids_json(ids: &[i64]) -> Db {
    Db::json(serde_json::json!(ids).to_string())
}

/// Discord markdown out of names players choose, and no bare links.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
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

/// What the viewer may see and do (aa-blueprints' permissions).
struct Access {
    account: i64,
    name: String,
    /// The account's characters and their corporations and alliances.
    characters: Vec<i64>,
    corporations: Vec<i64>,
    alliances: Vec<i64>,
    alliance_wide: bool,
    locations: bool,
    jobs: bool,
    request: bool,
    manage_requests: bool,
    add_personal: bool,
    manage: bool,
}

impl Access {
    fn of(viewer: &Viewer) -> Self {
        let mut corporations: Vec<i64> =
            viewer.characters.iter().map(|c| c.corporation_id).collect();
        corporations.sort_unstable();
        corporations.dedup();
        let mut alliances: Vec<i64> = viewer
            .characters
            .iter()
            .filter_map(|c| c.alliance_id)
            .collect();
        alliances.sort_unstable();
        alliances.dedup();
        Self {
            account: viewer.account_id,
            name: viewer.main.name.clone(),
            characters: viewer.characters.iter().map(|c| c.id).collect(),
            corporations,
            alliances,
            alliance_wide: viewer.can("view_alliance_blueprints"),
            locations: viewer.can("view_blueprint_locations"),
            jobs: viewer.can("view_industry_jobs"),
            request: viewer.can("request_blueprints"),
            manage_requests: viewer.can("manage_requests"),
            add_personal: viewer.can("add_personal_blueprint_owner"),
            manage: viewer.can("manage"),
        }
    }

    /// SQL for "owner `o` is one whose blueprints the viewer sees", with
    /// its three parameters from `n` on: AA's user_has_access, by the
    /// owners' corporations (a personal owner's character's).
    fn sees(&self, n: usize) -> (String, Vec<Db>) {
        (
            format!(
                "(o.missing_since IS NULL \
                  AND (o.corporation_id IN (SELECT jsonb_array_elements_text(${a}::jsonb)::bigint) \
                  OR (${b} AND o.alliance_id IN (SELECT jsonb_array_elements_text(${c}::jsonb)::bigint))))",
                a = n,
                b = n + 1,
                c = n + 2
            ),
            vec![
                ids_json(&self.corporations),
                self.alliance_wide.into(),
                ids_json(&self.alliances),
            ],
        )
    }

    /// SQL for "the viewer may fulfil requests for owner `o`'s
    /// blueprints", from parameter `n`: its corporation is one of theirs,
    /// or it's their own character (AA's requests_fulfillable_by_user).
    fn builds(&self, n: usize) -> (String, Vec<Db>) {
        (
            format!(
                "(o.missing_since IS NULL \
                  AND ((o.kind = 'corporation' AND o.id IN (SELECT jsonb_array_elements_text(${a}::jsonb)::bigint)) \
                  OR (o.kind = 'character' AND o.id IN (SELECT jsonb_array_elements_text(${b}::jsonb)::bigint))))",
                a = n,
                b = n + 1
            ),
            vec![ids_json(&self.corporations), ids_json(&self.characters)],
        )
    }
}

/// A blueprint's name with its product's icon (the image server has no
/// plain icon for blueprints), or the name alone.
fn blueprint_value(product: Option<i64>, name: String) -> Value {
    match product {
        Some(id) => item_type(id, name).into(),
        None => name.into(),
    }
}

fn owner_value(kind: &str, id: i64, name: String) -> Value {
    if kind == "corporation" {
        corporation(id, name).into()
    } else {
        character(id, name).into()
    }
}

fn kind_badge(original: bool) -> Value {
    if original {
        badge("Original", Tone::Accent).into()
    } else {
        badge("Copy", Tone::Neutral).into()
    }
}

/// Where a blueprint is: the place, then the containers and hangars
/// inside it, outermost first ("Jita IV - Moon 4 › Corp Hangar 2 ›
/// Station Container").
fn place_text(place: String, within: Option<&str>, flag: &str) -> String {
    let mut parts: Vec<String> = vec![place];
    let within: Vec<serde_json::Value> = within
        .and_then(|w| serde_json::from_str(w).ok())
        .unwrap_or_default();
    for holder in within.iter().rev() {
        let flag = holder[1].as_str().unwrap_or_default();
        let hangar = hangar_name(flag);
        if let Some(hangar) = hangar {
            parts.push(hangar);
        }
        if let Some(name) = holder[2].as_str().filter(|n| !n.is_empty())
            && !flag.starts_with("Office")
        {
            parts.push(name.to_owned());
        }
    }
    if let Some(hangar) = hangar_name(flag) {
        parts.push(hangar);
    }
    parts.dedup();
    parts.join(" › ")
}

/// A place without a name: found but not named yet, looked for and not
/// found (aa-blueprints shows "Location #<id>"), or not looked for yet.
fn unnamed(known: bool, looked: bool) -> &'static str {
    if known {
        "Not named yet"
    } else if looked {
        "Unknown location"
    } else {
        "Not read yet"
    }
}

/// A hangar's name for a location flag, if it's one.
fn hangar_name(flag: &str) -> Option<String> {
    match flag {
        "Hangar" => Some("Hangar".to_owned()),
        "Deliveries" | "CorpDeliveries" => Some("Deliveries".to_owned()),
        _ => flag
            .strip_prefix("CorpSAG")
            .map(|n| format!("Corp Hangar {n}")),
    }
}

/// aa-blueprints' activities.
fn activity(id: i64) -> &'static str {
    match id {
        1 => "Manufacturing",
        2 => "Researching Technology",
        3 => "Researching Time Efficiency",
        4 => "Researching Material Efficiency",
        5 => "Copying",
        6 => "Duplicating",
        7 => "Reverse Engineering",
        8 => "Inventing",
        9 => "Reacting",
        _ => "Industry job",
    }
}

// ---- the library -----------------------------------------------------------

/// The containers a blueprint is in, with their names joined in
/// ([[type_id, flag, name], ...]).
const WITHIN: &str = "(SELECT jsonb_agg(jsonb_build_array(w ->> 0, w ->> 1, coalesce(wn.name, ''))) \
     FROM jsonb_array_elements(coalesce(b.within, '[]')) w \
     LEFT JOIN names wn ON wn.id = (w ->> 0)::bigint)::text";

/// The library: identical blueprints (type, owner, ME, TE, runs, place)
/// as one row with their count, each with a Request button.
fn library(access: &Access, q: &str) -> Result<Page, PageError> {
    let (sees, mut params) = access.sees(1);
    let filter = q.trim().to_lowercase();
    params.push(format!("%{filter}%").into());
    let rows = storage::query(
        &format!(
            "SELECT min(b.item_id), b.type_id, coalesce(n.name, 'Blueprint ' || b.type_id), \
                    p.product_type_id, o.kind, o.id, o.name, b.runs IS NULL, \
                    b.material_efficiency, b.time_efficiency, b.runs, sum(b.quantity)::bigint, \
                    pl.name, {WITHIN}, b.location_flag, count(j.job_id), \
                    (array_agg(j.activity ORDER BY j.end_date) FILTER (WHERE j.job_id IS NOT NULL))[1], \
                    min(j.end_date), b.place_id IS NOT NULL, b.place_read_at IS NOT NULL \
             FROM blueprints b JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
             LEFT JOIN names n ON n.id = b.type_id \
             LEFT JOIN products p ON p.blueprint_type_id = b.type_id \
             LEFT JOIN places pl ON pl.id = b.place_id \
             LEFT JOIN jobs j ON j.item_id = b.item_id \
             WHERE {sees} AND (lower(coalesce(n.name, '')) LIKE $4 OR lower(o.name) LIKE $4) \
             GROUP BY b.type_id, n.name, p.product_type_id, o.kind, o.id, o.name, b.runs, \
                 b.material_efficiency, b.time_efficiency, pl.name, b.within, b.location_flag, \
                 b.place_id IS NOT NULL, b.place_read_at IS NOT NULL \
             ORDER BY n.name, b.material_efficiency DESC, b.time_efficiency DESC \
             LIMIT {}",
            LISTED + 1
        ),
        &params,
    )
    .map_err(|e| failed("reading blueprints", e))?;
    let (sees, params) = access.sees(1);
    let counts = storage::query(
        &format!(
            "SELECT count(*), count(*) FILTER (WHERE b.runs IS NULL), \
                    count(*) FILTER (WHERE EXISTS (SELECT 1 FROM jobs j WHERE j.item_id = b.item_id)) \
             FROM blueprints b JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
             WHERE {sees}"
        ),
        &params,
    )
    .map_err(|e| failed("counting blueprints", e))?;
    let count = |i: usize| counts.rows.first().map_or(0, |r| int(r, i));
    let mut columns = vec![
        Column::text("Blueprint"),
        Column::text("Owner"),
        Column::text("Kind"),
        Column::numeric("ME"),
        Column::numeric("TE"),
        Column::numeric("Runs"),
        Column::numeric("Quantity"),
    ];
    if access.locations {
        columns.push(Column::text("Location"));
    }
    columns.push(Column::text("In use"));
    if access.request {
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns)
        .title("Blueprints")
        .empty(if filter.is_empty() {
            "No blueprints yet. Has a data source been added?"
        } else {
            "No blueprint matches."
        });
    let more = rows.rows.len() > usize::try_from(LISTED).unwrap_or(usize::MAX);
    for r in rows
        .rows
        .iter()
        .take(usize::try_from(LISTED).unwrap_or(usize::MAX))
    {
        let name = text(r, 2);
        let owner = text(r, 6);
        let mut cells = vec![
            blueprint_value(opt_int(r, 3), name.clone()),
            owner_value(&text(r, 4), int(r, 5), owner.clone()),
            kind_badge(flag(r, 7)),
            int(r, 8).into(),
            int(r, 9).into(),
            opt_int(r, 10).map_or_else(|| "".into(), Value::from),
            int(r, 11).into(),
        ];
        if access.locations {
            let place = opt_text(r, 12).unwrap_or_else(|| unnamed(flag(r, 18), flag(r, 19)).into());
            cells.push(place_text(place, opt_text(r, 13).as_deref(), &text(r, 14)).into());
        }
        cells.push(in_use(access, int(r, 15), opt_int(r, 16), opt_text(r, 17)));
        if access.request {
            cells.push(
                // Opens the request form below in a popup, for this one.
                action("Request", "request")
                    .field("item", int(r, 0).to_string())
                    .tone(Tone::Accent)
                    .confirm(format!("Copies of {name}, from {owner}."))
                    .into(),
            );
        }
        table = table.row(cells);
    }
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let mut page = Page::new("Blueprints").description(
            "Your corporations' and pilots' blueprints, read every 3 hours. Request copies of any of them.",
        )
    .stats(vec![
        Stat::new("Blueprints", count(0)),
        Stat::new("Originals", count(1)),
        Stat::new("In use", count(2)),
        Stat::new(
            "Last read",
            settings
                .blueprints_at
                .map_or_else(|| Value::from("not yet"), time),
        ),
    ])
    // Its own search (Tether's toolbar, in the address), by a blueprint's
    // or an owner's name, among more than it lists.
    .toolbar(Toolbar::new().search("Search blueprints and owners"));
    // Which owners failed, and why, is for those who set the app up: it
    // names owners viewers elsewhere may not see.
    if access.manage
        && let Some(error) = settings.sync_error
    {
        page = page.text(format!("The last read had a problem: {error}"));
    }
    if more {
        page = page.text(format!(
            "Showing the first {LISTED} rows: search to find the rest."
        ));
    }
    page = page.table(table);
    // The rows' Request opens this in a popup (not drawn on the page).
    if access.request {
        page = page.form(
            Form::new("request", "Request copies")
                .title("Request copies")
                .field(
                    Field::number("runs", "Runs per copy")
                        .range(Some(1.0), None, true)
                        .help("Leave empty for as many as the blueprint allows. Its builders are told on Discord, and you hear back in your notifications."),
                ),
        );
    }
    Ok(page)
}

/// Whether a row's blueprints are in use: with `view_industry_jobs`, what
/// the soonest job does and until when (as aa-blueprints' job details).
fn in_use(access: &Access, jobs: i64, activity_id: Option<i64>, ends: Option<String>) -> Value {
    if jobs == 0 {
        return "".into();
    }
    if !access.jobs {
        return badge("In use", Tone::Warning).into();
    }
    let until = ends
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| format!(" until {}", t.format("%Y-%m-%d %H:%M")))
        .unwrap_or_default();
    let more = if jobs > 1 {
        format!(" (+{} more)", jobs - 1)
    } else {
        String::new()
    };
    badge(
        format!("{}{until}{more}", activity(activity_id.unwrap_or_default())),
        Tone::Warning,
    )
    .into()
}

/// An instant from storage as a time value, or nothing.
fn when(row: &[Db], i: usize) -> Value {
    opt_text(row, i).map_or_else(|| "".into(), time)
}

// ---- requests --------------------------------------------------------------

/// Status labels as aa-blueprints shows them.
fn status_badge(status: &str) -> Value {
    match status {
        "open" => badge("Open", Tone::Accent),
        "in_progress" => badge("In progress", Tone::Warning),
        "fulfilled" => badge("Fulfilled", Tone::Success),
        _ => badge("Cancelled", Tone::Neutral),
    }
    .into()
}

const REQUEST_COLUMNS: &str = "r.id, coalesce(n.name, 'Blueprint ' || b.type_id), \
     p.product_type_id, o.kind, o.id, o.name, b.material_efficiency, b.time_efficiency, r.runs, \
     r.status, r.requester_id, r.requester_name, pl.name, b.location_flag, r.created_at, \
     r.fulfiller_name, b.place_id IS NOT NULL, b.place_read_at IS NOT NULL";

const REQUEST_JOINS: &str = "FROM requests r JOIN blueprints b ON b.item_id = r.item_id \
     JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
     LEFT JOIN names n ON n.id = b.type_id \
     LEFT JOIN products p ON p.blueprint_type_id = b.type_id \
     LEFT JOIN places pl ON pl.id = b.place_id";

fn request_cells(access: &Access, r: &[Db]) -> Vec<Value> {
    let mut cells = vec![
        blueprint_value(opt_int(r, 2), text(r, 1)),
        owner_value(&text(r, 3), int(r, 4), text(r, 5)),
        format!("{} / {}", int(r, 6), int(r, 7)).into(),
        opt_int(r, 8).map_or_else(|| Value::from("Max"), Value::from),
    ];
    if access.locations {
        let place = opt_text(r, 12).unwrap_or_else(|| unnamed(flag(r, 16), flag(r, 17)).into());
        cells.push(place_text(place, None, &text(r, 13)).into());
    }
    cells
}

fn request_columns(access: &Access, extra: &[&str]) -> Vec<Column> {
    let mut columns = vec![
        Column::text("Blueprint"),
        Column::text("Owner"),
        Column::text("ME / TE"),
        Column::numeric("Runs"),
    ];
    if access.locations {
        columns.push(Column::text("Location"));
    }
    for e in extra {
        columns.push(Column::text(*e));
    }
    columns
}

fn my_requests(access: &Access) -> Result<Page, PageError> {
    let rows = storage::query(
        &format!(
            "SELECT {REQUEST_COLUMNS} {REQUEST_JOINS} \
             WHERE r.requester_account = $1 AND r.closed_at IS NULL \
             ORDER BY r.created_at DESC LIMIT {LISTED}"
        ),
        &[access.account.into()],
    )
    .map_err(|e| failed("reading requests", e))?;
    let mut table = Table::new(request_columns(
        access,
        &["Requested", "Status", "Taken by", ""],
    ))
    .title("Your open requests")
    .empty("No open requests. Request copies of a blueprint from the library.");
    for r in &rows.rows {
        let mut cells = request_cells(access, r);
        cells.push(when(r, 14));
        cells.push(status_badge(&text(r, 9)));
        cells.push(opt_text(r, 15).unwrap_or_default().into());
        cells.push(
            action("Cancel", "cancel_own")
                .field("request", int(r, 0).to_string())
                .tone(Tone::Danger)
                .confirm("The request is closed, and the builders are told.")
                .into(),
        );
        table = table.row(cells);
    }
    Ok(Page::new("My requests")
        .description("Requests for copies you made that are still open.")
        .table(table))
}

fn open_requests(access: &Access) -> Result<Page, PageError> {
    let (builds, mut params) = access.builds(1);
    params.push(access.account.into());
    let rows = storage::query(
        &format!(
            "SELECT {REQUEST_COLUMNS} {REQUEST_JOINS} \
             WHERE r.closed_at IS NULL AND {builds} \
               AND (r.status = 'open' OR (r.status = 'in_progress' AND r.fulfiller_account = $3)) \
             ORDER BY r.created_at LIMIT {LISTED}"
        ),
        &params,
    )
    .map_err(|e| failed("reading requests", e))?;
    let mut table = Table::new(request_columns(
        access,
        &["Requested by", "Requested", "Status", ""],
    ))
    .title("Requests you may fulfil")
    .empty("No open requests for your corporations' or characters' blueprints.");
    for r in &rows.rows {
        let id = int(r, 0).to_string();
        let status = text(r, 9);
        let button = |label: &str, to: &str| {
            action(label, "mark")
                .field("request", id.clone())
                .field("to", to)
        };
        let buttons = if status == "open" {
            vec![
                button("In progress", "in_progress").tone(Tone::Accent),
                button("Cancel", "cancelled")
                    .tone(Tone::Danger)
                    .confirm("The request is closed, and its pilot is told."),
            ]
        } else {
            vec![
                button("Fulfilled", "fulfilled").tone(Tone::Accent),
                button("Re-open", "open"),
                button("Cancel", "cancelled")
                    .tone(Tone::Danger)
                    .confirm("The request is closed, and its pilot is told."),
            ]
        };
        let mut cells = request_cells(access, r);
        cells.push(character(int(r, 10), text(r, 11)).into());
        cells.push(when(r, 14));
        cells.push(status_badge(&status));
        cells.push(actions(buttons));
        table = table.row(cells);
    }
    Ok(Page::new("Open requests").description(
            "Requests for copies of your corporations' and characters' blueprints: open ones, and those you took.",
        )
    .table(table))
}

/// What a request is about, for notices: the blueprint's name and the
/// requester's account and name.
struct About {
    blueprint: String,
    product: Option<i64>,
    requester_account: i64,
    requester: String,
    runs: Option<i64>,
    owner: String,
}

fn about(request: i64) -> Result<Option<About>, PageError> {
    let rows = storage::query(
        "SELECT coalesce(n.name, 'Blueprint ' || b.type_id), p.product_type_id, \
                r.requester_account, r.requester_name, r.runs, o.name \
         FROM requests r JOIN blueprints b ON b.item_id = r.item_id \
         JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
         LEFT JOIN names n ON n.id = b.type_id \
         LEFT JOIN products p ON p.blueprint_type_id = b.type_id WHERE r.id = $1",
        &[request.into()],
    )
    .map_err(|e| failed("reading the request", e))?;
    Ok(rows.rows.first().map(|r| About {
        blueprint: text(r, 0),
        product: opt_int(r, 1),
        requester_account: int(r, 2),
        requester: text(r, 3),
        runs: opt_int(r, 4),
        owner: text(r, 5),
    }))
}

/// Notices are best effort: a request stands whether or not they went.
fn tell(account: i64, title: &str, message: &str, level: Level) {
    if let Err(err) = notify::account(account, title, message, level) {
        log::warn(format!("a notice wasn't sent: {err:?}"));
    }
}

fn request_copy(access: &Access, item: i64, runs: &str) -> Result<SubmitResult, PageError> {
    if !access.request {
        return Err(PageError::Forbidden);
    }
    // Only one the viewer may see (stricter than AA, which takes any).
    let (sees, mut params) = access.sees(1);
    params.push(item.into());
    let visible = storage::query(
        &format!(
            "SELECT 1 FROM blueprints b JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
             WHERE {sees} AND b.item_id = $4"
        ),
        &params,
    )
    .map_err(|e| failed("reading the blueprint", e))?;
    if visible.rows.is_empty() {
        return Err(PageError::NotFound);
    }
    // Runs per copy; none for as many as the blueprint allows (AA's).
    let runs = match runs.trim() {
        "" => None,
        value => Some(
            value
                .parse::<i64>()
                .ok()
                .filter(|r| *r > 0 && *r <= i64::from(i32::MAX))
                .ok_or_else(|| PageError::Failed("runs wasn't a whole number".into()))?,
        ),
    };
    let open = storage::query(
        "SELECT count(*) FROM requests WHERE requester_account = $1 AND closed_at IS NULL",
        &[access.account.into()],
    )
    .map_err(|e| failed("counting requests", e))?;
    if open.rows.first().map_or(0, |r| int(r, 0)) >= MAX_OPEN_REQUESTS {
        return Err(PageError::Failed(format!(
            "at most {MAX_OPEN_REQUESTS} open requests a pilot"
        )));
    }
    // The pilot they act as (Change character), else their main.
    let pilot = identity::acting();
    let (pilot_id, pilot_name) = pilot.map_or((0, access.name.clone()), |c| (c.id, c.name));
    // A second open request for the same blueprint is the first one.
    let created = storage::query(
        "INSERT INTO requests (item_id, requester_account, requester_id, requester_name, runs) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (item_id, requester_account) WHERE closed_at IS NULL DO NOTHING \
         RETURNING id",
        &[
            item.into(),
            access.account.into(),
            pilot_id.into(),
            pilot_name.clone().into(),
            runs.into(),
        ],
    )
    .map_err(|e| failed("saving the request", e))?;
    // Builders hear on Discord and see it on Open requests. A notice to
    // every manage_requests holder, as AA sends, would tell other
    // corporations' builders about blueprints they may not see.
    if let Some(id) = created.rows.first().map(|r| int(r, 0))
        && let Some(a) = about(id)?
    {
        post_card(&a);
    }
    // Back to the library, under its search (Tether keeps the query).
    Ok(SubmitResult::Redirect(String::new()))
}

/// A card for a new request in the channel Settings picked, if any.
fn post_card(a: &About) {
    let Ok(settings) = settings() else { return };
    let Some(channel) = settings.channel else {
        return;
    };
    let mut card = Embed::new(format!("Copy requested: {}", a.blueprint))
        .description(format!(
            "{} asks for a copy of {}.",
            escape(&a.requester),
            escape(&a.blueprint)
        ))
        .color(0x3b82f6)
        .field("Owner", escape(&a.owner))
        .field(
            "Runs per copy",
            a.runs
                .map_or_else(|| "As many as allowed".to_owned(), |r| r.to_string()),
        )
        .footer("Blueprints");
    if let Some(product) = a.product {
        card = card.thumbnail(Image::TypeIcon(product));
    }
    if let Err(err) = discord::send_embed(&channel, &card, Mention::None) {
        log::warn(format!("the request wasn't posted to Discord: {err:?}"));
    }
}

fn cancel_own(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    let id = number(submission.value("request"))?;
    storage::execute(
        "UPDATE requests SET status = 'cancelled', closed_at = now(), fulfiller_account = NULL, \
             fulfiller_name = NULL \
         WHERE id = $1 AND requester_account = $2 AND closed_at IS NULL",
        &[id.into(), access.account.into()],
    )
    .map_err(|e| failed("cancelling the request", e))?;
    Ok(SubmitResult::Redirect("requests".into()))
}

/// A builder moves a request on: In progress, Fulfilled, Re-open or
/// Cancel, as aa-blueprints' mark_request, and tells its pilot.
fn mark(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !access.manage_requests {
        return Err(PageError::Forbidden);
    }
    let id = number(submission.value("request"))?;
    let to = submission.value("to");
    // From where, to where, checked on the row being changed (so two
    // builders can't both take it): In progress from open; the rest from
    // what the builder took (or open, for Cancel).
    let from = match to {
        "in_progress" => "requests.status = 'open'",
        "fulfilled" | "open" => {
            "requests.status = 'in_progress' AND requests.fulfiller_account = $4"
        }
        "cancelled" => {
            "(requests.status = 'open' \
              OR (requests.status = 'in_progress' AND requests.fulfiller_account = $4))"
        }
        _ => return Err(PageError::NotFound),
    };
    let closes = matches!(to, "fulfilled" | "cancelled");
    let (builds, builds_params) = access.builds(6);
    let mut params: Vec<Db> = vec![
        id.into(),
        to.into(),
        closes.into(),
        access.account.into(),
        access.name.clone().into(),
    ];
    params.extend(builds_params);
    // Who took it stays with In progress and Fulfilled; Re-open and
    // Cancel let it go.
    let changed = storage::execute(
        &format!(
            "UPDATE requests SET status = $2, \
                 closed_at = CASE WHEN $3 THEN now() END, \
                 fulfiller_account = CASE WHEN $2 IN ('in_progress', 'fulfilled') THEN $4 END, \
                 fulfiller_name = CASE WHEN $2 IN ('in_progress', 'fulfilled') THEN $5 END \
             WHERE requests.id = $1 AND requests.closed_at IS NULL AND {from} \
               AND EXISTS (SELECT 1 FROM blueprints b \
                   JOIN owners o ON o.kind = b.owner_kind AND o.id = b.owner_id \
                   WHERE b.item_id = requests.item_id AND {builds})"
        ),
        &params,
    )
    .map_err(|e| failed("updating the request", e))?;
    if changed == 1
        && let Some(a) = about(id)?
    {
        let me = access.name.as_str();
        let (bp, who) = (a.blueprint.as_str(), a.requester.as_str());
        let (title, message, level) = match to {
            "in_progress" => (
                format!("{bp} request in progress"),
                format!("{me} has started producing copies for {bp}."),
                Level::Info,
            ),
            "fulfilled" => (
                format!("{bp} request completed"),
                format!("{me} has finished producing copies for {bp}."),
                Level::Success,
            ),
            "open" => (
                format!("{bp} request re-opened"),
                format!("{me} has re-opened the request for {bp} by {who}."),
                Level::Warning,
            ),
            _ => (
                format!("{bp} request canceled"),
                format!("{me} has canceled the request for {bp} by {who}."),
                Level::Danger,
            ),
        };
        tell(a.requester_account, &title, &message, level);
    }
    Ok(SubmitResult::Redirect("open".into()))
}

// ---- owners ----------------------------------------------------------------

fn owners_page(access: &Access) -> Result<Page, PageError> {
    let registered: Vec<_> = tether_plugin_sdk::esi::characters()
        .into_iter()
        .filter(|c| access.characters.contains(&c.id))
        .collect();
    let added = storage::query(
        "SELECT po.character_id, coalesce(o.name, ''), \
                (SELECT count(*) FROM blueprints b WHERE b.owner_kind = 'character' AND b.owner_id = po.character_id), \
                o.read_at, o.error \
         FROM personal_owners po LEFT JOIN owners o ON o.kind = 'character' AND o.id = po.character_id \
         WHERE po.account_id = $1 ORDER BY po.added_at",
        &[access.account.into()],
    )
    .map_err(|e| failed("reading owners", e))?;
    let added_ids: Vec<i64> = added.rows.iter().map(|r| int(r, 0)).collect();
    let mut mine = Table::new(vec![
        Column::text("Character"),
        Column::numeric("Blueprints"),
        Column::numeric("Last read"),
        Column::text("Problem"),
        Column::text(""),
    ])
    .title("In the library")
    .empty("None yet: add one of your registered characters below.");
    for r in &added.rows {
        let id = int(r, 0);
        let name = registered
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| text(r, 1), |c| c.name.clone());
        mine = mine.row(vec![
            character(id, name).into(),
            int(r, 2).into(),
            opt_text(r, 3).map_or_else(|| Value::from("not yet"), time),
            if registered.iter().any(|c| c.id == id) {
                opt_text(r, 4).unwrap_or_default().into()
            } else {
                "Not registered for the app any more: register it again".into()
            },
            action("Remove", "remove_owner")
                .field("character", id.to_string())
                .tone(Tone::Danger)
                .confirm("Its blueprints leave the library, with their requests.")
                .into(),
        ]);
    }
    let mut add = Table::new(vec![Column::text("Character"), Column::text("")])
        .title("Add a character")
        .empty("All your registered characters are in the library.");
    for c in registered.iter().filter(|c| !added_ids.contains(&c.id)) {
        add = add.row(vec![
            character(c.id, c.name.clone()).into(),
            action("Add", "add_owner")
                .field("character", c.id.to_string())
                .tone(Tone::Accent)
                .into(),
        ]);
    }
    let mut page = Page::new("My blueprints").description(
            "Your characters whose own blueprints are in the library. Register a character for this app \
             first (it reads its blueprints, industry jobs and assets). Corporations' blueprints come \
             from the app's data sources, each a Director's character, under Manage.",
        )
    .table(mine);
    if registered.is_empty() {
        page = page.cards(CardGrid::new().register());
    } else {
        page = page.table(add);
    }
    Ok(page)
}

fn personal_owner(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !access.add_personal {
        return Err(PageError::Forbidden);
    }
    let id = number(submission.value("character"))?;
    if !access.characters.contains(&id) {
        return Err(PageError::Forbidden);
    }
    if submission.form == "add_owner" {
        let registered = tether_plugin_sdk::esi::characters()
            .iter()
            .any(|c| c.id == id);
        if !registered {
            return Err(PageError::Failed(
                "that character isn't registered for the app".into(),
            ));
        }
        storage::execute(
            "INSERT INTO personal_owners (character_id, account_id) VALUES ($1, $2) \
             ON CONFLICT (character_id) DO UPDATE SET account_id = EXCLUDED.account_id",
            &[id.into(), access.account.into()],
        )
        .map_err(|e| failed("adding the owner", e))?;
        // Read its blueprints now, and then where they are.
        for (name, delay) in [(SYNC_BLUEPRINTS, 0), (SYNC_JOBS, 60), (SYNC_PLACES, 120)] {
            let at = chrono::Utc::now() + chrono::Duration::seconds(delay);
            if let Err(err) = jobs::enqueue(
                NewJob::new(name)
                    .key(format!("{name}:now"))
                    .at(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ) {
                log::warn(format!("{name} wasn't queued: {err:?}"));
            }
        }
    } else {
        storage::transaction(&[
            storage::Statement::new(
                "DELETE FROM personal_owners WHERE character_id = $1 AND account_id = $2",
                vec![id.into(), access.account.into()],
            ),
            storage::Statement::new(
                "DELETE FROM owners WHERE kind = 'character' AND id = $1 \
                 AND NOT EXISTS (SELECT 1 FROM personal_owners WHERE character_id = $1)",
                vec![id.into()],
            ),
        ])
        .map_err(|e| failed("removing the owner", e))?;
    }
    Ok(SubmitResult::Redirect("owners".into()))
}

// ---- settings --------------------------------------------------------------

struct Settings {
    channel: Option<String>,
    blueprints_at: Option<String>,
    sync_error: Option<String>,
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT channel, blueprints_at, sync_error FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.rows.first();
    Ok(Settings {
        channel: row.and_then(|r| opt_text(r, 0)),
        blueprints_at: row.and_then(|r| opt_text(r, 1)),
        sync_error: row.and_then(|r| opt_text(r, 2)),
    })
}

fn settings_page() -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let mut channels: Vec<(String, String)> = vec![(String::new(), "Not posted".to_owned())];
    channels.extend(
        discord::channels()
            .into_iter()
            .map(|c| (c.id, format!("#{}", c.name))),
    );
    let current = settings
        .channel
        .filter(|c| channels.iter().any(|(id, _)| id == c))
        .unwrap_or_default();
    Ok(Page::new("Blueprints settings").settings(
        SettingsForm::new("settings").group(
            SettingsGroup::new("Discord").field(
                Field::select("channel", "Post new requests to", channels)
                    .value(current)
                    .help("A channel an admin assigned this app (Administration › Apps › Blueprints). Builders also hear in Tether's notifications."),
            ),
        ),
    ))
}

fn save_settings(access: &Access, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !access.manage {
        return Err(PageError::Forbidden);
    }
    let assigned: Vec<String> = discord::channels().into_iter().map(|c| c.id).collect();
    let value = submission.value("channel");
    let channel = assigned.iter().find(|id| id.as_str() == value).cloned();
    storage::execute(
        "UPDATE settings SET channel = $1 WHERE id = 1",
        &[channel.clone().into()],
    )
    .map_err(|e| failed("saving settings", e))?;
    log::info(format!(
        "settings changed by {}: channel {channel:?}",
        access.name
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_reads_outermost_first() {
        let within =
            r#"[["17366", "CorpSAG2", "Station Container"], ["27", "OfficeFolder", "Office"]]"#;
        assert_eq!(
            place_text("Jita IV - Moon 4".into(), Some(within), "Unlocked"),
            "Jita IV - Moon 4 › Corp Hangar 2 › Station Container"
        );
        assert_eq!(
            place_text("Amarr VIII".into(), None, "Hangar"),
            "Amarr VIII › Hangar"
        );
    }

    #[test]
    fn a_place_without_a_name_says_why() {
        assert_eq!(unnamed(false, false), "Not read yet");
        assert_eq!(unnamed(false, true), "Unknown location");
        assert_eq!(unnamed(true, true), "Not named yet");
        assert_eq!(
            place_text(unnamed(false, true).into(), None, "CorpSAG1"),
            "Unknown location › Corp Hangar 1"
        );
    }

    #[test]
    fn names_cannot_ping_or_link() {
        assert_eq!(escape("@here [x](y)"), "\\@here \\[x\\]\\(y\\)");
        assert!(!escape("https://evil.example").contains("://"));
    }
}
