//! Fleet Operations (Alliance Auth's optimer; PRD F22).
//!
//! Fleet operations announced ahead: the operation's name, doctrine,
//! form-up system, start in EVE time, duration, FC, type and a
//! description.
//!
//! - `optimer_view` sees upcoming and past operations, with countdowns, and
//!   the next five on the Dashboard (AA's "Upcoming Fleets").
//! - `optimer_management` creates, edits and deletes them (AA's
//!   permissions and behaviour; only the pages differ).
//! - Types are AA's `OpTimerType`: free text, found by name whatever its
//!   case or made the first time it's used, and offered again after.
//! - An operation keeps the character who last saved it, as AA's
//!   `eve_character`, and when it was first posted (`post_time`).

mod when;

use chrono::{DateTime, Utc};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission,
    SubmitResult, Table, Tone, Value, action, badge, character, countdown, link, log, time,
};

/// AA's field lengths.
const MAX_TEXT: u32 = 254;
const MAX_DURATION: u32 = 25;
/// AA's description is unlimited; this keeps it within one piece of page
/// text (2 KiB), whatever the characters.
const MAX_DESCRIPTION: u32 = 500;
/// The host's limit on a select's options: past it, types are still
/// typed by name (AA's text box offers every one).
const TYPE_OPTIONS: i64 = 100;
/// Rows per list, within the host's page limits.
const UPCOMING_ROWS: i64 = 500;
const PAST_ROWS: i64 = 200;
/// AA's Dashboard widget shows the next five.
const WIDGET_ROWS: i64 = 5;

const MANAGE: &str = "optimer_management";

struct FleetOperations;

impl Plugin for FleetOperations {
    fn render(request: Request) -> Result<Page, PageError> {
        // plugin.toml's page rules let only optimer_management open add
        // and op/*, and optimer_view the rest, as AA's views.
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if let Some(id) = path.strip_prefix("op/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return edit_page(id, None, None);
        }
        match path {
            "" => ops_page(&viewer),
            "widget" => widget_page(),
            "add" => add_page(None, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        if !viewer.can(MANAGE) {
            return Err(PageError::Forbidden);
        }
        let path = submission.request.path.as_str();
        if let Some(id) = path.strip_prefix("op/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return match submission.form.as_str() {
                "op" => save_op(&viewer, Some(id), &submission),
                "delete" => delete_op(&viewer, id),
                _ => Err(PageError::NotFound),
            };
        }
        match (path, submission.form.as_str()) {
            ("add", "op") => save_op(&viewer, None, &submission),
            // A row's Delete on the list.
            ("", "delete") => {
                let id: i64 = submission
                    .value("op")
                    .parse()
                    .map_err(|_| PageError::NotFound)?;
                delete_op(&viewer, id)
            }
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(FleetOperations);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

// ---- operations ------------------------------------------------------------

struct Op {
    id: i64,
    name: String,
    doctrine: String,
    system: String,
    start: DateTime<Utc>,
    duration: String,
    fc: String,
    description: String,
    /// "" when it has none.
    op_type: String,
    type_id: Option<i64>,
    /// AA's `eve_character`: whoever last saved it.
    creator: String,
    creator_id: i64,
    /// AA's `post_time`: when it was first posted.
    posted: Option<DateTime<Utc>>,
}

const OP_COLUMNS: &str = "o.id, o.operation_name, o.doctrine, o.system, o.start_time, o.duration, \
                          o.fc, o.description, coalesce(t.name, ''), o.type_id, o.character_name, \
                          o.character_id, o.post_time";

const OP_FROM: &str = "ops o LEFT JOIN op_types t ON t.id = o.type_id";

fn op(row: &[Db]) -> Option<Op> {
    Some(Op {
        id: int(row, 0),
        name: text(row, 1),
        doctrine: text(row, 2),
        system: text(row, 3),
        start: when(row, 4)?,
        duration: text(row, 5),
        fc: text(row, 6),
        description: text(row, 7),
        op_type: text(row, 8),
        type_id: row.get(9).and_then(Db::as_integer),
        creator: text(row, 10),
        creator_id: int(row, 11),
        posted: when(row, 12),
    })
}

fn ops(filter: &str, order: &str, limit: i64) -> Result<Vec<Op>, PageError> {
    Ok(query(
        &format!("SELECT {OP_COLUMNS} FROM {OP_FROM} WHERE {filter} ORDER BY {order} LIMIT $1"),
        &[limit.into()],
    )?
    .iter()
    .filter_map(|r| op(r))
    .collect())
}

fn upcoming(limit: i64) -> Result<Vec<Op>, PageError> {
    // AA's "Next Fleet Operations": those starting now or later.
    ops("o.start_time >= now()", "o.start_time, o.id", limit)
}

fn past(limit: i64) -> Result<Vec<Op>, PageError> {
    ops("o.start_time < now()", "o.start_time DESC, o.id", limit)
}

/// Upcoming operations tick in the browser; past ones say how long ago.
fn starts_in(now: DateTime<Utc>, at: DateTime<Utc>) -> Value {
    if at > now {
        countdown(rfc3339(at))
    } else {
        when::countdown(now, at).into()
    }
}

fn type_value(op: &Op) -> Value {
    if op.op_type.is_empty() {
        "".into()
    } else {
        badge(op.op_type.clone(), Tone::Neutral).into()
    }
}

/// An operation's Delete, asking first. It posts `delete` with the
/// operation's id, from the list or the operation's own page.
fn delete_button(op: &Op) -> Value {
    action("Delete", "delete")
        .field("op", op.id.to_string())
        .tone(Tone::Danger)
        .confirm(format!(
            "The fleet operation {} is deleted for everyone.",
            op.name
        ))
        .into()
}

fn ops_table(list: &[Op], now: DateTime<Utc>, manage: bool, upcoming: bool, empty: &str) -> Table {
    let mut columns = vec![
        Column::text("Operation"),
        Column::text("Type"),
        Column::text("Doctrine"),
        Column::text("Form-up system"),
        Column::numeric("Start (EVE time)"),
        Column::numeric(if upcoming { "Starts in" } else { "Started" }),
        Column::text("Duration"),
        Column::text("FC"),
        Column::text("Description"),
        Column::text("Creator"),
    ];
    if manage {
        columns.push(Column::text("Action"));
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns).empty(empty);
    for op in list {
        let creator: Value = if op.creator_id > 0 {
            character(op.creator_id, op.creator.clone()).into()
        } else {
            op.creator.clone().into()
        };
        let mut row = vec![
            op.name.clone().into(),
            type_value(op),
            op.doctrine.clone().into(),
            op.system.clone().into(),
            time(rfc3339(op.start)),
            starts_in(now, op.start),
            op.duration.clone().into(),
            op.fc.clone().into(),
            op.description.clone().into(),
            creator,
        ];
        if manage {
            row.push(link("Edit", format!("op/{}", op.id)).into());
            row.push(delete_button(op));
        }
        table = table.row(row);
    }
    table
}

/// AA's "Create Operation", for managers.
fn header(page: Page, manage: bool) -> Page {
    if manage {
        page.button("Create Operation", "add")
    } else {
        page
    }
}

/// Both tabs' rows together, well under the host's 1 MiB a page.
const LIST_BYTES: usize = 600 * 1024;

/// What one row of the list costs the page: every text it draws (the name
/// twice, in Delete's question), 16 bytes for each of its 12 values, and
/// room for its times and links. Fields are capped in characters, and the
/// host counts bytes.
fn row_bytes(op: &Op) -> usize {
    op.name.len() * 2
        + op.doctrine.len()
        + op.system.len()
        + op.duration.len()
        + op.fc.len()
        + op.description.len()
        + op.op_type.len()
        + op.creator.len()
        + 12 * 16
        + 256
}

/// Keeps as much of `list` as fits in `budget`, taking it from the budget.
/// Whether anything was left out.
fn fit(list: &mut Vec<Op>, budget: &mut usize) -> bool {
    let mut kept = 0;
    for op in list.iter() {
        let bytes = row_bytes(op);
        if bytes > *budget {
            break;
        }
        *budget -= bytes;
        kept += 1;
    }
    let cut = kept < list.len();
    list.truncate(kept);
    cut
}

fn ops_page(viewer: &Viewer) -> Result<Page, PageError> {
    let now = Utc::now();
    let manage = viewer.can(MANAGE);
    let mut upcoming = upcoming(UPCOMING_ROWS)?;
    let mut past = past(PAST_ROWS)?;
    let next = upcoming.first().map_or_else(
        || Value::from("None"),
        |op| badge(when::countdown(now, op.start), Tone::Accent).into(),
    );
    let mut next_stat = Stat::new("Next operation", next);
    if let Some(op) = upcoming.first() {
        next_stat = next_stat.caption(format!("{}, from {}", op.name, op.system));
    }
    let upcoming_count = count(upcoming.len());
    // However long the operations' texts, the page stays within the
    // host's limits: the lists are cut short rather than the page refused.
    // Upcoming first, leaving the past at least a third.
    let mut budget = LIST_BYTES * 2 / 3;
    let upcoming_title = if fit(&mut upcoming, &mut budget) {
        format!(
            "Next Fleet Operations: the first {}, all that fit",
            upcoming.len()
        )
    } else {
        "Next Fleet Operations".to_owned()
    };
    budget += LIST_BYTES / 3;
    let past_title = if fit(&mut past, &mut budget) {
        format!(
            "Past Fleet Operations: the latest {}, all that fit",
            past.len()
        )
    } else {
        format!("Past Fleet Operations: the latest {PAST_ROWS}, newest first")
    };
    let page = Page::new("Fleet Operations")
        .description("Fleet operations in EVE time, with their doctrine, form-up system and FC.")
        .stats(vec![next_stat, Stat::new("Upcoming", upcoming_count)]);
    Ok(header(page, manage)
        .tab(
            "Upcoming",
            vec![Section::Table(
                ops_table(&upcoming, now, manage, true, "No upcoming operations.")
                    .title(upcoming_title),
            )],
        )
        .tab(
            "Past",
            vec![Section::Table(
                ops_table(&past, now, manage, false, "No past operations.").title(past_title),
            )],
        ))
}

/// AA's Dashboard widget: the next five operations.
fn widget_page() -> Result<Page, PageError> {
    let now = Utc::now();
    let mut table = Table::new(vec![
        Column::text("Operation"),
        Column::text("Type"),
        Column::text("Form-up system"),
        Column::numeric("EVE time"),
        Column::numeric("Starts in"),
    ])
    .empty("No upcoming fleets.");
    for op in upcoming(WIDGET_ROWS)? {
        table = table.row(vec![
            op.name.clone().into(),
            type_value(&op),
            op.system.clone().into(),
            time(rfc3339(op.start)),
            starts_in(now, op.start),
        ]);
    }
    // On the Dashboard only the table shows; opened on its own, the link
    // leads to the full list.
    Ok(Page::new("Upcoming Fleets")
        .description("The next five fleet operations")
        .link("All Operations", "")
        .table(table))
}

// ---- adding and editing ------------------------------------------------------

/// Operation types, by name, for the form's select.
fn types() -> Result<Vec<(i64, String)>, PageError> {
    Ok(query(
        "SELECT id, name FROM op_types ORDER BY lower(name), id LIMIT $1",
        &[TYPE_OPTIONS.into()],
    )?
    .iter()
    .map(|r| (int(r, 0), text(r, 1)))
    .collect())
}

const TEXT_FIELDS: [&str; 9] = [
    "operation_name",
    "doctrine",
    "system",
    "start",
    "duration",
    "fc",
    "type",
    "new_type",
    "description",
];

/// What the operation form shows: a new operation's (empty), a stored
/// one, or what was just posted (to fix a mistake).
struct Values(Vec<(&'static str, String)>);

impl Values {
    fn new_op() -> Self {
        Self(Vec::new())
    }

    fn stored(op: &Op) -> Self {
        Self(vec![
            ("operation_name", op.name.clone()),
            ("doctrine", op.doctrine.clone()),
            ("system", op.system.clone()),
            ("start", when::eve_time_text(op.start)),
            ("duration", op.duration.clone()),
            ("fc", op.fc.clone()),
            (
                "type",
                op.type_id.map(|id| id.to_string()).unwrap_or_default(),
            ),
            ("description", op.description.clone()),
            // Written into "new type" if the select can't offer it.
            ("type_name", op.op_type.clone()),
        ])
    }

    fn posted(submission: &Submission) -> Self {
        Self(
            TEXT_FIELDS
                .iter()
                .map(|name| (*name, submission.value(name).to_owned()))
                .collect(),
        )
    }

    fn get(&self, name: &str) -> String {
        self.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

/// AA's form, in the order the list shows it. The type is chosen from
/// those used before, or a new one is typed (AA's text box with
/// suggestions).
fn op_form(values: &Values, submit: &str) -> Result<Form, PageError> {
    let v = |name: &str| values.get(name);
    let known = types()?;
    let mut form = Form::new("op", submit)
        .description("Times are EVE time.")
        .field(
            Field::text("operation_name", "Operation name", MAX_TEXT)
                .required()
                .value(v("operation_name")),
        )
        .field(
            Field::text("doctrine", "Doctrine", MAX_TEXT)
                .required()
                .value(v("doctrine")),
        )
        .field(
            Field::text("system", "Form-up system", MAX_TEXT)
                .required()
                .value(v("system")),
        )
        .field(
            Field::text("start", "Start (EVE time)", 20)
                .required()
                .help("As YYYY-MM-DD HH:MM, or YYYY.MM.DD HH:MM as the game shows it.")
                .value(v("start")),
        )
        .field(
            Field::text("duration", "Duration", MAX_DURATION)
                .required()
                .help("Such as 2h, or 90 minutes.")
                .value(v("duration")),
        )
        .field(
            Field::text("fc", "Fleet commander", MAX_TEXT)
                .required()
                .value(v("fc")),
        );
    if !known.is_empty() {
        let mut select = Field::select(
            "type",
            "Operation type",
            known
                .iter()
                .map(|(id, name)| (id.to_string(), name.clone()))
                .collect(),
        );
        // Only a type that's still there (the host refuses a select
        // starting on anything else); otherwise none.
        let chosen = v("type");
        if known.iter().any(|(id, _)| id.to_string() == chosen) {
            select = select.value(chosen);
        }
        form = form.field(select);
    }
    // A stored type the select doesn't hold (past its 100) is written in
    // by name, so saving keeps it.
    let offered = known.iter().any(|(id, _)| id.to_string() == v("type"));
    let new_type = if v("new_type").is_empty() && !offered {
        v("type_name")
    } else {
        v("new_type")
    };
    let new_label = if known.is_empty() {
        "Operation type"
    } else {
        "Or a new type"
    };
    Ok(form
        .field(
            Field::text("new_type", new_label, MAX_TEXT)
                .help("Such as CTA, Stratop or Roam: offered above after, for everyone.")
                .value(new_type),
        )
        .field(
            Field::textarea("description", "Additional info", MAX_DESCRIPTION)
                .help("Optional: the operation in a few words.")
                .value(v("description")),
        ))
}

fn add_page(note: Option<&str>, values: Option<Values>) -> Result<Page, PageError> {
    let mut page = header(
        Page::new("Create Operation").description("A new fleet operation"),
        true,
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    let values = values.unwrap_or_else(Values::new_op);
    Ok(page.form(op_form(&values, "Create Operation")?))
}

fn stored_op(id: i64) -> Result<Op, PageError> {
    query(
        &format!("SELECT {OP_COLUMNS} FROM {OP_FROM} WHERE o.id = $1"),
        &[id.into()],
    )?
    .first()
    .and_then(|r| op(r))
    .ok_or(PageError::NotFound)
}

fn edit_page(id: i64, note: Option<&str>, values: Option<Values>) -> Result<Page, PageError> {
    let op = stored_op(id)?;
    let now = Utc::now();
    let mut about = Card::new("Operation")
        .field("Start (EVE time)", time(rfc3339(op.start)))
        .field("Starts in", starts_in(now, op.start))
        .field("Creator", character(op.creator_id, op.creator.clone()));
    if let Some(posted) = op.posted {
        about = about.field("Posted", time(rfc3339(posted)));
    }
    about = about.field("Delete", delete_button(&op));
    let mut page = header(
        Page::new("Edit Operation").description(format!("{}, from {}", op.name, op.system)),
        true,
    )
    .card(about);
    if let Some(note) = note {
        page = page.text(note);
    }
    let values = values.unwrap_or_else(|| Values::stored(&op));
    Ok(page.form(op_form(&values, "Save Operation")?))
}

/// The type an operation gets: a new one typed (found by name whatever its
/// case, or made), else the one chosen, else none.
fn resolve_type(submission: &Submission) -> Result<Result<Option<i64>, &'static str>, PageError> {
    let typed = submission.value("new_type").trim();
    if !typed.is_empty() {
        let found = query(
            "SELECT id FROM op_types WHERE lower(name) = lower($1)",
            &[typed.to_owned().into()],
        )?;
        if let Some(row) = found.first() {
            return Ok(Ok(Some(int(row, 0))));
        }
        return Ok(Ok(Some(make_type(typed)?)));
    }
    let chosen = submission.value("type");
    if chosen.is_empty() {
        return Ok(Ok(None));
    }
    // Chosen from the select the form drew, so it exists.
    let id: i64 = chosen.parse().map_err(|_| PageError::NotFound)?;
    let exists = !query("SELECT id FROM op_types WHERE id = $1", &[id.into()])?.is_empty();
    Ok(if exists {
        Ok(Some(id))
    } else {
        Err("That operation type is gone: choose another.")
    })
}

/// Makes a type, or finds one of that name made at the same moment.
fn make_type(name: &str) -> Result<i64, PageError> {
    let made = storage::query(
        "INSERT INTO op_types (name) VALUES ($1) \
         ON CONFLICT ((lower(name))) DO UPDATE SET name = op_types.name RETURNING id",
        &[name.to_owned().into()],
    )
    .map_err(|e| failed("adding the type", e))?;
    made.rows
        .first()
        .map(|r| int(r, 0))
        .ok_or_else(|| PageError::Failed("adding the type returned nothing".to_owned()))
}

fn save_op(
    viewer: &Viewer,
    id: Option<i64>,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let again = |note: &str| -> Result<SubmitResult, PageError> {
        let values = Some(Values::posted(submission));
        Ok(SubmitResult::Page(match id {
            Some(id) => edit_page(id, Some(note), values)?,
            None => add_page(Some(note), values)?,
        }))
    };
    if let Some(id) = id {
        stored_op(id)?;
    }
    let start = match when::start_time(submission.value("start")) {
        Ok(at) => at,
        Err(why) => return again(why),
    };
    let required = ["operation_name", "doctrine", "system", "duration", "fc"];
    if required
        .iter()
        .any(|name| submission.value(name).trim().is_empty())
    {
        return again("Give the operation's name, doctrine, form-up system, duration and FC.");
    }
    let type_id = match resolve_type(submission)? {
        Ok(type_id) => type_id,
        Err(why) => return again(why),
    };
    let trimmed = |name: &str| Db::from(submission.value(name).trim().to_owned());
    // AA's eve_character: whoever saves it, creating or editing.
    let mut params: Vec<Db> = vec![
        trimmed("operation_name"),
        trimmed("doctrine"),
        trimmed("system"),
        Db::timestamp(rfc3339(start)),
        trimmed("duration"),
        trimmed("fc"),
        trimmed("description"),
        type_id.map_or(Db::Null, Db::from),
        viewer.account_id.into(),
        viewer.main.id.into(),
        viewer.main.name.clone().into(),
    ];
    match id {
        None => {
            let added = storage::query(
                "INSERT INTO ops (operation_name, doctrine, system, start_time, duration, fc, \
                 description, type_id, account_id, character_id, character_name) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
                &params,
            )
            .map_err(|e| failed("adding the operation", e))?;
            let new = added.rows.first().map_or(0, |r| int(r, 0));
            log::info(format!(
                "operation {new} created by {} ({})",
                viewer.main.name, viewer.main.id
            ));
        }
        Some(id) => {
            params.push(id.into());
            let changed = storage::execute(
                "UPDATE ops SET operation_name = $1, doctrine = $2, system = $3, start_time = $4, \
                 duration = $5, fc = $6, description = $7, type_id = $8, account_id = $9, \
                 character_id = $10, character_name = $11 WHERE id = $12",
                &params,
            )
            .map_err(|e| failed("saving the operation", e))?;
            if changed == 0 {
                return Err(PageError::NotFound);
            }
            log::info(format!(
                "operation {id} edited by {} ({})",
                viewer.main.name, viewer.main.id
            ));
        }
    }
    Ok(SubmitResult::Redirect(String::new()))
}

fn delete_op(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    let deleted = storage::execute("DELETE FROM ops WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the operation", e))?;
    if deleted == 0 {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "operation {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}
