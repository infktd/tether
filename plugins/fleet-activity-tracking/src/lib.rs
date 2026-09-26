//! Fleet Activity Tracking (Alliance Auth's name for it, with aa-afat's
//! additions; PRD F23).
//!
//! - **FAT links**: an FC creates one for a fleet, with a fleet type, a
//!   doctrine and an expiry, and shares its link. Members open it and
//!   register their characters' attendance (a FAT) while it's open;
//!   multiboxers tick every character they brought.
//! - FCs close their links early or reopen them once; managers edit any
//!   link, add and remove FATs by hand, delete links and keep the fleet
//!   types.
//! - **Statistics** per pilot, corporation and alliance, by month, behind
//!   aa-afat's permissions.
//! - **Logs** of what FCs and managers did, kept for 60 days.
//!
//! aa-afat's ESI-tracked fleets need ESI's fleet endpoints, which the host
//! doesn't offer plugins yet: FAT links are clickable ones.

use chrono::{DateTime, Datelike, Duration, SecondsFormat, Utc};
use tether_plugin_sdk::esi;
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Section, Stat, Submission,
    SubmitResult, Table, Tone, Value, badge, link, log, time,
};

/// A new link's expiry unless the FC picks another (aa-afat's default).
const DEFAULT_EXPIRY_MINUTES: i64 = 60;
/// The longest a link may stay open at once.
const MAX_EXPIRY_MINUTES: i64 = 24 * 60;
/// How often an FC may reopen their own expired link; managers may always.
const REOPEN_LIMIT: i64 = 1;
/// Logs are kept this long (aa-afat's default).
const LOG_DAYS: i64 = 60;
/// FAT links per page of the FAT Links list.
const LINKS_PER_PAGE: i64 = 100;
/// Characters offered on the register form (a form has at most 30 fields).
const MAX_FORM_CHARACTERS: usize = 30;
/// Attendees shown on a link's page, and offered for removal in a list.
const MAX_ATTENDEES: i64 = 500;
const MAX_SELECT: usize = 100;
/// Rows in statistics tables, within the host's page limits.
const MAX_STAT_ROWS: usize = 300;
const MAX_FLEET: u32 = 100;
const MAX_DOCTRINE: u32 = 100;
const MAX_TYPE_NAME: u32 = 50;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct FleetActivityTracking;

impl Plugin for FleetActivityTracking {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => dashboard(&viewer),
            ["links"] => links_page(&viewer, 1),
            ["links", "page", n] => links_page(&viewer, number(n)?),
            ["links", "create"] => create_page(&viewer, None),
            ["links", hash] => details_page(&viewer, hash, None),
            ["links", hash, "add"] => register_page(&viewer, hash, None),
            ["stats"] => stats_page(&viewer, this_year()),
            ["stats", year] => stats_page(&viewer, year_of(year)?),
            ["stats", "corporation", id] => corporation_page(&viewer, number(id)?, this_year()),
            ["stats", "corporation", id, year] => {
                corporation_page(&viewer, number(id)?, year_of(year)?)
            }
            ["stats", "alliance", id] => alliance_page(&viewer, number(id)?, this_year()),
            ["stats", "alliance", id, year] => alliance_page(&viewer, number(id)?, year_of(year)?),
            ["stats", "character", id] => character_page(&viewer, number(id)?, this_year()),
            ["stats", "character", id, year] => {
                character_page(&viewer, number(id)?, year_of(year)?)
            }
            ["fleet-types"] => fleet_types_page(None),
            ["logs"] => logs_page(&viewer),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        remember_characters(&viewer);
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        match (parts.as_slice(), submission.form.as_str()) {
            (["links", "create"], "create") => create_link(&viewer, &submission),
            (["links", hash, "add"], "register") => register(&viewer, hash, &submission),
            (["links", hash], form) => change_link(&viewer, hash, form, &submission),
            (["fleet-types"], form) => change_fleet_types(&viewer, form, &submission),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "housekeeping" => housekeeping(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(FleetActivityTracking);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
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

fn maybe_text(row: &[Db], i: usize) -> Option<String> {
    row.get(i).and_then(Db::as_text).map(str::to_owned)
}

fn flag(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or_default()
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

fn with_rows(mut table: Table, rows: impl IntoIterator<Item = Vec<Value>>) -> Table {
    for row in rows {
        table = table.row(row);
    }
    table
}

/// A positive id or page number from a path segment.
fn number(segment: &str) -> Result<i64, PageError> {
    match segment.parse::<i64>() {
        Ok(n) if n > 0 && segment.len() <= 19 => Ok(n),
        _ => Err(PageError::NotFound),
    }
}

fn this_year() -> i32 {
    Utc::now().year()
}

/// A year from a path segment: EVE's, up to next year.
fn year_of(segment: &str) -> Result<i32, PageError> {
    match segment.parse::<i32>() {
        Ok(y) if (2003..=this_year() + 1).contains(&y) && segment.len() == 4 => Ok(y),
        _ => Err(PageError::NotFound),
    }
}

/// The year's bounds, as timestamp parameters.
fn year_bounds(year: i32) -> (Db, Db) {
    (
        Db::timestamp(format!("{year:04}-01-01T00:00:00Z")),
        Db::timestamp(format!("{:04}-01-01T00:00:00Z", year + 1)),
    )
}

/// A FAT link's hash: 32 lowercase hex characters (from a UUID v4).
fn valid_hash(hash: &str) -> bool {
    hash.len() == 32
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Ids as a parameter for `= ANY(string_to_array($n, ',')::bigint[])`.
fn id_list(ids: impl IntoIterator<Item = i64>) -> Db {
    let ids: Vec<String> = ids.into_iter().map(|id| id.to_string()).collect();
    ids.join(",").into()
}

fn can_create(viewer: &Viewer) -> bool {
    viewer.can("add_fatlink") || viewer.can("manage_afat")
}

fn can_edit(viewer: &Viewer, link: &LinkInfo) -> bool {
    viewer.can("manage_afat")
        || (viewer.can("add_fatlink") && link.creator_account == viewer.account_id)
}

/// A log entry of what the viewer did (aa-afat's events).
fn log_entry(viewer: &Viewer, event: &str, hash: Option<&str>, description: String) -> Statement {
    Statement::new(
        "INSERT INTO logs (event, actor_id, actor_name, link_hash, description) VALUES ($1, $2, $3, $4, $5)",
        vec![
            event.into(),
            viewer.main.id.into(),
            viewer.main.name.clone().into(),
            hash.map(str::to_owned).into(),
            description.into(),
        ],
    )
}

fn run(statements: &[Statement], what: &str) -> Result<(), PageError> {
    storage::transaction(statements)
        .map(|_| ())
        .map_err(|e| failed(what, e))
}

/// Keeps every character of the viewer's account, so managers can add
/// FATs by name. Best effort: a failure is only logged.
fn remember_characters(viewer: &Viewer) {
    let rows: Vec<serde_json::Value> = viewer
        .characters
        .iter()
        // Only characters whose corporation Tether knows.
        .filter(|c| c.corporation_id > 0)
        .map(|c| {
            serde_json::json!({
                "character_id": c.id,
                "name": c.name,
                "corporation_id": c.corporation_id,
                "alliance_id": c.alliance_id,
            })
        })
        .collect();
    if let Err(err) = storage::execute(
        "INSERT INTO characters (character_id, name, corporation_id, alliance_id, seen_at) \
         SELECT character_id, name, corporation_id, alliance_id, now() \
         FROM json_to_recordset($1::json) AS x(character_id bigint, name text, corporation_id bigint, alliance_id bigint) \
         ON CONFLICT (character_id) DO UPDATE SET name = EXCLUDED.name, \
         corporation_id = EXCLUDED.corporation_id, alliance_id = EXCLUDED.alliance_id, seen_at = now()",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    ) {
        log::warn(format!("remembering characters: {err:?}"));
    }
}

/// Stores names for corporation and alliance ids we don't have yet, from
/// public ESI. Best effort: names fall back to ids.
fn learn_names(ids: &[i64]) {
    let mut ids: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return;
    }
    let known = match storage::query(
        "SELECT id FROM names WHERE id = ANY(string_to_array($1, ',')::bigint[])",
        &[id_list(ids.iter().copied())],
    ) {
        Ok(rows) => rows.rows.iter().map(|r| int(r, 0)).collect::<Vec<_>>(),
        Err(err) => {
            log::warn(format!("reading names: {err:?}"));
            return;
        }
    };
    let missing: Vec<i64> = ids.into_iter().filter(|id| !known.contains(id)).collect();
    if missing.is_empty() {
        return;
    }
    let named = match esi::names(&missing[..missing.len().min(1000)]) {
        Ok(named) => named,
        Err(err) => {
            log::warn(format!("names: {err:?}"));
            return;
        }
    };
    let rows: Vec<serde_json::Value> = named
        .into_iter()
        .map(|n| serde_json::json!({ "id": n.id, "name": n.name, "category": n.category }))
        .collect();
    if let Err(err) = storage::execute(
        "INSERT INTO names (id, name, category) \
         SELECT id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    ) {
        log::warn(format!("storing names: {err:?}"));
    }
}

fn name_of(id: i64, fallback: &str) -> Result<String, PageError> {
    let rows = query("SELECT name FROM names WHERE id = $1", &[id.into()])?;
    Ok(rows
        .first()
        .map_or_else(|| format!("{fallback} {id}"), |r| text(r, 0)))
}

// ---- links -----------------------------------------------------------------

struct LinkInfo {
    id: i64,
    hash: String,
    fleet: String,
    fleet_type: Option<String>,
    doctrine: Option<String>,
    creator_account: i64,
    creator_name: String,
    created_at: String,
    expires_at: String,
    reopened: i64,
    fats: i64,
    open: bool,
}

const LINK_COLUMNS: &str = "l.id, l.hash, l.fleet, l.fleet_type, l.doctrine, l.creator_account, \
     l.creator_name, l.created_at, l.expires_at, l.reopened, \
     (SELECT count(*) FROM fats f WHERE f.link_id = l.id)::bigint, l.expires_at > now()";

fn link_info(row: &[Db]) -> LinkInfo {
    LinkInfo {
        id: int(row, 0),
        hash: text(row, 1),
        fleet: text(row, 2),
        fleet_type: maybe_text(row, 3),
        doctrine: maybe_text(row, 4),
        creator_account: int(row, 5),
        creator_name: text(row, 6),
        created_at: text(row, 7),
        expires_at: text(row, 8),
        reopened: int(row, 9),
        fats: int(row, 10),
        open: flag(row, 11),
    }
}

fn load_link(hash: &str) -> Result<LinkInfo, PageError> {
    if !valid_hash(hash) {
        return Err(PageError::NotFound);
    }
    let rows = query(
        &format!("SELECT {LINK_COLUMNS} FROM links l WHERE l.hash = $1"),
        &[hash.into()],
    )?;
    rows.first()
        .map(|r| link_info(r))
        .ok_or(PageError::NotFound)
}

fn status(link: &LinkInfo) -> Value {
    if link.open {
        badge("Open", Tone::Success).into()
    } else {
        badge("Closed", Tone::Neutral).into()
    }
}

/// The fleet's name, a link to its page for those who may open it.
fn fleet_cell(viewer: &Viewer, link: &LinkInfo) -> Value {
    if can_create(viewer) {
        tether_plugin_sdk::link(link.fleet.clone(), format!("links/{}", link.hash)).into()
    } else {
        link.fleet.clone().into()
    }
}

fn links_table(viewer: &Viewer, title: &str, empty: &str, links: &[LinkInfo]) -> Table {
    with_rows(
        Table::new(vec![
            Column::text("Fleet"),
            Column::text("Fleet type"),
            Column::text("Created by"),
            Column::numeric("Created"),
            Column::numeric("FATs"),
            Column::text("Status"),
        ])
        .title(title)
        .empty(empty),
        links.iter().map(|l| {
            vec![
                fleet_cell(viewer, l),
                l.fleet_type.clone().unwrap_or_default().into(),
                l.creator_name.clone().into(),
                time(l.created_at.clone()),
                l.fats.into(),
                status(l),
            ]
        }),
    )
}

/// Enabled fleet types as select options, plus `keep` (a link's current
/// type, even if it was disabled since).
fn type_options(keep: Option<&str>) -> Result<Vec<(String, String)>, PageError> {
    let rows = query(
        "SELECT name FROM fleet_types WHERE enabled ORDER BY lower(name)",
        &[],
    )?;
    let mut options = vec![(String::new(), "No fleet type".to_owned())];
    options.extend(rows.iter().map(|r| (text(r, 0), text(r, 0))));
    if let Some(keep) = keep
        && !options.iter().any(|(v, _)| v == keep)
    {
        options.push((keep.to_owned(), keep.to_owned()));
    }
    Ok(options)
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

// ---- pages -----------------------------------------------------------------

fn dashboard(viewer: &Viewer) -> Result<Page, PageError> {
    let mine = id_list(viewer.characters.iter().map(|c| c.id));
    let now = Utc::now();
    let month_start = format!("{:04}-{:02}-01T00:00:00Z", now.year(), now.month());
    let (year_start, _) = year_bounds(now.year());
    let totals = query(
        "SELECT count(*) FILTER (WHERE l.created_at >= $2)::bigint, \
                count(*) FILTER (WHERE l.created_at >= $3)::bigint, count(*)::bigint \
         FROM fats f JOIN links l ON l.id = f.link_id \
         WHERE f.character_id = ANY(string_to_array($1, ',')::bigint[])",
        &[mine.clone(), Db::timestamp(month_start), year_start],
    )?;
    let totals = totals.first();
    let recent = query(
        "SELECT f.character_name, l.fleet, coalesce(l.fleet_type, ''), l.created_at \
         FROM fats f JOIN links l ON l.id = f.link_id \
         WHERE f.character_id = ANY(string_to_array($1, ',')::bigint[]) \
         ORDER BY l.created_at DESC LIMIT 20",
        &[mine],
    )?;
    let mut stats = vec![
        Stat::new("FATs this month", totals.map_or(0, |r| int(r, 0))),
        Stat::new("FATs this year", totals.map_or(0, |r| int(r, 1))),
        Stat::new("FATs all time", totals.map_or(0, |r| int(r, 2))),
    ];
    let mut page = Page::new("Fleet Activity Tracking")
        .description("Your fleet attendance (FATs). Open the FAT link your FC shares to register.");
    let open: Vec<LinkInfo> = if can_create(viewer) {
        query(
            &format!(
                "SELECT {LINK_COLUMNS} FROM links l WHERE l.expires_at > now() \
                 ORDER BY l.created_at DESC LIMIT 50"
            ),
            &[],
        )?
        .iter()
        .map(|r| link_info(r))
        .collect()
    } else {
        Vec::new()
    };
    if can_create(viewer) {
        stats.push(Stat::new("Open FAT links", count(open.len())));
    }
    page = page.stats(stats).table(with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Fleet"),
            Column::text("Fleet type"),
            Column::numeric("EVE time"),
        ])
        .title("Your most recent FATs")
        .empty("No FATs yet. Open the FAT link your FC shares in fleet to register one."),
        recent.iter().map(|r| {
            vec![
                text(r, 0).into(),
                text(r, 1).into(),
                text(r, 2).into(),
                time(text(r, 3)),
            ]
        }),
    ));
    if can_create(viewer) {
        page = page.table(links_table(
            viewer,
            "Open FAT links",
            "No FAT links are open.",
            &open,
        ));
    }
    let latest: Vec<LinkInfo> = query(
        &format!("SELECT {LINK_COLUMNS} FROM links l ORDER BY l.created_at DESC LIMIT 10"),
        &[],
    )?
    .iter()
    .map(|r| link_info(r))
    .collect();
    page = page.table(links_table(
        viewer,
        "Recent FAT links",
        "No FAT links yet.",
        &latest,
    ));
    Ok(page.card(menu(viewer)))
}

/// Links to the app's pages the viewer may open.
fn menu(viewer: &Viewer) -> Card {
    let mut card = Card::new("Fleet Activity Tracking")
        .field("FAT Links", link("Every FAT link", "links"))
        .field("Statistics", link("Attendance by month", "stats"));
    if can_create(viewer) {
        card = card.field(
            "Create FAT Link",
            link("For a fleet you run", "links/create"),
        );
    }
    if viewer.can("manage_afat") {
        card = card.field("Fleet types", link("Manage fleet types", "fleet-types"));
    }
    if viewer.can("logs_view") {
        card = card.field("Logs", link("What FCs and managers did", "logs"));
    }
    card
}

fn links_page(viewer: &Viewer, page_number: i64) -> Result<Page, PageError> {
    let total = query("SELECT count(*)::bigint FROM links", &[])?
        .first()
        .map_or(0, |r| int(r, 0));
    let pages = ((total + LINKS_PER_PAGE - 1) / LINKS_PER_PAGE).max(1);
    if page_number > pages {
        return Err(PageError::NotFound);
    }
    let links: Vec<LinkInfo> = query(
        &format!(
            "SELECT {LINK_COLUMNS} FROM links l ORDER BY l.created_at DESC, l.id DESC LIMIT $1 OFFSET $2"
        ),
        &[
            LINKS_PER_PAGE.into(),
            ((page_number - 1) * LINKS_PER_PAGE).into(),
        ],
    )?
    .iter()
    .map(|r| link_info(r))
    .collect();
    let mut page = Page::new("FAT Links")
        .description("Every FAT link, newest first. FCs share a link's register page in fleet.")
        .stats(vec![Stat::new("FAT links", total)])
        .table(links_table(
            viewer,
            &format!("Page {page_number} of {pages}"),
            "No FAT links yet.",
            &links,
        ));
    let mut more = Card::new("More");
    if can_create(viewer) {
        more = more.field(
            "Create FAT Link",
            link("For a fleet you run", "links/create"),
        );
    }
    if page_number > 1 {
        more = more.field(
            "Newer",
            link(
                format!("Page {}", page_number - 1),
                format!("links/page/{}", page_number - 1),
            ),
        );
    }
    if page_number < pages {
        more = more.field(
            "Older",
            link(
                format!("Page {}", page_number + 1),
                format!("links/page/{}", page_number + 1),
            ),
        );
    }
    if !more.fields.is_empty() {
        page = page.card(more);
    }
    Ok(page)
}

fn create_page(viewer: &Viewer, note: Option<&str>) -> Result<Page, PageError> {
    if !can_create(viewer) {
        return Err(PageError::Forbidden);
    }
    let mut page = Page::new("Create FAT Link").description(
        "Members open the link's register page while it's open to record their attendance.",
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page.form(
        Form::new("create", "Create FAT link")
            .field(Field::text("fleet", "Fleet name", MAX_FLEET).required())
            .field(
                Field::select("fleet_type", "Fleet type", type_options(None)?)
                    .help("Managers keep the list of fleet types."),
            )
            .field(Field::text("doctrine", "Doctrine", MAX_DOCTRINE))
            .field(
                Field::number("expiry", "Open for (minutes)")
                    .range(Some(1.0), Some(MAX_EXPIRY_MINUTES as f64), true)
                    .value(DEFAULT_EXPIRY_MINUTES.to_string())
                    .help("After this, nobody can register; you can close it sooner or reopen it once.")
                    .required(),
            ),
    ))
}

fn minutes(submission: &Submission, name: &str) -> Result<i64, PageError> {
    submission
        .value(name)
        .parse::<i64>()
        .ok()
        .filter(|m| (1..=MAX_EXPIRY_MINUTES).contains(m))
        .ok_or_else(|| PageError::Failed(format!("{name} wasn't a number of minutes")))
}

fn create_link(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !can_create(viewer) {
        return Err(PageError::Forbidden);
    }
    let Some(fleet) = optional(submission.value("fleet")) else {
        return Ok(SubmitResult::Page(create_page(
            viewer,
            Some("Give the fleet a name."),
        )?));
    };
    let fleet_type = optional(submission.value("fleet_type"));
    let doctrine = optional(submission.value("doctrine"));
    let expiry = minutes(submission, "expiry")?;
    let expires = Utc::now() + Duration::minutes(expiry);
    let description = format!(
        "FAT link for \"{fleet}\" ({}), open for {expiry} minutes",
        fleet_type.as_deref().unwrap_or("no fleet type")
    );
    // The link and its log entry in one statement.
    let rows = query(
        "WITH created AS ( \
             INSERT INTO links (hash, fleet, fleet_type, doctrine, creator_account, creator_id, creator_name, expires_at) \
             VALUES (replace(gen_random_uuid()::text, '-', ''), $1, $2, $3, $4, $5, $6, $7) RETURNING hash) \
         INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
         SELECT 'Create FAT Link', $5, $6, hash, $8 FROM created RETURNING link_hash",
        &[
            fleet.into(),
            fleet_type.into(),
            doctrine.into(),
            viewer.account_id.into(),
            viewer.main.id.into(),
            viewer.main.name.clone().into(),
            Db::timestamp(rfc3339(expires)),
            description.into(),
        ],
    )?;
    let hash = rows
        .first()
        .map(|r| text(r, 0))
        .ok_or_else(|| PageError::Failed("the new link has no hash".into()))?;
    Ok(SubmitResult::Redirect(format!("links/{hash}")))
}

/// A link's page, for FCs and managers: its register link, attendees, and
/// the forms to change it.
fn details_page(viewer: &Viewer, hash: &str, note: Option<&str>) -> Result<Page, PageError> {
    if !can_create(viewer) {
        return Err(PageError::Forbidden);
    }
    let link = load_link(hash)?;
    let attendees = query(
        // Registration times to the minute only: characters ticked in one
        // submission share an instant, which would single out one account's
        // characters to every FC.
        "SELECT f.character_id, f.character_name, coalesce(c.name, ''), coalesce(a.name, ''), \
                date_trunc('minute', f.created_at), f.added_by, f.corporation_id, f.alliance_id \
         FROM fats f LEFT JOIN names c ON c.id = f.corporation_id LEFT JOIN names a ON a.id = f.alliance_id \
         WHERE f.link_id = $1 ORDER BY lower(f.character_name) LIMIT $2",
        &[link.id.into(), MAX_ATTENDEES.into()],
    )?;
    let mut page = Page::new(format!("FAT link: {}", link.fleet))
        .description("Share the register link in fleet; members open it to record their FAT.");
    if let Some(note) = note {
        page = page.text(note);
    }
    let status_stat = if link.open {
        badge("Open", Tone::Accent)
    } else {
        badge("Closed", Tone::Neutral)
    };
    page = page.stats(vec![
        Stat::new("Status", status_stat),
        Stat::new("FATs", link.fats),
        Stat::new(
            if link.open { "Closes" } else { "Closed" },
            time(link.expires_at.clone()),
        ),
    ]);
    let mut card = Card::new("FAT link")
        .field(
            "Register link",
            tether_plugin_sdk::link("Register attendance", format!("links/{}/add", link.hash)),
        )
        .field("Fleet", link.fleet.clone())
        .field(
            "Fleet type",
            link.fleet_type.clone().unwrap_or_else(|| "None".to_owned()),
        )
        .field(
            "Doctrine",
            link.doctrine.clone().unwrap_or_else(|| "None".to_owned()),
        )
        .field("Created by", link.creator_name.clone())
        .field("Created", time(link.created_at.clone()));
    if link.reopened > 0 {
        card = card.field("Reopened", link.reopened);
    }
    page = page.card(card).table(with_rows(
        Table::new(vec![
            Column::text("Character"),
            Column::text("Corporation"),
            Column::text("Alliance"),
            Column::numeric("Registered"),
            Column::text("How"),
        ])
        .title("Attendees")
        .empty("Nobody has registered yet."),
        attendees.iter().map(|r| {
            let corporation = match (text(r, 2), int(r, 6)) {
                (name, _) if !name.is_empty() => name,
                (_, 0) => "Unknown".to_owned(),
                (_, id) => format!("Corporation {id}"),
            };
            let alliance = match (text(r, 3), int(r, 7)) {
                (name, _) if !name.is_empty() => name,
                (_, 0) => String::new(),
                (_, id) => format!("Alliance {id}"),
            };
            vec![
                text(r, 1).into(),
                corporation.into(),
                alliance.into(),
                time(text(r, 4)),
                match maybe_text(r, 5) {
                    Some(by) => format!("Added by {by}").into(),
                    None => "Registered".into(),
                },
            ]
        }),
    ));
    if link.fats > MAX_ATTENDEES {
        page = page.text(format!(
            "Showing the first {MAX_ATTENDEES} of {} attendees; statistics count them all.",
            link.fats
        ));
    }
    if !can_edit(viewer, &link) {
        return Ok(page.text("Only the FC who created this link, or a manager, can change it."));
    }
    let manage = viewer.can("manage_afat");
    page = page.tab(
        "Edit",
        vec![Section::Form(
            Form::new("edit", "Save changes")
                .field(
                    Field::text("fleet", "Fleet name", MAX_FLEET)
                        .value(link.fleet.clone())
                        .required(),
                )
                .field(
                    Field::select(
                        "fleet_type",
                        "Fleet type",
                        type_options(link.fleet_type.as_deref())?,
                    )
                    .value(link.fleet_type.clone().unwrap_or_default()),
                )
                .field(
                    Field::text("doctrine", "Doctrine", MAX_DOCTRINE)
                        .value(link.doctrine.clone().unwrap_or_default()),
                ),
        )],
    );
    if link.open {
        page = page.tab(
            "Close",
            vec![Section::Form(
                Form::new("close", "Close FAT link")
                    .description("Nobody can register once it's closed.")
                    .field(Field::checkbox("confirm", "Close it now", false).required()),
            )],
        );
    } else if manage || link.reopened < REOPEN_LIMIT {
        page = page.tab(
            "Reopen",
            vec![Section::Form(
                Form::new("reopen", "Reopen FAT link")
                    .description(if manage {
                        "Members can register again for this long."
                    } else {
                        "Members can register again for this long. You can reopen a link once; after that, ask a manager."
                    })
                    .field(
                        Field::number("expiry", "Open for (minutes)")
                            .range(Some(1.0), Some(MAX_EXPIRY_MINUTES as f64), true)
                            .value(DEFAULT_EXPIRY_MINUTES.to_string())
                            .required(),
                    ),
            )],
        );
    } else {
        page = page.tab(
            "Reopen",
            vec![Section::Text(
                "You've reopened this link once already; a manager can reopen it again.".into(),
            )],
        );
    }
    page = page.tab(
        "Add FAT",
        vec![Section::Form(
            Form::new("add_fat", "Add FAT")
                .description(
                    "For a pilot who was in fleet but didn't register. Works after the link closes.",
                )
                .field(
                    Field::text("character", "Character name or ID", 40)
                        .help("A character that has used Fleet Activity Tracking, by exact name, or any character by its ID.")
                        .required(),
                ),
        )],
    );
    if manage {
        let remove = if link.fats == 0 {
            Section::Text("There are no FATs to remove.".into())
        } else if link.fats <= count(MAX_SELECT) {
            Section::Form(
                Form::new("remove_fat", "Remove FAT").field(
                    Field::select(
                        "character_id",
                        "Character",
                        attendees
                            .iter()
                            .map(|r| (int(r, 0).to_string(), text(r, 1)))
                            .collect(),
                    )
                    .required(),
                ),
            )
        } else {
            Section::Form(
                Form::new("remove_fat", "Remove FAT")
                    .field(Field::text("character_name", "Character name", 40).required()),
            )
        };
        page = page.tab("Remove FAT", vec![remove]).tab(
            "Delete",
            vec![Section::Form(
                Form::new("delete", "Delete FAT link")
                    .description(format!(
                        "Deletes this link and its {} FATs; statistics lose them. This can't be undone.",
                        link.fats
                    ))
                    .field(
                        Field::checkbox(
                            "confirm",
                            format!("Delete \"{}\" and its {} FATs", link.fleet, link.fats),
                            false,
                        )
                        .required(),
                    ),
            )],
        );
    }
    Ok(page)
}

fn change_link(
    viewer: &Viewer,
    hash: &str,
    form: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let link = load_link(hash)?;
    if !can_edit(viewer, &link) {
        return Err(PageError::Forbidden);
    }
    let manage = viewer.can("manage_afat");
    let back = || SubmitResult::Redirect(format!("links/{}", link.hash));
    match form {
        "edit" => {
            let Some(fleet) = optional(submission.value("fleet")) else {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some("Give the fleet a name."),
                )?));
            };
            let fleet_type = optional(submission.value("fleet_type"));
            let doctrine = optional(submission.value("doctrine"));
            run(
                &[
                    Statement::new(
                        "UPDATE links SET fleet = $2, fleet_type = $3, doctrine = $4 WHERE id = $1",
                        vec![
                            link.id.into(),
                            fleet.clone().into(),
                            fleet_type.clone().into(),
                            doctrine.clone().into(),
                        ],
                    ),
                    log_entry(
                        viewer,
                        "Change FAT Link",
                        Some(&link.hash),
                        format!(
                            "\"{}\" is now \"{fleet}\", fleet type {}, doctrine {}",
                            link.fleet,
                            fleet_type.as_deref().unwrap_or("none"),
                            doctrine.as_deref().unwrap_or("none")
                        ),
                    ),
                ],
                "changing the link",
            )?;
            Ok(back())
        }
        "close" => {
            run(
                &[
                    Statement::new(
                        "UPDATE links SET expires_at = now() WHERE id = $1 AND expires_at > now()",
                        vec![link.id.into()],
                    ),
                    log_entry(
                        viewer,
                        "Close FAT Link",
                        Some(&link.hash),
                        format!("\"{}\" closed early", link.fleet),
                    ),
                ],
                "closing the link",
            )?;
            Ok(back())
        }
        "reopen" => {
            if link.open {
                return Ok(back());
            }
            if !manage && link.reopened >= REOPEN_LIMIT {
                return Err(PageError::Forbidden);
            }
            let expiry = minutes(submission, "expiry")?;
            let expires = Utc::now() + Duration::minutes(expiry);
            // Checked again as it's changed, so two posts at once can't
            // both reopen it; logged in the same statement.
            let reopened = storage::execute(
                "WITH reopened AS ( \
                     UPDATE links SET expires_at = $2, reopened = reopened + 1 \
                     WHERE id = $1 AND expires_at <= now() AND ($3 OR reopened < $4) RETURNING hash) \
                 INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
                 SELECT 'Reopen FAT Link', $5, $6, hash, $7 FROM reopened",
                &[
                    link.id.into(),
                    Db::timestamp(rfc3339(expires)),
                    manage.into(),
                    REOPEN_LIMIT.into(),
                    viewer.main.id.into(),
                    viewer.main.name.clone().into(),
                    format!("\"{}\" reopened for {expiry} minutes", link.fleet).into(),
                ],
            )
            .map_err(|e| failed("reopening the link", e))?;
            if reopened == 0 {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some("The link was already reopened or is open again."),
                )?));
            }
            Ok(back())
        }
        "add_fat" => add_fat(viewer, &link, submission.value("character").trim()),
        "remove_fat" if manage => {
            let rows = query(
                "SELECT character_id, character_name FROM fats WHERE link_id = $1 \
                 AND (character_id::text = $2 OR lower(character_name) = lower($3))",
                &[
                    link.id.into(),
                    submission.value("character_id").into(),
                    submission.value("character_name").trim().into(),
                ],
            )?;
            let Some(row) = rows.first() else {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some("That character has no FAT on this link."),
                )?));
            };
            let (id, name) = (int(row, 0), text(row, 1));
            run(
                &[
                    Statement::new(
                        "DELETE FROM fats WHERE link_id = $1 AND character_id = $2",
                        vec![link.id.into(), id.into()],
                    ),
                    log_entry(
                        viewer,
                        "Delete FAT",
                        Some(&link.hash),
                        format!("FAT of {name} removed from \"{}\"", link.fleet),
                    ),
                ],
                "removing the FAT",
            )?;
            Ok(back())
        }
        "delete" if manage => {
            run(
                &[
                    log_entry(
                        viewer,
                        "Delete FAT Link",
                        Some(&link.hash),
                        format!(
                            "\"{}\" by {} deleted with its {} FATs",
                            link.fleet, link.creator_name, link.fats
                        ),
                    ),
                    Statement::new("DELETE FROM links WHERE id = $1", vec![link.id.into()]),
                ],
                "deleting the link",
            )?;
            Ok(SubmitResult::Redirect("links".into()))
        }
        "remove_fat" | "delete" => Err(PageError::Forbidden),
        _ => Err(PageError::NotFound),
    }
}

/// A manual FAT: by the exact name of a character the app has seen, or by
/// any character's id.
fn add_fat(viewer: &Viewer, link: &LinkInfo, who: &str) -> Result<SubmitResult, PageError> {
    let rows = query(
        "SELECT character_id, name, corporation_id, alliance_id FROM characters \
         WHERE lower(name) = lower($1) OR character_id::text = $1 ORDER BY seen_at DESC LIMIT 1",
        &[who.into()],
    )?;
    let found = match rows.first() {
        Some(r) => Some((
            int(r, 0),
            text(r, 1),
            Some(int(r, 2)),
            r.get(3).and_then(Db::as_integer),
        )),
        None => match who.parse::<i64>() {
            Ok(id) if id > 0 && who.len() <= 19 => match esi::names(&[id]) {
                Ok(named) => named
                    .into_iter()
                    .find(|n| n.id == id && n.category == "character")
                    .map(|n| (id, n.name, None, None)),
                Err(err) => {
                    log::warn(format!("names for a manual FAT: {err:?}"));
                    None
                }
            },
            _ => None,
        },
    };
    let Some((id, name, corporation, alliance)) = found else {
        return Ok(SubmitResult::Page(details_page(
            viewer,
            &link.hash,
            Some(&format!(
                "No character called \"{who}\" has used Fleet Activity Tracking yet. \
                 Enter their character ID instead."
            )),
        )?));
    };
    // The FAT and its log entry together, only if it wasn't there yet.
    let added = storage::execute(
        "WITH added AS ( \
             INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id, added_by) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (link_id, character_id) DO NOTHING RETURNING 1) \
         INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
         SELECT 'Manual FAT Added', $7, $6, $8, $9 FROM added",
        &[
            link.id.into(),
            id.into(),
            name.clone().into(),
            corporation.into(),
            alliance.into(),
            viewer.main.name.clone().into(),
            viewer.main.id.into(),
            link.hash.clone().into(),
            format!("FAT for {name} added to \"{}\"", link.fleet).into(),
        ],
    )
    .map_err(|e| failed("adding the FAT", e))?;
    if added == 0 {
        return Ok(SubmitResult::Page(details_page(
            viewer,
            &link.hash,
            Some(&format!("{name} already has a FAT on this link.")),
        )?));
    }
    learn_names(&[
        corporation.unwrap_or_default(),
        alliance.unwrap_or_default(),
    ]);
    Ok(SubmitResult::Redirect(format!("links/{}", link.hash)))
}

/// The page members open from the FC's link: their characters, to tick.
fn register_page(viewer: &Viewer, hash: &str, note: Option<&str>) -> Result<Page, PageError> {
    let link = load_link(hash)?;
    let registered = query(
        "SELECT f.character_id, f.character_name, f.created_at FROM fats f \
         WHERE f.link_id = $1 AND f.character_id = ANY(string_to_array($2, ',')::bigint[]) \
         ORDER BY lower(f.character_name)",
        &[
            link.id.into(),
            id_list(viewer.characters.iter().map(|c| c.id)),
        ],
    )?;
    let done: Vec<i64> = registered.iter().map(|r| int(r, 0)).collect();
    let mut page = Page::new(format!("Register FAT: {}", link.fleet));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.card(
        Card::new("Fleet")
            .field("Fleet", link.fleet.clone())
            .field(
                "Fleet type",
                link.fleet_type.clone().unwrap_or_else(|| "None".to_owned()),
            )
            .field(
                "Doctrine",
                link.doctrine.clone().unwrap_or_else(|| "None".to_owned()),
            )
            .field("FC", link.creator_name.clone())
            .field(
                if link.open { "Closes" } else { "Closed" },
                time(link.expires_at.clone()),
            ),
    );
    if !registered.is_empty() {
        page = page.table(with_rows(
            Table::new(vec![
                Column::text("Character"),
                Column::numeric("Registered"),
            ])
            .title("Registered for this fleet"),
            registered
                .iter()
                .map(|r| vec![text(r, 1).into(), time(text(r, 2))]),
        ));
    }
    if !link.open {
        return Ok(page
            .description("This FAT link is closed.")
            .text("Registration closed with the link. If you were in the fleet, ask the FC to reopen it or to add your FAT."));
    }
    let mut left: Vec<_> = viewer
        .characters
        .iter()
        .filter(|c| !done.contains(&c.id))
        .collect();
    if left.is_empty() {
        return Ok(page.description("All your characters are registered for this fleet."));
    }
    // The main first, then by name.
    left.sort_by_key(|c| (c.id != viewer.main.id, c.name.to_lowercase()));
    let mut form = Form::new("register", "Register")
        .title("Your characters in this fleet")
        .description("Tick every character you brought.");
    for character in left.iter().take(MAX_FORM_CHARACTERS) {
        form = form.field(Field::checkbox(
            format!("c_{}", character.id),
            character.name.clone(),
            character.id == viewer.main.id || left.len() == 1,
        ));
    }
    Ok(page
        .description("Register your attendance while the link is open.")
        .form(form))
}

fn register(
    viewer: &Viewer,
    hash: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let link = load_link(hash)?;
    let chosen: Vec<serde_json::Value> = viewer
        .characters
        .iter()
        .filter(|c| submission.checked(&format!("c_{}", c.id)))
        .map(|c| {
            serde_json::json!({
                "character_id": c.id,
                "character_name": c.name,
                "corporation_id": c.corporation_id,
                "alliance_id": c.alliance_id,
            })
        })
        .collect();
    if chosen.is_empty() {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some("Tick at least one character."),
        )?));
    }
    // Only while the link is open, checked by the database as it inserts;
    // a character already registered is left alone.
    let added = query(
        "INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id) \
         SELECT l.id, x.character_id, x.character_name, NULLIF(x.corporation_id, 0), NULLIF(x.alliance_id, 0) \
         FROM json_to_recordset($1::json) AS x(character_id bigint, character_name text, corporation_id bigint, alliance_id bigint) \
         JOIN links l ON l.id = $2 AND l.expires_at > now() \
         ON CONFLICT (link_id, character_id) DO NOTHING RETURNING corporation_id, alliance_id",
        &[
            Db::json(serde_json::Value::Array(chosen).to_string()),
            link.id.into(),
        ],
    )?;
    if added.is_empty() && !load_link(hash)?.open {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some("The link closed before you registered."),
        )?));
    }
    let ids: Vec<i64> = added.iter().flat_map(|r| [int(r, 0), int(r, 1)]).collect();
    learn_names(&ids);
    Ok(SubmitResult::Redirect(format!("links/{}/add", link.hash)))
}

// ---- statistics ------------------------------------------------------------

/// Rows of (key, label, month 1-12, count) pivoted into one row per key
/// with a count per month, busiest first.
fn pivot(rows: &[Vec<Db>]) -> Vec<(i64, String, [i64; 12])> {
    let mut out: Vec<(i64, String, [i64; 12])> = Vec::new();
    for row in rows {
        let (key, label, month, n) = (int(row, 0), text(row, 1), int(row, 2), int(row, 3));
        let Some(slot) = usize::try_from(month - 1).ok().filter(|m| *m < 12) else {
            continue;
        };
        match out.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, _, months)) => months[slot] += n,
            None => {
                let mut months = [0; 12];
                months[slot] = n;
                out.push((key, label, months));
            }
        }
    }
    out.sort_by(|a, b| {
        let (ta, tb): (i64, i64) = (a.2.iter().sum(), b.2.iter().sum());
        tb.cmp(&ta)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    out
}

/// A table with a column per month and a total.
fn month_table(
    first: &str,
    title: &str,
    empty: &str,
    rows: &[(i64, String, [i64; 12])],
    cell: impl Fn(i64, &str) -> Value,
) -> Table {
    let mut columns = vec![Column::text(first)];
    columns.extend(MONTHS.iter().map(|m| Column::numeric(*m)));
    columns.push(Column::numeric("Total"));
    let mut table = Table::new(columns).title(title).empty(empty);
    for (key, label, months) in rows.iter().take(MAX_STAT_ROWS) {
        let mut row = vec![cell(*key, label)];
        row.extend(months.iter().map(|n| Value::from(*n)));
        row.push(months.iter().sum::<i64>().into());
        table = table.row(row);
    }
    table
}

const MONTH: &str = "extract(month FROM l.created_at AT TIME ZONE 'UTC')::bigint";

/// FATs in `year` grouped by `key` (with `label`) and month, filtered by
/// `filter` on `$3` onwards. The SQL fragments are this file's own
/// constants, never input.
fn grouped(
    key: &str,
    label: &str,
    joins: &str,
    filter: &str,
    year: i32,
    params: &[Db],
) -> Result<Vec<(i64, String, [i64; 12])>, PageError> {
    let (start, end) = year_bounds(year);
    let mut all = vec![start, end];
    all.extend(params.iter().cloned());
    let rows = query(
        &format!(
            "SELECT {key}, max({label}), {MONTH}, count(*)::bigint \
             FROM fats f JOIN links l ON l.id = f.link_id {joins} \
             WHERE l.created_at >= $1 AND l.created_at < $2 AND {filter} \
             GROUP BY 1, 3 LIMIT 4800"
        ),
        &all,
    )?;
    Ok(pivot(&rows))
}

const CORPORATION_LABEL: &str = "coalesce(n.name, 'Corporation ' || f.corporation_id::text)";
const ALLIANCE_LABEL: &str = "coalesce(n.name, 'Alliance ' || f.alliance_id::text)";

fn year_card(path: &str, year: i32) -> Card {
    let mut card = Card::new("Year").field("Showing", year.to_string());
    card = card.field(
        "Previous year",
        link((year - 1).to_string(), format!("{path}/{}", year - 1)),
    );
    if year < this_year() {
        card = card.field(
            "Next year",
            link((year + 1).to_string(), format!("{path}/{}", year + 1)),
        );
    }
    card
}

/// The viewer's "own corporation" for stats_corporation_own: the main's,
/// as in aa-afat (an alt parked in another corporation doesn't count).
fn viewer_corporations(viewer: &Viewer) -> Vec<i64> {
    Some(viewer.main.corporation_id)
        .filter(|id| *id > 0)
        .into_iter()
        .collect()
}

/// Statistics: your characters, and the corporations and alliances the
/// viewer may see, by month.
fn stats_page(viewer: &Viewer, year: i32) -> Result<Page, PageError> {
    let mine = grouped(
        "f.character_id",
        "f.character_name",
        "",
        "f.character_id = ANY(string_to_array($3, ',')::bigint[])",
        year,
        &[id_list(viewer.characters.iter().map(|c| c.id))],
    )?;
    let character_link = |id: i64, name: &str| -> Value {
        link(name, format!("stats/character/{id}/{year}")).into()
    };
    let mut page = Page::new(format!("Statistics {year}"))
        .description("FATs by month, counted by when the fleet's link was created (EVE time).")
        .table(month_table(
            "Character",
            "Your characters",
            "No FATs this year.",
            &mine,
            character_link,
        ));
    let other = viewer.can("stats_corporation_other");
    let corporation_link = |id: i64, name: &str| -> Value {
        link(name, format!("stats/corporation/{id}/{year}")).into()
    };
    if other {
        let corporations = grouped(
            "f.corporation_id",
            CORPORATION_LABEL,
            "LEFT JOIN names n ON n.id = f.corporation_id",
            "f.corporation_id IS NOT NULL",
            year,
            &[],
        )?;
        let alliances = grouped(
            "f.alliance_id",
            ALLIANCE_LABEL,
            "LEFT JOIN names n ON n.id = f.alliance_id",
            "f.alliance_id IS NOT NULL",
            year,
            &[],
        )?;
        page = page
            .tab(
                "Alliances",
                vec![Section::Table(month_table(
                    "Alliance",
                    "By alliance",
                    "No alliance FATs this year.",
                    &alliances,
                    |id, name| link(name, format!("stats/alliance/{id}/{year}")).into(),
                ))],
            )
            .tab(
                "Corporations",
                vec![Section::Table(month_table(
                    "Corporation",
                    "By corporation",
                    "No FATs this year.",
                    &corporations,
                    corporation_link,
                ))],
            );
    } else if viewer.can("stats_corporation_own") {
        let corporations = grouped(
            "f.corporation_id",
            CORPORATION_LABEL,
            "LEFT JOIN names n ON n.id = f.corporation_id",
            "f.corporation_id = ANY(string_to_array($3, ',')::bigint[])",
            year,
            &[id_list(viewer_corporations(viewer))],
        )?;
        page = page.tab(
            "Your corporation",
            vec![Section::Table(month_table(
                "Corporation",
                "By corporation",
                "Your main's corporation has no FATs this year.",
                &corporations,
                corporation_link,
            ))],
        );
    }
    Ok(page.card(year_card("stats", year)))
}

/// Totals for a scope: FATs, pilots and fleets, then a row per month.
fn scope_summary(filter: &str, id: i64, year: i32) -> Result<(Vec<Stat>, Table), PageError> {
    let (start, end) = year_bounds(year);
    let params = [start, end, id.into()];
    let totals = query(
        &format!(
            "SELECT count(*)::bigint, count(DISTINCT f.character_id)::bigint, count(DISTINCT f.link_id)::bigint \
             FROM fats f JOIN links l ON l.id = f.link_id \
             WHERE l.created_at >= $1 AND l.created_at < $2 AND {filter}"
        ),
        &params,
    )?;
    let totals = totals.first();
    let (fats, pilots, fleets) = (
        totals.map_or(0, |r| int(r, 0)),
        totals.map_or(0, |r| int(r, 1)),
        totals.map_or(0, |r| int(r, 2)),
    );
    let months = query(
        &format!(
            "SELECT {MONTH}, count(*)::bigint, count(DISTINCT f.character_id)::bigint, \
                    count(DISTINCT f.link_id)::bigint \
             FROM fats f JOIN links l ON l.id = f.link_id \
             WHERE l.created_at >= $1 AND l.created_at < $2 AND {filter} GROUP BY 1 ORDER BY 1"
        ),
        &params,
    )?;
    let average = if pilots > 0 {
        format!("{:.1}", fats as f64 / pilots as f64)
    } else {
        "0".to_owned()
    };
    let stats = vec![
        Stat::new("FATs", fats),
        Stat::new("Pilots", pilots),
        Stat::new("Fleets", fleets),
        Stat::new("FATs per pilot", average),
    ];
    let table = with_rows(
        Table::new(vec![
            Column::text("Month"),
            Column::numeric("FATs"),
            Column::numeric("Pilots"),
            Column::numeric("Fleets"),
        ])
        .title("By month")
        .empty("No FATs this year."),
        months.iter().map(|r| {
            let month = usize::try_from(int(r, 0) - 1)
                .ok()
                .and_then(|m| MONTHS.get(m))
                .copied()
                .unwrap_or("?");
            vec![
                format!("{month} {year}").into(),
                int(r, 1).into(),
                int(r, 2).into(),
                int(r, 3).into(),
            ]
        }),
    );
    Ok((stats, table))
}

fn fleet_type_table(filter: &str, id: i64, year: i32) -> Result<Table, PageError> {
    let (start, end) = year_bounds(year);
    let rows = query(
        &format!(
            "SELECT coalesce(l.fleet_type, 'No fleet type'), count(*)::bigint \
             FROM fats f JOIN links l ON l.id = f.link_id \
             WHERE l.created_at >= $1 AND l.created_at < $2 AND {filter} \
             GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 100"
        ),
        &[start, end, id.into()],
    )?;
    Ok(with_rows(
        Table::new(vec![Column::text("Fleet type"), Column::numeric("FATs")])
            .title("By fleet type")
            .empty("No FATs this year."),
        rows.iter()
            .map(|r| vec![text(r, 0).into(), int(r, 1).into()]),
    ))
}

fn corporation_page(viewer: &Viewer, id: i64, year: i32) -> Result<Page, PageError> {
    let allowed = viewer.can("stats_corporation_other")
        || (viewer.can("stats_corporation_own") && viewer_corporations(viewer).contains(&id));
    if !allowed {
        return Err(PageError::Forbidden);
    }
    let filter = "f.corporation_id = $3";
    let name = name_of(id, "Corporation")?;
    let (stats, months) = scope_summary(filter, id, year)?;
    let pilots = grouped(
        "f.character_id",
        "f.character_name",
        "",
        filter,
        year,
        &[id.into()],
    )?;
    Ok(Page::new(format!("{name}: statistics {year}"))
        .description("FATs of the corporation's pilots, by month (EVE time).")
        .stats(stats)
        .table(months)
        .table(month_table(
            "Pilot",
            "By pilot",
            "No FATs this year.",
            &pilots,
            |cid, n| link(n, format!("stats/character/{cid}/{year}")).into(),
        ))
        .table(fleet_type_table(filter, id, year)?)
        .card(year_card(&format!("stats/corporation/{id}"), year)))
}

fn alliance_page(viewer: &Viewer, id: i64, year: i32) -> Result<Page, PageError> {
    if !viewer.can("stats_corporation_other") {
        return Err(PageError::Forbidden);
    }
    let filter = "f.alliance_id = $3";
    let name = name_of(id, "Alliance")?;
    let (stats, months) = scope_summary(filter, id, year)?;
    let corporations = grouped(
        "f.corporation_id",
        CORPORATION_LABEL,
        "LEFT JOIN names n ON n.id = f.corporation_id",
        &format!("{filter} AND f.corporation_id IS NOT NULL"),
        year,
        &[id.into()],
    )?;
    Ok(Page::new(format!("{name}: statistics {year}"))
        .description("FATs of the alliance's pilots, by month (EVE time).")
        .stats(stats)
        .table(months)
        .table(month_table(
            "Corporation",
            "By corporation",
            "No FATs this year.",
            &corporations,
            |cid, n| link(n, format!("stats/corporation/{cid}/{year}")).into(),
        ))
        .table(fleet_type_table(filter, id, year)?)
        .card(year_card(&format!("stats/alliance/{id}"), year)))
}

fn character_page(viewer: &Viewer, id: i64, year: i32) -> Result<Page, PageError> {
    let own = viewer.characters.iter().any(|c| c.id == id);
    let allowed = own
        || viewer.can("stats_corporation_other")
        // A pilot of the main's corporation: now, or (for characters the
        // app hasn't seen) when their FATs were recorded.
        || (viewer.can("stats_corporation_own")
            && !query(
                "SELECT 1 WHERE EXISTS (SELECT 1 FROM characters \
                     WHERE character_id = $1 AND corporation_id = ANY(string_to_array($2, ',')::bigint[])) \
                 OR (NOT EXISTS (SELECT 1 FROM characters WHERE character_id = $1) \
                     AND EXISTS (SELECT 1 FROM fats WHERE character_id = $1 \
                         AND corporation_id = ANY(string_to_array($2, ',')::bigint[])))",
                &[id.into(), id_list(viewer_corporations(viewer))],
            )?
            .is_empty());
    if !allowed {
        return Err(PageError::Forbidden);
    }
    let filter = "f.character_id = $3";
    let (start, end) = year_bounds(year);
    let fats = query(
        "SELECT f.character_name, l.fleet, coalesce(l.fleet_type, ''), l.created_at, l.hash \
         FROM fats f JOIN links l ON l.id = f.link_id \
         WHERE l.created_at >= $1 AND l.created_at < $2 AND f.character_id = $3 \
         ORDER BY l.created_at DESC LIMIT 500",
        &[start, end, id.into()],
    )?;
    let name = match fats.first() {
        Some(r) => text(r, 0),
        None => query(
            "SELECT name FROM characters WHERE character_id = $1",
            &[id.into()],
        )?
        .first()
        .map_or_else(|| format!("Character {id}"), |r| text(r, 0)),
    };
    let (stats, months) = scope_summary(filter, id, year)?;
    let fleets = can_create(viewer);
    Ok(Page::new(format!("{name}: statistics {year}"))
        .description("This character's FATs, by month (EVE time).")
        .stats(stats.into_iter().take(1).collect())
        .table(months)
        .table(fleet_type_table(filter, id, year)?)
        .table(with_rows(
            Table::new(vec![
                Column::text("Fleet"),
                Column::text("Fleet type"),
                Column::numeric("EVE time"),
            ])
            .title("FATs")
            .empty("No FATs this year."),
            fats.iter().map(|r| {
                let fleet: Value = if fleets {
                    link(text(r, 1), format!("links/{}", text(r, 4))).into()
                } else {
                    text(r, 1).into()
                };
                vec![fleet, text(r, 2).into(), time(text(r, 3))]
            }),
        ))
        .card(year_card(&format!("stats/character/{id}"), year)))
}

// ---- fleet types -----------------------------------------------------------

fn fleet_types_page(note: Option<&str>) -> Result<Page, PageError> {
    let rows = query(
        "SELECT t.id, t.name, t.enabled, \
                (SELECT count(*) FROM links l WHERE l.fleet_type = t.name)::bigint \
         FROM fleet_types t ORDER BY lower(t.name)",
        &[],
    )?;
    let mut page = Page::new("Fleet types")
        .description("The fleet types FCs pick from when they create a FAT link.");
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page
        .table(with_rows(
            Table::new(vec![
                Column::text("Fleet type"),
                Column::text("Status"),
                Column::numeric("FAT links"),
            ])
            .empty("No fleet types yet: add the kinds of fleets you run (CTA, Home Defense, Mining...)."),
            rows.iter().map(|r| {
                let status = if flag(r, 2) {
                    badge("Enabled", Tone::Success)
                } else {
                    badge("Disabled", Tone::Neutral)
                };
                vec![text(r, 1).into(), status.into(), int(r, 3).into()]
            }),
        ))
        .form(
            Form::new("add_type", "Add fleet type")
                .field(Field::text("name", "Name", MAX_TYPE_NAME).required()),
        );
    if !rows.is_empty() {
        page = page.form(
            Form::new("change_type", "Apply")
                .title("Change a fleet type")
                .description(
                    "Disabled types aren't offered for new links. Deleting one keeps it on the links that used it.",
                )
                .field(
                    Field::select(
                        "type",
                        "Fleet type",
                        rows.iter()
                            .map(|r| (int(r, 0).to_string(), text(r, 1)))
                            .collect(),
                    )
                    .required(),
                )
                .field(
                    Field::select(
                        "action",
                        "Action",
                        vec![
                            ("enable".into(), "Enable".into()),
                            ("disable".into(), "Disable".into()),
                            ("delete".into(), "Delete".into()),
                        ],
                    )
                    .required(),
                ),
        );
    }
    Ok(page)
}

fn change_fleet_types(
    viewer: &Viewer,
    form: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if !viewer.can("manage_afat") {
        return Err(PageError::Forbidden);
    }
    match form {
        "add_type" => {
            let Some(name) = optional(submission.value("name")) else {
                return Ok(SubmitResult::Page(fleet_types_page(Some(
                    "Give the fleet type a name.",
                ))?));
            };
            let added = storage::transaction(&[
                Statement::new(
                    "INSERT INTO fleet_types (name) VALUES ($1)",
                    vec![name.clone().into()],
                ),
                log_entry(
                    viewer,
                    "Fleet Type Added",
                    None,
                    format!("Fleet type \"{name}\" added"),
                ),
            ]);
            match added {
                Ok(_) => Ok(SubmitResult::Redirect("fleet-types".into())),
                Err(storage::Error::Database(e)) if e.code == "23505" => {
                    Ok(SubmitResult::Page(fleet_types_page(Some(&format!(
                        "There's already a fleet type called \"{name}\"."
                    )))?))
                }
                Err(e) => Err(failed("adding the fleet type", e)),
            }
        }
        "change_type" => {
            let id: i64 = submission
                .value("type")
                .parse()
                .map_err(|_| PageError::NotFound)?;
            let name = query("SELECT name FROM fleet_types WHERE id = $1", &[id.into()])?
                .first()
                .map(|r| text(r, 0))
                .ok_or(PageError::NotFound)?;
            let (sql, event, done) = match submission.value("action") {
                "enable" => (
                    "UPDATE fleet_types SET enabled = true WHERE id = $1",
                    "Fleet Type Changed",
                    "enabled",
                ),
                "disable" => (
                    "UPDATE fleet_types SET enabled = false WHERE id = $1",
                    "Fleet Type Changed",
                    "disabled",
                ),
                "delete" => (
                    "DELETE FROM fleet_types WHERE id = $1",
                    "Fleet Type Deleted",
                    "deleted",
                ),
                _ => return Err(PageError::NotFound),
            };
            run(
                &[
                    Statement::new(sql, vec![id.into()]),
                    log_entry(viewer, event, None, format!("Fleet type \"{name}\" {done}")),
                ],
                "changing the fleet type",
            )?;
            Ok(SubmitResult::Redirect("fleet-types".into()))
        }
        _ => Err(PageError::NotFound),
    }
}

// ---- logs ------------------------------------------------------------------

fn logs_page(viewer: &Viewer) -> Result<Page, PageError> {
    let rows = query(
        "SELECT g.at, g.event, g.actor_name, g.link_hash, g.description, l.hash IS NOT NULL \
         FROM logs g LEFT JOIN links l ON l.hash = g.link_hash \
         ORDER BY g.at DESC, g.id DESC LIMIT 500",
        &[],
    )?;
    let links = can_create(viewer);
    Ok(Page::new("Logs")
        .description(format!(
            "What FCs and managers did, newest first. Kept for {LOG_DAYS} days."
        ))
        .table(with_rows(
            Table::new(vec![
                Column::numeric("EVE time"),
                Column::text("Event"),
                Column::text("By"),
                Column::text("Details"),
                Column::text("FAT link"),
            ])
            .empty("Nothing logged yet."),
            rows.iter().map(|r| {
                let target: Value = match maybe_text(r, 3) {
                    Some(hash) if links && flag(r, 5) => {
                        link("Open", format!("links/{hash}")).into()
                    }
                    Some(_) if !flag(r, 5) => "Deleted".into(),
                    _ => "".into(),
                };
                vec![
                    time(text(r, 0)),
                    text(r, 1).into(),
                    text(r, 2).into(),
                    text(r, 4).into(),
                    target,
                ]
            }),
        )))
}

// ---- jobs ------------------------------------------------------------------

/// Daily: logs older than LOG_DAYS go.
fn housekeeping() -> Result<(), JobError> {
    let removed = storage::execute(
        &format!("DELETE FROM logs WHERE at < now() - interval '{LOG_DAYS} days'"),
        &[],
    )
    .map_err(|e| JobError::Retry(format!("clearing old logs: {e:?}")))?;
    if removed > 0 {
        log::info(format!("cleared {removed} old log entries"));
    }
    Ok(())
}
