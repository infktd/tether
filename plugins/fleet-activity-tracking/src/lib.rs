//! Fleet Activity Tracking (Alliance Auth's name for it, with aa-afat's
//! additions; PRD F23).
//!
//! - **FAT links**: an FC creates one for a fleet, with a fleet type, a
//!   doctrine and an expiry, and shares its link. Members open it and
//!   register their characters' attendance (a FAT) while it's open;
//!   multiboxers tick every character they brought. As aa-afat, each
//!   character must be online in EVE: its token (the app's location
//!   scopes, registered for this app) shows ESI that it is, and the FAT
//!   records its system and ship.
//! - As aa-afat, everyone with `add_fatlink` (or `manage_afat`) changes
//!   any link: renames it, closes it, reopens it once within the reopen
//!   grace time for the reopen duration, and adds FATs by hand within 24
//!   hours of its creation and before it was reopened. Managers remove
//!   FATs, delete links and keep the fleet types and the settings.
//! - **Settings** (aa-afat's Setting): a new link's default expiry, the
//!   reopen grace time and duration, and how long logs are kept.
//! - **Statistics** per pilot, corporation and alliance, by month, behind
//!   aa-afat's permissions.
//! - **Logs** of what FCs and managers did, kept for the settings' days.
//! - **Fleet snapshot** (aa-afat's): within the manual FAT window, an FC
//!   pastes the fleet composition from EVE's fleet window, and everyone in
//!   it gets a FAT, with ship and system.
//! - **ESI-tracked fleets** (aa-afat's): a link can follow the fleet an FC's
//!   character is boss of, adding a FAT (with ship and system) for everyone
//!   in it. As in aa-afat, the FC logs in with the fleet boss from Create
//!   FAT Link (Tether's Add data source: the character becomes the app's data
//!   source, no approval), and it's offered there at once. Such a link has
//!   no expiry, as aa-afat's: it stays open while it tracks, and closes when
//!   tracking stops. One keyed job polls every tracked fleet each minute
//!   while any is tracked; tracking stops when the fleet ends, the character
//!   isn't boss, ESI refuses, the data source goes, the link closes, or
//!   after six hours.

use chrono::{DateTime, Datelike, Duration, SecondsFormat, Utc};
use tether_plugin_sdk::doctrines;
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Action, Card, CardGrid, Column, Field, Form, Page, PageError, Plugin, Request, Section,
    SettingsForm, SettingsGroup, Stat, Submission, SubmitResult, Table, Tone, Value, action,
    actions, add_owner, alliance, badge, character, corporation, item_type, link, log, share, time,
};

/// The longest a link may stay open at once.
const MAX_EXPIRY_MINUTES: i64 = 24 * 60;
/// Manual FATs are added within this long of a link's creation (and before
/// it's reopened), as aa-afat.
const MANUAL_FAT_HOURS: i64 = 24;
/// FAT links per page of the FAT links list.
const LINKS_PER_PAGE: i64 = 100;
/// Characters offered on the register form (a form has at most 30 fields).
const MAX_FORM_CHARACTERS: usize = 30;
/// Attendees shown on a link's page, each with its Remove for managers
/// (beyond them, a manager removes a FAT by name).
const MAX_ATTENDEES: i64 = 500;
/// Rows in statistics tables, within the host's page limits.
const MAX_STAT_ROWS: usize = 300;
const MAX_FLEET: u32 = 100;
const MAX_DOCTRINE: u32 = 100;
const MAX_TYPE_NAME: u32 = 50;

/// The job that polls tracked fleets, queued under its own name as key so
/// there's only ever one.
const TRACK_JOB: &str = "track_fleets";
/// How often a tracked fleet is read (ESI caches fleet members for a few
/// seconds; a minute is plenty for attendance).
const TRACK_EVERY: Duration = Duration::seconds(60);
/// Tracking stops this long after it first started.
const TRACK_CAP: Duration = Duration::hours(6);
/// Fleets read per run: two host calls each at most (the fleet, and names
/// for new members), within the host's 100 ESI calls per job run.
const TRACKED_PER_RUN: i64 = 40;
/// Who the log says stopped tracking when the job did.
const TRACKER: &str = "ESI fleet tracking";

/// Names one `universe-ids` call takes (ESI's limit).
const MAX_NAMES: usize = 500;

mod snapshot;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct FleetActivityTracking;

impl Plugin for FleetActivityTracking {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let page = render_page(&request, &viewer)?;
        Ok(with_create(page, &request.path, &viewer))
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        remember_characters(&viewer);
        Ok(match submit_form(&submission, &viewer)? {
            SubmitResult::Page(page) => {
                SubmitResult::Page(with_create(page, &submission.request.path, &viewer))
            }
            other => other,
        })
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "housekeeping" => housekeeping(),
            "report_filters" => report_filters(),
            TRACK_JOB => track_fleets(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(FleetActivityTracking);

/// New FAT link as the header's button, beside the app's views (Tether
/// draws those from the manifest). The app adds it rather than the
/// manifest's `[action]`, which shows by one page rule: aa-afat lets
/// `add_fatlink` or `manage_afat` create. Not on the Manage pages.
fn with_create(page: Page, path: &str, viewer: &Viewer) -> Page {
    let manage = ["fleet-types", "settings", "logs"]
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")));
    if can_create(viewer) && !manage {
        page.button("New FAT link", "links/create")
    } else {
        page
    }
}

fn render_page(request: &Request, viewer: &Viewer) -> Result<Page, PageError> {
    let parts: Vec<&str> = request.path.split('/').collect();
    match parts.as_slice() {
        [""] => dashboard(viewer),
        ["links"] => links_page(viewer, 1),
        ["links", "page", n] => links_page(viewer, number(n)?),
        ["links", "create"] => create_page(viewer, None, owner_added(request)),
        ["links", hash] => details_page(viewer, hash, None),
        ["links", hash, "add"] => register_page(viewer, hash, None, false),
        ["stats"] => stats_page(viewer, this_year()),
        ["stats", year] => stats_page(viewer, year_of(year)?),
        ["stats", "corporation", id] => corporation_page(viewer, number(id)?, this_year()),
        ["stats", "corporation", id, year] => corporation_page(viewer, number(id)?, year_of(year)?),
        ["stats", "alliance", id] => alliance_page(viewer, number(id)?, this_year()),
        ["stats", "alliance", id, year] => alliance_page(viewer, number(id)?, year_of(year)?),
        ["stats", "character", id] => character_page(viewer, number(id)?, this_year()),
        ["stats", "character", id, year] => character_page(viewer, number(id)?, year_of(year)?),
        ["fleet-types"] => fleet_types_page(None),
        ["settings"] => settings_page(),
        ["logs"] => logs_page(viewer),
        _ => Err(PageError::NotFound),
    }
}

fn submit_form(submission: &Submission, viewer: &Viewer) -> Result<SubmitResult, PageError> {
    let path = submission.request.path.clone();
    let parts: Vec<&str> = path.split('/').collect();
    match (parts.as_slice(), submission.form.as_str()) {
        (["links", "create"], "create") => create_link(viewer, submission),
        (["links", hash, "add"], "register") => register(viewer, hash, submission),
        (["links", hash], form) => change_link(viewer, hash, form, submission),
        (["fleet-types"], form) => change_fleet_types(viewer, form, submission),
        (["settings"], "settings") => save_settings(viewer, submission),
        _ => Err(PageError::NotFound),
    }
}

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

/// Whether the character is one of the viewer's.
fn owns(viewer: &Viewer, character_id: i64) -> bool {
    viewer.characters.iter().any(|c| c.id == character_id)
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
    // Recent FATs recorded without an affiliation (from an ESI fleet, or a
    // manual FAT by id) take the character's corporation once it's known.
    // Only the last week's: today's corporation isn't the one of months ago.
    let ids = id_list(
        viewer
            .characters
            .iter()
            .filter(|c| c.corporation_id > 0)
            .map(|c| c.id),
    );
    match storage::execute(
        "UPDATE fats f SET corporation_id = c.corporation_id, alliance_id = c.alliance_id \
         FROM characters c WHERE c.character_id = f.character_id AND f.corporation_id IS NULL \
         AND f.created_at > now() - interval '7 days' \
         AND c.character_id = ANY(string_to_array($1, ',')::bigint[])",
        &[ids],
    ) {
        Ok(0) => {}
        Ok(_) => learn_names(
            &viewer
                .characters
                .iter()
                .flat_map(|c| [c.corporation_id, c.alliance_id.unwrap_or_default()])
                .collect::<Vec<_>>(),
        ),
        Err(err) => log::warn(format!("filling in affiliations: {err:?}")),
    }
}

/// Refused name look-ups one call to `learn_names` tolerates.
const NAME_REFUSALS: usize = 6;

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
    // ESI refuses a whole batch for one id it can't name (a deleted
    // character, say): halve a refused batch to name the rest, giving up
    // after a few refusals (each is an ESI error).
    let mut named = Vec::new();
    let mut todo: Vec<Vec<i64>> = vec![missing[..missing.len().min(1000)].to_vec()];
    let mut refused = 0;
    while let Some(chunk) = todo.pop() {
        match esi::names(&chunk) {
            Ok(n) => named.extend(n),
            Err(esi::Error::Status(404)) if chunk.len() > 1 && refused < NAME_REFUSALS => {
                refused += 1;
                let (a, b) = chunk.split_at(chunk.len() / 2);
                todo.push(a.to_vec());
                todo.push(b.to_vec());
            }
            Err(err) => {
                log::warn(format!("names of {} ids: {err:?}", chunk.len()));
                refused += 1;
                if refused >= NAME_REFUSALS {
                    break;
                }
            }
        }
    }
    if named.is_empty() {
        return;
    }
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

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

/// Characters' corporations and alliances now, from ESI's public
/// affiliation (a thousand at a time). Best effort: none on trouble.
fn affiliations(ids: &[i64]) -> Vec<(i64, i64, Option<i64>)> {
    let mut ids: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = Vec::new();
    for chunk in ids.chunks(1000) {
        let list = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        match esi::get(
            "character-affiliation",
            Subject::Character(0),
            &[("character_ids".to_owned(), list)],
            None,
        ) {
            Ok(answer) => {
                let list: serde_json::Value =
                    serde_json::from_str(&answer.body).unwrap_or_default();
                out.extend(list.as_array().into_iter().flatten().filter_map(|a| {
                    Some((
                        a["character_id"].as_i64()?,
                        a["corporation_id"].as_i64()?,
                        a["alliance_id"].as_i64(),
                    ))
                }));
            }
            Err(err) => log::warn(format!("affiliations: {err:?}")),
        }
    }
    out
}

/// Hourly: the last week's FATs recorded without a corporation (a pilot
/// the app hadn't seen) take the one ESI says now, and every corporation
/// and alliance on a FAT gets its name.
fn fill_in_affiliations() -> Result<(), JobError> {
    let unknown = storage::query(
        "SELECT DISTINCT character_id FROM fats \
         WHERE corporation_id IS NULL AND created_at > now() - interval '7 days' LIMIT 1000",
        &[],
    )
    .map_err(|e| retry("finding FATs without a corporation", e))?;
    let ids: Vec<i64> = unknown.rows.iter().map(|r| int(r, 0)).collect();
    let found = affiliations(&ids);
    if !found.is_empty() {
        let rows: Vec<serde_json::Value> = found
            .iter()
            .map(|(c, corp, alliance)| {
                serde_json::json!({ "character_id": c, "corporation_id": corp, "alliance_id": alliance })
            })
            .collect();
        storage::execute(
            "UPDATE fats f SET corporation_id = x.corporation_id, alliance_id = x.alliance_id \
             FROM json_to_recordset($1::json) AS x(character_id bigint, corporation_id bigint, alliance_id bigint) \
             WHERE f.character_id = x.character_id AND f.corporation_id IS NULL \
               AND f.created_at > now() - interval '7 days'",
            &[Db::json(serde_json::Value::Array(rows).to_string())],
        )
        .map_err(|e| retry("filling in corporations", e))?;
    }
    let unnamed = storage::query(
        "SELECT DISTINCT id FROM ( \
             SELECT corporation_id AS id FROM fats UNION SELECT alliance_id FROM fats) x \
         WHERE id IS NOT NULL AND id > 0 AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = x.id) \
         LIMIT 1000",
        &[],
    )
    .map_err(|e| retry("finding unnamed corporations", e))?;
    learn_names(&unnamed.rows.iter().map(|r| int(r, 0)).collect::<Vec<_>>());
    Ok(())
}

/// A name read for the app, or `unknown` in words (never an id) when it
/// hasn't been yet.
fn name_of(id: i64, unknown: &str) -> Result<String, PageError> {
    let rows = query("SELECT name FROM names WHERE id = $1", &[id.into()])?;
    Ok(rows
        .first()
        .map_or_else(|| unknown.to_owned(), |r| text(r, 0)))
}

// ---- links -----------------------------------------------------------------

struct LinkInfo {
    id: i64,
    hash: String,
    fleet: String,
    fleet_type: Option<String>,
    doctrine: Option<String>,
    creator_name: String,
    /// The FC's main when they created it.
    creator_id: i64,
    created_at: String,
    /// None for an ESI-tracked link while it tracks: it has no expiry.
    expires_at: Option<String>,
    reopened: i64,
    fats: i64,
    open: bool,
    /// Closed, never reopened, and within the reopen grace time.
    reopenable: bool,
    /// Within 24 hours of its creation and never reopened: manual FATs.
    manual: bool,
    /// Set for a link following an ESI fleet.
    esi: Option<Tracking>,
}

/// A link's ESI fleet tracking.
struct Tracking {
    character_id: i64,
    character_name: String,
    tracking: bool,
    stop_reason: Option<String>,
    polled_at: Option<String>,
    /// Still within the six-hour cap.
    within_cap: bool,
}

const LINK_COLUMNS: &str = "l.id, l.hash, l.fleet, l.fleet_type, l.doctrine, l.creator_account, \
     l.creator_name, l.created_at, l.expires_at, l.reopened, \
     (SELECT count(*) FROM fats f WHERE f.link_id = l.id)::bigint, \
     coalesce(l.expires_at > now(), l.esi_state = 'tracking'), \
     l.esi_state, l.esi_character_name, l.esi_stop_reason, l.esi_polled_at, \
     l.esi_started_at > now() - interval '6 hours', l.esi_character_id, l.creator_id, \
     l.reopened = 0 AND l.expires_at <= now() AND l.expires_at > now() - make_interval(mins => \
         coalesce((SELECT reopen_grace_minutes FROM settings WHERE id = 1), 60)), \
     l.reopened = 0 AND l.created_at > now() - interval '24 hours'"; // TRACK_CAP, MANUAL_FAT_HOURS

fn link_info(row: &[Db]) -> LinkInfo {
    LinkInfo {
        id: int(row, 0),
        hash: text(row, 1),
        fleet: text(row, 2),
        fleet_type: maybe_text(row, 3),
        doctrine: maybe_text(row, 4),
        creator_name: text(row, 6),
        creator_id: int(row, 18),
        created_at: text(row, 7),
        expires_at: maybe_text(row, 8),
        reopened: int(row, 9),
        fats: int(row, 10),
        open: flag(row, 11),
        reopenable: flag(row, 19),
        manual: flag(row, 20),
        esi: maybe_text(row, 12).map(|state| Tracking {
            character_id: int(row, 17),
            character_name: text(row, 13),
            tracking: state == "tracking",
            stop_reason: maybe_text(row, 14),
            polled_at: maybe_text(row, 15),
            within_cap: flag(row, 16),
        }),
    }
}

/// Why tracking stopped, for people.
fn stop_text(reason: &str, character: &str) -> String {
    match reason {
        "fleet_ended" => format!("{character} isn't in a fleet: the fleet ended or they left it."),
        "not_boss" => format!(
            "{character} isn't the fleet boss. Pass boss back to them, then resume tracking."
        ),
        "refused" => "ESI refused to show the fleet (403).".to_owned(),
        "data_source" => format!(
            "{character} is no longer a data source of this app (withdrawn, removed, or moved corporation). Log in with the fleet boss again on New FAT link."
        ),
        "token" => format!("{character}'s login has expired: they need to log in again."),
        "cap" => "Tracking stopped after six hours.".to_owned(),
        "manual" => "Tracking was stopped by hand.".to_owned(),
        "closed" => "The FAT link was closed.".to_owned(),
        other => other.to_owned(),
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

/// When a link closes or closed: a time, or "when the fleet ends" for an
/// ESI-tracked link, which has no expiry.
fn closes(link: &LinkInfo) -> (&'static str, Value) {
    match &link.expires_at {
        Some(at) => (
            if link.open { "Closes" } else { "Closed" },
            time(at.clone()),
        ),
        None => ("Closes", "When the fleet ends".into()),
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
                character(l.creator_id, l.creator_name.clone()).into(),
                time(l.created_at.clone()),
                l.fats.into(),
                status(l),
            ]
        }),
    )
}

/// A character, corporation, alliance or type: its picture and name, or
/// the id until the name is known, or nothing for none.
fn named(
    make: fn(i64, String) -> tether_plugin_sdk::Entity,
    name: String,
    id: i64,
    what: &str,
) -> Value {
    match (name, id) {
        (_, 0) => "".into(),
        (name, id) if !name.is_empty() => make(id, name).into(),
        (_, id) => make(id, format!("{what} {id}")).into(),
    }
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
        "SELECT f.character_name, l.fleet, coalesce(l.fleet_type, ''), l.created_at, f.character_id \
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
                "SELECT {LINK_COLUMNS} FROM links l \
                 WHERE coalesce(l.expires_at > now(), l.esi_state = 'tracking') \
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
                character(int(r, 4), text(r, 0)).into(),
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
    Ok(page)
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
    let mut page = Page::new("FAT links")
        .description("Every FAT link, newest first. FCs share a link's register page in fleet.")
        .stats(vec![Stat::new("FAT links", total)])
        .table(links_table(
            viewer,
            // Which page, only when there's more than one.
            &if pages > 1 {
                format!("Page {page_number} of {pages}")
            } else {
                "Newest first".to_owned()
            },
            "No FAT links yet.",
            &links,
        ));
    // Paging; the views and New FAT link are in the header.
    let mut more = Card::new("Pages");
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

/// The character Add data source just added, from the query Tether brings the
/// FC back with (`owner`). Anyone can type a query, so it's only a hint:
/// `create_page` uses it only if it's one of the viewer's own characters
/// that is a data source, and `create_link` checks the choice again.
fn owner_added(request: &Request) -> Option<i64> {
    request
        .query
        .iter()
        .find(|(name, _)| name == "owner")
        .and_then(|(_, id)| id.parse().ok())
}

/// The viewer's own characters that are data sources of this app: the
/// ones whose ESI fleet they may track.
fn trackable_characters(viewer: &Viewer) -> Vec<(i64, String)> {
    let sources = esi::data_sources();
    viewer
        .characters
        .iter()
        .filter(|c| sources.iter().any(|s| s.id == c.id))
        .map(|c| (c.id, c.name.clone()))
        .collect()
}

/// Says which link already tracks a character's fleet (one at a time).
fn already_tracked(character_id: i64) -> Result<String, PageError> {
    let rows = query(
        "SELECT fleet, esi_character_name FROM links \
         WHERE esi_character_id = $1 AND esi_state = 'tracking'",
        &[character_id.into()],
    )?;
    Ok(match rows.first() {
        Some(r) => format!(
            "{}'s fleet is already tracked by the FAT link \"{}\". Stop that tracking (or close \
             that link) first: a character tracks one fleet at a time.",
            text(r, 1),
            text(r, 0)
        ),
        None => "That character's fleet is already tracked by another FAT link.".to_owned(),
    })
}

/// New FAT link, aa-afat's clickable or ESI-tracked link. `added` is
/// the fleet boss Add data source just logged in with, chosen for tracking.
fn create_page(viewer: &Viewer, note: Option<&str>, added: Option<i64>) -> Result<Page, PageError> {
    if !can_create(viewer) {
        return Err(PageError::Forbidden);
    }
    let mut page = Page::new("New FAT link").description(
        "Members open the link's register page while it's open to record their attendance, \
         or Tether tracks your ESI fleet and gives everyone in it a FAT.",
    );
    if let Some(note) = note {
        page = page.text(note);
    }
    let settings = settings()?;
    let expiry = settings.expiry_minutes;
    let mut form = Form::new("create", "Create FAT link")
        .field(Field::text("fleet", "Fleet name", MAX_FLEET).required())
        .field(
            Field::select("fleet_type", "Fleet type", type_options(None)?)
                .help("Managers keep the list of fleet types."),
        )
        .field(doctrine_field(settings.doctrines_from_fittings, None))
        .field(
            Field::number("expiry", "Open for (minutes)")
                .range(Some(1.0), Some(MAX_EXPIRY_MINUTES as f64), true)
                .value(expiry.to_string())
                .help("For a link members click: after this, nobody can register. It can be closed sooner, or reopened once soon after. A link tracking your ESI fleet has no expiry.")
                .required(),
        );
    let trackable = trackable_characters(viewer);
    let first_login = trackable.is_empty();
    // aa-afat's ESI FAT link: log in with the fleet boss (once; it's then
    // offered here every time), and the link tracks its fleet.
    let added = added.filter(|id| trackable.iter().any(|(c, _)| c == id));
    let login = Card::new("Track your ESI fleet")
        .description(if trackable.is_empty() {
            "Log in with your own character that is (or will be) fleet boss: EVE asks you to \
             allow Tether to read its fleet, and it's offered below for tracking. Only your own: \
             logging in with someone else's character moves it to your account."
        } else {
            "Boss the fleet with another of your own characters? Log in with it: it joins the \
             characters offered below. Someone else's character would move to your account."
        })
        .field("Fleet boss", add_owner("Log in with the fleet boss"));
    if let Some(id) = added {
        let name = trackable
            .iter()
            .find(|(c, _)| *c == id)
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        page = page.text(format!(
            "{name} can be tracked: name the fleet and create the link to start."
        ));
    }
    if !trackable.is_empty() {
        let mut options = vec![(
            String::new(),
            "Don't track: members click the link".to_owned(),
        )];
        options.extend(
            trackable
                .into_iter()
                .map(|(id, name)| (id.to_string(), format!("Track {name}'s fleet"))),
        );
        let mut track = Field::select("track", "ESI fleet", options).help(
            "Every minute (up to six hours), everyone in the fleet that character is boss of gets \
             a FAT, with ship and system. The link has no expiry: it stays open until the fleet \
             ends, and closes when tracking stops.",
        );
        if let Some(id) = added {
            track = track.value(id.to_string());
        }
        form = form.field(track);
    }
    // Before the form while there's nobody to track (log in first, then
    // fill it in); after it once there is.
    Ok(if first_login {
        page.card(login).form(form)
    } else {
        page.form(form).card(login)
    })
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
            None,
        )?));
    };
    let fleet_type = optional(submission.value("fleet_type"));
    let doctrine = optional(submission.value("doctrine"));
    if !doctrine_offered(
        settings()?.doctrines_from_fittings,
        doctrine.as_deref(),
        None,
    ) {
        return Ok(SubmitResult::Page(create_page(
            viewer,
            Some("Choose one of the doctrines Fittings shares with you."),
            None,
        )?));
    }
    // Only the viewer's own characters that are data sources.
    let track = match submission.value("track") {
        "" => None,
        id => Some(
            trackable_characters(viewer)
                .into_iter()
                .find(|(c, _)| c.to_string() == id)
                .ok_or(PageError::Forbidden)?,
        ),
    };
    let tracking = track.is_some();
    // As aa-afat's ESI FAT link, a tracked one has no expiry: open until
    // the fleet ends.
    let (expires, open_for) = match &track {
        Some((_, name)) => (
            Db::Null,
            format!("open until the fleet ends, tracking {name}'s ESI fleet"),
        ),
        None => {
            let expiry = minutes(submission, "expiry")?;
            (
                Db::timestamp(rfc3339(Utc::now() + Duration::minutes(expiry))),
                format!("open for {expiry} minutes"),
            )
        }
    };
    let description = format!(
        "FAT link for \"{fleet}\" ({}), {open_for}",
        fleet_type.as_deref().unwrap_or("no fleet type"),
    );
    // The link and its log entry in one statement.
    let track_id = track.as_ref().map(|(id, _)| *id);
    let rows = storage::query(
        "WITH created AS ( \
             INSERT INTO links (hash, fleet, fleet_type, doctrine, creator_account, creator_id, creator_name, \
                                expires_at, esi_character_id, esi_character_name, esi_state, esi_started_at) \
             VALUES (replace(gen_random_uuid()::text, '-', ''), $1, $2, $3, $4, $5, $6, $7, $9, $10, \
                     CASE WHEN $9::bigint IS NULL THEN NULL ELSE 'tracking' END, \
                     CASE WHEN $9::bigint IS NULL THEN NULL ELSE now() END) RETURNING hash) \
         INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
         SELECT 'Create FAT Link', $5, $6, hash, $8 FROM created RETURNING link_hash",
        &[
            fleet.into(),
            fleet_type.into(),
            doctrine.into(),
            viewer.account_id.into(),
            viewer.main.id.into(),
            viewer.main.name.clone().into(),
            expires,
            description.into(),
            track_id.into(),
            track.map(|(_, name)| name).into(),
        ],
    );
    let rows = match rows {
        Ok(rows) => rows.rows,
        // That character already tracks a fleet on another link.
        Err(storage::Error::Database(e)) if e.code == "23505" && track_id.is_some() => {
            let note = already_tracked(track_id.unwrap_or_default())?;
            return Ok(SubmitResult::Page(create_page(viewer, Some(&note), None)?));
        }
        Err(e) => return Err(failed("creating the link", e)),
    };
    let hash = rows
        .first()
        .map(|r| text(r, 0))
        .ok_or_else(|| PageError::Failed("the new link has no hash".into()))?;
    if tracking {
        poll_now()?;
    }
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
                date_trunc('minute', f.created_at), f.added_by, f.corporation_id, f.alliance_id, \
                coalesce(s.name, ''), coalesce(y.name, ''), f.esi, f.ship_type_id, f.system_id \
         FROM fats f LEFT JOIN names c ON c.id = f.corporation_id LEFT JOIN names a ON a.id = f.alliance_id \
         LEFT JOIN names s ON s.id = f.ship_type_id LEFT JOIN names y ON y.id = f.system_id \
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
    let (closes_label, closes_at) = closes(&link);
    let mut stats = vec![
        Stat::new("Status", status_stat),
        Stat::new("FATs", link.fats),
        Stat::new(closes_label, closes_at),
    ];
    if let Some(esi) = &link.esi {
        stats.push(Stat::new(
            "ESI fleet",
            match (esi.tracking, esi.stop_reason.as_deref()) {
                (true, _) => badge("Tracking", Tone::Success),
                (false, Some("not_boss")) => badge("Not boss", Tone::Warning),
                (false, _) => badge("Stopped", Tone::Neutral),
            },
        ));
    }
    page = page.stats(stats);
    if let Some(esi) = &link.esi
        && !esi.tracking
    {
        page = page.text(format!(
            "ESI tracking stopped: {}",
            stop_text(
                esi.stop_reason.as_deref().unwrap_or_default(),
                &esi.character_name
            )
        ));
    }
    let manage = viewer.can("manage_afat");
    let settings = settings()?;
    let register = format!("links/{}/add", link.hash);
    let mut card = Card::new("FAT link");
    // aa-afat's "Copy FAT link to clipboard", while members can register.
    if link.open {
        card = card.field("Link to share", share(register.clone()));
    }
    card = card
        .field(
            "Register link",
            tether_plugin_sdk::link("Register attendance", register),
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
        .field(
            "Created by",
            character(link.creator_id, link.creator_name.clone()),
        )
        .field("Created", time(link.created_at.clone()));
    if link.reopened > 0 {
        card = card.field("Reopened", link.reopened);
    }
    if let Some(esi) = &link.esi {
        card = card.field(
            "ESI fleet",
            if esi.tracking {
                format!("Tracking {}'s fleet every minute", esi.character_name)
            } else {
                format!("{}'s fleet, not tracked now", esi.character_name)
            },
        );
        if let Some(at) = &esi.polled_at {
            card = card.field("Fleet last read", time(at.clone()));
        }
    }
    let buttons = link_actions(viewer, &link, &settings);
    if !buttons.is_empty() {
        card = card.field("Actions", actions(buttons));
    }
    let mut columns = vec![
        Column::text("Character"),
        Column::text("Corporation"),
        Column::text("Alliance"),
        Column::text("Ship"),
        Column::text("System"),
    ];
    columns.extend([Column::numeric("Registered"), Column::text("How")]);
    // Managers remove a FAT from its row.
    if manage {
        columns.push(Column::text(""));
    }
    page = page.card(card).table(with_rows(
        Table::new(columns)
            .title("Attendees")
            .empty("Nobody has registered yet."),
        attendees.iter().map(|r| {
            // Names from ESI, or the id until they're known.
            let corporation_cell = match (text(r, 2), int(r, 6)) {
                (_, 0) => "Unknown".into(),
                (name, id) => named(corporation, name, id, "Corporation"),
            };
            let mut row: Vec<Value> = vec![
                character(int(r, 0), text(r, 1)).into(),
                corporation_cell,
                named(alliance, text(r, 3), int(r, 7), "Alliance"),
            ];
            row.push(named(item_type, text(r, 8), int(r, 11), "Type"));
            row.push(match (text(r, 9), int(r, 12)) {
                (name, _) if !name.is_empty() => name.into(),
                (_, 0) => "".into(),
                (_, id) => format!("System {id}").into(),
            });
            row.push(time(text(r, 4)));
            row.push(match (maybe_text(r, 5), flag(r, 10)) {
                (Some(by), _) => format!("Added by {by}").into(),
                (None, true) => "ESI fleet".into(),
                (None, false) => "Registered".into(),
            });
            if manage {
                row.push(
                    action("Remove", "remove_fat")
                        .field("character_id", int(r, 0).to_string())
                        .tone(Tone::Danger)
                        .confirm(format!(
                            "{}'s FAT for this fleet is removed; statistics lose it.",
                            text(r, 1)
                        ))
                        .into(),
                );
            }
            row
        }),
    ));
    if link.fats > MAX_ATTENDEES {
        page = page.text(format!(
            "Showing the first {MAX_ATTENDEES} of {} attendees; statistics count them all.",
            link.fats
        ));
    }
    if !link.open && !link.reopenable {
        page = page.text(if link.reopened > 0 {
            "This link was reopened once already, and can't be reopened again.".to_owned()
        } else {
            format!(
                "A link can be reopened only within {} minutes of closing.",
                settings.reopen_grace_minutes
            )
        });
    }
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
                .field(doctrine_field(
                    settings.doctrines_from_fittings,
                    link.doctrine.as_deref(),
                )),
        )],
    );
    // aa-afat's manual FATs: within 24 hours, and before a reopen.
    if link.manual {
        page = page.tab(
            "Add FAT",
            vec![Section::Form(
                Form::new("add_fat", "Add FAT")
                    .description(format!(
                        "For a pilot who was in fleet but didn't register, within {MANUAL_FAT_HOURS} \
                         hours of the link's creation and before it's reopened."
                    ))
                    .field(
                        Field::text("character", "Character name or ID", 40)
                            .help("A character that has used Fleet Activity Tracking, by exact name, or any character by its ID.")
                            .required(),
                    ),
            )],
        );
        // aa-afat's fleet snapshot, under the same rules.
        page = page.tab(
            "Fleet snapshot",
            vec![Section::Form(
                Form::new("snapshot", "Add fleet snapshot")
                    .description(
                        "Copy the fleet composition from your fleet window in EVE and paste it \
                         here: everyone in it gets a FAT, with ship and system. This is logged.",
                    )
                    .field(
                        Field::textarea("composition", "Fleet composition", SNAPSHOT_LENGTH)
                            .help("One pilot a line, as the fleet window copies them.")
                            .required(),
                    ),
            )],
        );
    } else {
        page = page.text(format!(
            "FATs can be added by hand only within {MANUAL_FAT_HOURS} hours of the link's \
             creation and before it's reopened."
        ));
    }
    // Attendees beyond those listed (each with its Remove) by name.
    if manage && link.fats > MAX_ATTENDEES {
        page = page.tab(
            "Remove FAT",
            vec![Section::Form(
                Form::new("remove_by_name", "Remove FAT")
                    .field(Field::text("character_name", "Character name", 40).required()),
            )],
        );
    }
    Ok(page)
}

/// A link's buttons for its FC and managers, as aa-afat's: stop or resume
/// ESI tracking, close or reopen it, delete it. Each posts to `change_link`.
fn link_actions(viewer: &Viewer, link: &LinkInfo, settings: &Settings) -> Vec<Action> {
    let manage = viewer.can("manage_afat");
    let mut buttons = Vec::new();
    if let Some(esi) = &link.esi
        && esi.tracking
    {
        let link_then = if link.expires_at.is_some() {
            "the link stays open for members to click"
        } else {
            "the link closes with it"
        };
        buttons.push(action("Stop tracking", "stop_tracking").confirm(format!(
            "Tether stops reading {}'s fleet; {link_then}. Only {} can resume it.",
            esi.character_name, esi.character_name
        )));
    }
    // Only the owner of the tracked character resumes it.
    if let Some(esi) = &link.esi
        && !esi.tracking
        && (link.open || esi.stop_reason.as_deref() != Some("closed"))
        && esi.within_cap
        && owns(viewer, esi.character_id)
    {
        buttons.push(action("Resume tracking", "resume").confirm(format!(
            "Tether reads {}'s fleet again every minute. They must be the fleet boss.",
            esi.character_name
        )));
    }
    if link.open {
        buttons.push(action("Close", "close").confirm("Nobody can register once it's closed."));
    } else if link.reopenable {
        buttons.push(action("Reopen", "reopen").confirm(format!(
            "Members can register again for {} minutes. A link is reopened once only.",
            settings.reopen_duration_minutes
        )));
    }
    if manage {
        buttons.push(
            action("Delete", "delete")
                .tone(Tone::Danger)
                .confirm(format!(
                    "\"{}\" and its {} FATs are deleted; statistics lose them. This can't be undone.",
                    link.fleet, link.fats
                )),
        );
    }
    buttons
}

fn change_link(
    viewer: &Viewer,
    hash: &str,
    form: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let link = load_link(hash)?;
    if !can_create(viewer) {
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
            if !doctrine_offered(
                settings()?.doctrines_from_fittings,
                doctrine.as_deref(),
                link.doctrine.as_deref(),
            ) {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some("Choose one of the doctrines Fittings shares with you."),
                )?));
            }
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
                        "UPDATE links SET expires_at = now() \
                         WHERE id = $1 AND (expires_at IS NULL OR expires_at > now())",
                        vec![link.id.into()],
                    ),
                    // Closed by hand: tracking that had stopped resumes
                    // only once the link is reopened.
                    Statement::new(
                        "UPDATE links SET esi_stop_reason = 'closed' \
                         WHERE id = $1 AND esi_state = 'stopped'",
                        vec![link.id.into()],
                    ),
                    log_entry(
                        viewer,
                        "Close FAT Link",
                        Some(&link.hash),
                        format!("\"{}\" closed early", link.fleet),
                    ),
                    // Its ESI tracking stops with it.
                    stop_statement(
                        link.id,
                        "closed",
                        viewer.main.id,
                        &viewer.main.name,
                        &format!("\"{}\": {}", link.fleet, stop_text("closed", "")),
                    ),
                ],
                "closing the link",
            )?;
            Ok(back())
        }
        "resume" => {
            // Only the owner of the tracked character restarts it: a
            // manager can stop anyone's tracking, never start it.
            let Some(esi) = link.esi.as_ref().filter(|e| owns(viewer, e.character_id)) else {
                return Err(PageError::Forbidden);
            };
            // Only a stopped link that's open, or that stopping closed (not
            // closed by hand), within six hours of first tracking, and not
            // read in the last minute. A closed one opens again without
            // expiry, as a new tracked link.
            let resumed = storage::execute(
                "WITH resumed AS ( \
                     UPDATE links SET esi_state = 'tracking', esi_stop_reason = NULL, \
                            expires_at = CASE WHEN expires_at > now() THEN expires_at END \
                     WHERE id = $1 AND esi_state = 'stopped' \
                       AND (expires_at > now() OR esi_stop_reason <> 'closed') \
                       AND esi_started_at > $5 AND (esi_polled_at IS NULL OR esi_polled_at < $6) \
                     RETURNING hash) \
                 INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
                 SELECT 'Resume ESI Fleet Tracking', $2, $3, hash, $4 FROM resumed",
                &[
                    link.id.into(),
                    viewer.main.id.into(),
                    viewer.main.name.clone().into(),
                    format!("\"{}\": tracking resumed", link.fleet).into(),
                    Db::timestamp(rfc3339(Utc::now() - TRACK_CAP)),
                    Db::timestamp(rfc3339(Utc::now() - TRACK_EVERY)),
                ],
            );
            let resumed = match resumed {
                Ok(n) => n,
                Err(storage::Error::Database(e)) if e.code == "23505" => {
                    let note = already_tracked(esi.character_id)?;
                    return Ok(SubmitResult::Page(details_page(viewer, hash, Some(&note))?));
                }
                Err(e) => return Err(failed("resuming tracking", e)),
            };
            if resumed == 0 {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some(
                        "Tracking can't be resumed now: it's running, the link was closed (reopen it \
                         first), six hours have passed, or the fleet was read less than a minute ago \
                         (try again shortly).",
                    ),
                )?));
            }
            poll_now()?;
            Ok(back())
        }
        "stop_tracking" => {
            run(
                &[stop_statement(
                    link.id,
                    "manual",
                    viewer.main.id,
                    &viewer.main.name,
                    &format!("\"{}\": {}", link.fleet, stop_text("manual", "")),
                )],
                "stopping tracking",
            )?;
            Ok(back())
        }
        "reopen" => {
            if link.open {
                return Ok(back());
            }
            let settings = settings()?;
            // aa-afat's rule, checked again as it's changed (so two posts
            // at once can't both reopen it): once, within the grace time,
            // for the reopen duration. Logged in the same statement.
            let reopened = storage::execute(
                "WITH reopened AS ( \
                     UPDATE links SET expires_at = now() + make_interval(mins => $3::int), \
                            reopened = reopened + 1 \
                     WHERE id = $1 AND reopened = 0 AND expires_at <= now() \
                       AND expires_at > now() - make_interval(mins => $2::int) RETURNING hash) \
                 INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
                 SELECT 'Reopen FAT Link', $4, $5, hash, $6 FROM reopened",
                &[
                    link.id.into(),
                    settings.reopen_grace_minutes.into(),
                    settings.reopen_duration_minutes.into(),
                    viewer.main.id.into(),
                    viewer.main.name.clone().into(),
                    format!(
                        "\"{}\" reopened for {} minutes",
                        link.fleet, settings.reopen_duration_minutes
                    )
                    .into(),
                ],
            )
            .map_err(|e| failed("reopening the link", e))?;
            if reopened == 0 {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some(
                        "The link can't be reopened: it's open, was reopened once already, or \
                         closed too long ago.",
                    ),
                )?));
            }
            Ok(back())
        }
        "add_fat" => add_fat(viewer, &link, submission.value("character").trim()),
        "snapshot" => fleet_snapshot(viewer, &link, submission.value("composition")),
        "remove_fat" | "remove_by_name" if manage => {
            // The next read of the fleet would add it straight back.
            if link.esi.as_ref().is_some_and(|e| e.tracking) {
                return Ok(SubmitResult::Page(details_page(
                    viewer,
                    hash,
                    Some(
                        "This link is tracking an ESI fleet, which would add the FAT back within a \
                         minute. Stop ESI tracking first, then remove it.",
                    ),
                )?));
            }
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
        "remove_fat" | "remove_by_name" | "delete" => Err(PageError::Forbidden),
        _ => Err(PageError::NotFound),
    }
}

/// A manual FAT: by the exact name of a character the app has seen, or by
/// any character's id.
fn add_fat(viewer: &Viewer, link: &LinkInfo, who: &str) -> Result<SubmitResult, PageError> {
    // The form is drawn only while it's allowed; a post from an old page
    // is told why.
    if !link.manual {
        return Ok(SubmitResult::Page(details_page(
            viewer,
            &link.hash,
            Some(&format!(
                "FATs can be added by hand only within {MANUAL_FAT_HOURS} hours of the link's \
                 creation and before it's reopened."
            )),
        )?));
    }
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
    // The FAT and its log entry together, only if it wasn't there yet and
    // the link still takes manual FATs.
    let added = storage::execute(
        "WITH added AS ( \
             INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id, added_by) \
             SELECT $1, $2, $3, $4, $5, $6 FROM links \
             WHERE id = $1 AND reopened = 0 AND created_at > now() - interval '24 hours' \
             ON CONFLICT (link_id, character_id) DO NOTHING RETURNING 1) \
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

/// The longest fleet composition taken: a full fleet's lines, generously.
const SNAPSHOT_LENGTH: u32 = 100_000;

/// What `universe-ids` found: each name, lowercased, to its id and its name
/// as EVE writes it, by kind.
#[derive(Default)]
struct Found {
    characters: std::collections::BTreeMap<String, (i64, String)>,
    systems: std::collections::BTreeMap<String, (i64, String)>,
    ships: std::collections::BTreeMap<String, (i64, String)>,
}

/// Exact names to ids with ESI's public `/universe/ids`, 500 a call.
fn universe_ids(names: &[String]) -> Result<Found, esi::Error> {
    let mut found = Found::default();
    for chunk in names.chunks(MAX_NAMES) {
        let answer = esi::get(
            "universe-ids",
            Subject::Character(0),
            &[("names".to_owned(), chunk.join("\n"))],
            None,
        )?;
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_default();
        for (key, into) in [
            ("characters", &mut found.characters),
            ("systems", &mut found.systems),
            ("inventory_types", &mut found.ships),
        ] {
            for item in body[key].as_array().into_iter().flatten() {
                if let (Some(id), Some(name)) = (item["id"].as_i64(), item["name"].as_str())
                    && id > 0
                {
                    into.insert(name.to_lowercase(), (id, name.to_owned()));
                }
            }
        }
    }
    Ok(found)
}

/// aa-afat's fleet snapshot: a FAT for every pilot in a pasted fleet
/// composition, with ship and system, under the manual FAT rules (within
/// 24 hours of the link's creation, before it's reopened). Pilots EVE
/// doesn't know are left out and named; one already on the link is left
/// alone.
fn fleet_snapshot(
    viewer: &Viewer,
    link: &LinkInfo,
    composition: &str,
) -> Result<SubmitResult, PageError> {
    let back = |note: &str| -> Result<SubmitResult, PageError> {
        Ok(SubmitResult::Page(details_page(
            viewer,
            &link.hash,
            Some(note),
        )?))
    };
    if !link.manual {
        return back(&format!(
            "FATs can be added by hand only within {MANUAL_FAT_HOURS} hours of the link's \
             creation and before it's reopened."
        ));
    }
    let members = match snapshot::parse(composition) {
        Ok(members) => members,
        Err(why) => return back(&why),
    };
    let mut names: Vec<String> = Vec::new();
    for member in &members {
        for name in [&member.name, &member.system, &member.ship] {
            if !name.is_empty()
                && name.chars().count() <= 100
                && !names.iter().any(|n| n.eq_ignore_ascii_case(name))
            {
                names.push(name.clone());
            }
        }
    }
    let found = match universe_ids(&names) {
        Ok(found) => found,
        Err(err) => {
            log::warn(format!("universe-ids for a fleet snapshot: {err:?}"));
            return back("ESI couldn't look up the pilots just now. Try again shortly.");
        }
    };
    let mut rows = Vec::new();
    let mut unknown = Vec::new();
    for member in &members {
        let Some((id, name)) = found.characters.get(&member.name.to_lowercase()) else {
            unknown.push(member.name.clone());
            continue;
        };
        let id_of = |map: &std::collections::BTreeMap<String, (i64, String)>, name: &str| {
            map.get(&name.to_lowercase()).map(|(id, _)| *id)
        };
        rows.push((
            *id,
            name.clone(),
            id_of(&found.systems, &member.system),
            id_of(&found.ships, &member.ship),
        ));
    }
    // Who each flies for now (ESI's public affiliation), else what the app
    // last saw.
    let affiliated = affiliations(&rows.iter().map(|r| r.0).collect::<Vec<_>>());
    let json: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, name, system, ship)| {
            let (corporation, alliance) = affiliated
                .iter()
                .find(|a| a.0 == *id)
                .map_or((None, None), |a| (Some(a.1), a.2));
            serde_json::json!({
                "character_id": id, "character_name": name, "system_id": system,
                "ship_type_id": ship, "corporation_id": corporation, "alliance_id": alliance,
            })
        })
        .collect();
    // Names for the attendees table: the pilots, systems and ships just
    // looked up.
    let named: Vec<serde_json::Value> = [
        (&found.characters, "character"),
        (&found.systems, "solar_system"),
        (&found.ships, "inventory_type"),
    ]
    .into_iter()
    .flat_map(|(map, category)| {
        map.values().map(
            move |(id, name)| serde_json::json!({ "id": id, "name": name, "category": category }),
        )
    })
    .collect();
    if let Err(err) = storage::execute(
        "INSERT INTO names (id, name, category) \
         SELECT id, name, category FROM json_to_recordset($1::json) AS x(id bigint, name text, category text) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
        &[Db::json(serde_json::Value::Array(named).to_string())],
    ) {
        log::warn(format!("storing names: {err:?}"));
    }
    // The FATs and their log entry together, only while the link takes
    // manual FATs; pilots already on it are left alone.
    let added = query(
        "WITH added AS ( \
             INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id, \
                               system_id, ship_type_id, added_by) \
             SELECT l.id, x.character_id, x.character_name, \
                    coalesce(x.corporation_id, c.corporation_id), \
                    CASE WHEN x.corporation_id IS NOT NULL THEN x.alliance_id ELSE c.alliance_id END, \
                    x.system_id, x.ship_type_id, $3 \
             FROM json_to_recordset($1::json) AS x(character_id bigint, character_name text, \
                  system_id bigint, ship_type_id bigint, corporation_id bigint, alliance_id bigint) \
             JOIN links l ON l.id = $2 AND l.reopened = 0 \
                  AND l.created_at > now() - interval '24 hours' \
             LEFT JOIN characters c ON c.character_id = x.character_id \
             ON CONFLICT (link_id, character_id) DO NOTHING \
             RETURNING corporation_id, alliance_id), \
         logged AS ( \
             INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
             SELECT 'Fleet Snapshot', $4, $3, $5, \
                    'Fleet snapshot added ' || n || ' FATs to \"' || $6 || '\"' \
             FROM (SELECT count(*) AS n FROM added) x WHERE n > 0) \
         SELECT corporation_id, alliance_id FROM added",
        &[
            Db::json(serde_json::Value::Array(json).to_string()),
            link.id.into(),
            viewer.main.name.clone().into(),
            viewer.main.id.into(),
            link.hash.clone().into(),
            link.fleet.clone().into(),
        ],
    )?;
    learn_names(
        &added
            .iter()
            .flat_map(|r| [int(r, 0), int(r, 1)])
            .collect::<Vec<_>>(),
    );
    let already = rows.len().saturating_sub(added.len());
    let mut note = format!(
        "Fleet snapshot: {} added.",
        match added.len() {
            1 => "1 FAT".to_owned(),
            n => format!("{n} FATs"),
        }
    );
    if already > 0 {
        note.push_str(&format!(" {already} already had one."));
    }
    if !unknown.is_empty() {
        note.push_str(&format!(
            " EVE knows no pilot called {}.",
            unknown.join(", ")
        ));
    }
    back(&note)
}

/// The page members open from the FC's link. `unregistered`: a character
/// they ticked isn't registered for the app, so Tether's Register
/// Character card is shown.
fn register_page(
    viewer: &Viewer,
    hash: &str,
    note: Option<&str>,
    unregistered: bool,
) -> Result<Page, PageError> {
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
            .field(closes(&link).0, closes(&link).1),
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
    let mut ready: Vec<_> = viewer
        .characters
        .iter()
        .filter(|c| !done.contains(&c.id))
        .collect();
    if ready.is_empty() {
        return Ok(page.description("All your characters are registered for this fleet."));
    }
    page = page.description(
        "Register your attendance while the link is open. Each character must be online in EVE: \
         Tether asks ESI, and records its system and ship.",
    );
    // As aa-afat's token_required: a FAT needs the character registered
    // for this app (its location scopes). Registering is Tether's page.
    if unregistered {
        page = page
            .text(
                "Register your characters for Fleet Activity Tracking (EVE asks to let Tether see \
                 whether they're online, where they are and what they fly), then open this link \
                 again.",
            )
            .cards(CardGrid::new().register());
    }
    // The character they act as (their main unless they chose another)
    // first and ticked, then by name.
    let acting = identity::acting().map_or(viewer.main.id, |c| c.id);
    ready.sort_by_key(|c| (c.id != acting, c.name.to_lowercase()));
    let mut form = Form::new("register", "Register")
        .title("Your characters in this fleet")
        .description("Tick every character you brought. Each must be logged in to EVE.");
    for character in ready.iter().take(MAX_FORM_CHARACTERS) {
        form = form.field(Field::checkbox(
            format!("c_{}", character.id),
            character.name.clone(),
            character.id == acting || ready.len() == 1,
        ));
    }
    Ok(page.form(form))
}

/// Where a character is, as aa-afat checks before a FAT.
enum Presence {
    /// Online, in this system and ship (each if ESI said).
    Online {
        system: Option<i64>,
        ship: Option<i64>,
    },
    Offline,
    /// Not registered for this app (no token with its scopes).
    NotRegistered,
    /// Its login expired or was revoked.
    Token,
    /// ESI or Tether trouble; try again.
    Trouble,
}

/// A character endpoint's JSON, or why not. After ESI or Tether trouble
/// (`troubled`), no more calls in this submission: every ESI error counts
/// against the app's error allowance, which ESI fleet tracking shares.
fn character_json(
    endpoint: &str,
    id: i64,
    troubled: &mut bool,
) -> Result<serde_json::Value, Presence> {
    if *troubled {
        return Err(Presence::Trouble);
    }
    match esi::get(endpoint, Subject::Character(id), &[], None) {
        Ok(response) => Ok(serde_json::from_str(&response.body).unwrap_or_default()),
        Err(esi::Error::NotRegistered | esi::Error::NotAllowed(_)) => Err(Presence::NotRegistered),
        Err(esi::Error::Token) => Err(Presence::Token),
        Err(err) => {
            log::warn(format!("{endpoint} for a FAT: {err:?}"));
            *troubled = true;
            Err(Presence::Trouble)
        }
    }
}

/// aa-afat's add_fat: the character must be online; its system and ship
/// are recorded (if ESI can't say, the FAT goes without them).
fn presence(id: i64, troubled: &mut bool) -> Presence {
    let online = match character_json("character-online", id, troubled) {
        Ok(json) => json["online"].as_bool() == Some(true),
        Err(why) => return why,
    };
    if !online {
        return Presence::Offline;
    }
    let mut field = |endpoint: &str, name: &str| {
        character_json(endpoint, id, troubled)
            .ok()
            .and_then(|json| json[name].as_i64())
            .filter(|n| *n > 0)
    };
    Presence::Online {
        system: field("character-location", "solar_system_id"),
        ship: field("character-ship", "ship_type_id"),
    }
}

fn register(
    viewer: &Viewer,
    hash: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let link = load_link(hash)?;
    let ticked: Vec<_> = viewer
        .characters
        .iter()
        .filter(|c| submission.checked(&format!("c_{}", c.id)))
        .collect();
    if ticked.is_empty() {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some("Tick at least one character."),
            false,
        )?));
    }
    if !link.open {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some("The link closed before you registered."),
            false,
        )?));
    }
    // aa-afat's check, character by character (at most three ESI calls
    // each, and a form holds 30 characters: within the host's 100).
    let mut chosen = Vec::new();
    let mut problems = Vec::new();
    let mut unregistered = false;
    let mut troubled = false;
    for c in ticked.iter().take(MAX_FORM_CHARACTERS) {
        let why = match presence(c.id, &mut troubled) {
            Presence::Online { system, ship } => {
                chosen.push(serde_json::json!({
                    "character_id": c.id,
                    "character_name": c.name,
                    "corporation_id": c.corporation_id,
                    "alliance_id": c.alliance_id,
                    "system_id": system,
                    "ship_type_id": ship,
                }));
                continue;
            }
            Presence::Offline => "isn't online in EVE; log in and try again",
            Presence::NotRegistered => {
                unregistered = true;
                "isn't registered for Fleet Activity Tracking"
            }
            Presence::Token => "needs to log in to Tether again (its login expired)",
            Presence::Trouble => "couldn't be checked with ESI just now; try again",
        };
        problems.push(format!("{} {why}.", c.name));
    }
    // Only while the link is open, checked by the database as it inserts;
    // a character already registered is left alone.
    let added = if chosen.is_empty() {
        Vec::new()
    } else {
        query(
            "INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id, \
                               system_id, ship_type_id) \
             SELECT l.id, x.character_id, x.character_name, NULLIF(x.corporation_id, 0), \
                    NULLIF(x.alliance_id, 0), x.system_id, x.ship_type_id \
             FROM json_to_recordset($1::json) AS x(character_id bigint, character_name text, \
                  corporation_id bigint, alliance_id bigint, system_id bigint, ship_type_id bigint) \
             JOIN links l ON l.id = $2 \
                  AND coalesce(l.expires_at > now(), l.esi_state = 'tracking') \
             ON CONFLICT (link_id, character_id) DO NOTHING \
             RETURNING corporation_id, alliance_id, system_id, ship_type_id",
            &[
                Db::json(serde_json::Value::Array(chosen).to_string()),
                link.id.into(),
            ],
        )?
    };
    if problems.is_empty() && added.is_empty() && !load_link(hash)?.open {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some("The link closed before you registered."),
            false,
        )?));
    }
    let ids: Vec<i64> = added
        .iter()
        .flat_map(|r| [int(r, 0), int(r, 1), int(r, 2), int(r, 3)])
        .collect();
    learn_names(&ids);
    if !problems.is_empty() {
        return Ok(SubmitResult::Page(register_page(
            viewer,
            hash,
            Some(&problems.join(" ")),
            unregistered,
        )?));
    }
    Ok(SubmitResult::Redirect(format!("links/{}/add", link.hash)))
}

// ---- statistics ------------------------------------------------------------

/// FATs by month, a row per key (a pilot, a corporation): the busiest
/// `MAX_STAT_ROWS`, and how many keys there are in all.
struct Grouped {
    rows: Vec<(i64, String, [i64; 12])>,
    keys: i64,
}

/// A table with a column per month and a total; past `MAX_STAT_ROWS`
/// keys, its title says it shows the busiest.
fn month_table(
    first: &str,
    title: &str,
    empty: &str,
    grouped: &Grouped,
    cell: impl Fn(i64, &str) -> Value,
) -> Table {
    let mut columns = vec![Column::text(first)];
    columns.extend(MONTHS.iter().map(|m| Column::numeric(*m)));
    columns.push(Column::numeric("Total"));
    let title = if grouped.keys > grouped.rows.len() as i64 {
        format!(
            "{title}: the busiest {} of {}",
            grouped.rows.len(),
            grouped.keys
        )
    } else {
        title.to_owned()
    };
    let mut table = Table::new(columns).title(title).empty(empty);
    for (key, label, months) in &grouped.rows {
        let mut row = vec![cell(*key, label)];
        row.extend(months.iter().map(|n| Value::from(*n)));
        row.push(months.iter().sum::<i64>().into());
        table = table.row(row);
    }
    table
}

const MONTH: &str = "extract(month FROM l.created_at AT TIME ZONE 'UTC')::bigint";

/// FATs in `year` by `key` (with `label`), a count per month, filtered by
/// `filter` on `$3` onwards: Postgres counts and ranks every key, busiest
/// first, and returns the top `MAX_STAT_ROWS`, exact however many FATs
/// there are. The SQL fragments are this file's own constants, never
/// input.
fn grouped(
    key: &str,
    label: &str,
    joins: &str,
    filter: &str,
    year: i32,
    params: &[Db],
) -> Result<Grouped, PageError> {
    let (start, end) = year_bounds(year);
    let mut all = vec![start, end];
    all.extend(params.iter().cloned());
    let months: String = (1..=12)
        .map(|m| format!(", count(*) FILTER (WHERE {MONTH} = {m})::bigint"))
        .collect();
    let rows = query(
        &format!(
            "SELECT {key}, max({label}), count(*) OVER ()::bigint{months} \
             FROM fats f JOIN links l ON l.id = f.link_id {joins} \
             WHERE l.created_at >= $1 AND l.created_at < $2 AND {filter} \
             GROUP BY 1 ORDER BY count(*) DESC, lower(max({label})), 1 LIMIT {MAX_STAT_ROWS}"
        ),
        &all,
    )?;
    Ok(Grouped {
        keys: rows.first().map_or(0, |r| int(r, 2)),
        rows: rows
            .iter()
            .map(|r| {
                let mut months = [0; 12];
                for (i, n) in months.iter_mut().enumerate() {
                    *n = int(r, i + 3);
                }
                (int(r, 0), text(r, 1), months)
            })
            .collect(),
    })
}

const CORPORATION_LABEL: &str = "coalesce(n.name, 'Unknown corporation')";
const ALLIANCE_LABEL: &str = "coalesce(n.name, 'Unknown alliance')";

/// The years as chips under the header, the one shown marked: this year
/// (at `path` itself) and the four before, and the one shown if older.
fn year_links(mut page: Page, path: &str, year: i32) -> Page {
    let now = this_year();
    let mut years: Vec<i32> = ((now - 4)..=now).rev().collect();
    if !years.contains(&year) {
        years.push(year);
    }
    for y in years {
        let to = if y == now {
            path.to_owned()
        } else {
            format!("{path}/{y}")
        };
        page = page.link(y.to_string(), to);
    }
    page
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
    Ok(year_links(page, "stats", year))
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
    let name = name_of(id, "Unknown corporation")?;
    let (stats, months) = scope_summary(filter, id, year)?;
    let pilots = grouped(
        "f.character_id",
        "f.character_name",
        "",
        filter,
        year,
        &[id.into()],
    )?;
    Ok(year_links(
        Page::new(format!("{name}: statistics {year}"))
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
            .table(fleet_type_table(filter, id, year)?),
        &format!("stats/corporation/{id}"),
        year,
    ))
}

fn alliance_page(viewer: &Viewer, id: i64, year: i32) -> Result<Page, PageError> {
    if !viewer.can("stats_corporation_other") {
        return Err(PageError::Forbidden);
    }
    let filter = "f.alliance_id = $3";
    let name = name_of(id, "Unknown alliance")?;
    let (stats, months) = scope_summary(filter, id, year)?;
    let corporations = grouped(
        "f.corporation_id",
        CORPORATION_LABEL,
        "LEFT JOIN names n ON n.id = f.corporation_id",
        &format!("{filter} AND f.corporation_id IS NOT NULL"),
        year,
        &[id.into()],
    )?;
    Ok(year_links(
        Page::new(format!("{name}: statistics {year}"))
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
            .table(fleet_type_table(filter, id, year)?),
        &format!("stats/alliance/{id}"),
        year,
    ))
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
        .map_or_else(|| "Unknown character".to_owned(), |r| text(r, 0)),
    };
    let (stats, months) = scope_summary(filter, id, year)?;
    let fleets = can_create(viewer);
    Ok(year_links(
        Page::new(format!("{name}: statistics {year}"))
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
            )),
        &format!("stats/character/{id}"),
        year,
    ))
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
                Column::text(""),
            ])
            .empty("No fleet types yet: add the kinds of fleets you run (CTA, Home Defense, Mining...)."),
            rows.iter().map(|r| {
                let enabled = flag(r, 2);
                let status = if enabled {
                    badge("Enabled", Tone::Success)
                } else {
                    badge("Disabled", Tone::Neutral)
                };
                // Each posts `change_type` with the type and what to do.
                let change = |label: &str, what: &str| {
                    action(label, "change_type")
                        .field("type", int(r, 0).to_string())
                        .field("action", what)
                };
                let buttons = vec![
                    if enabled {
                        change("Disable", "disable")
                    } else {
                        change("Enable", "enable")
                    },
                    change("Delete", "delete").tone(Tone::Danger).confirm(format!(
                        "\"{}\" is deleted; the links that used it keep it.",
                        text(r, 1)
                    )),
                ];
                vec![
                    text(r, 1).into(),
                    status.into(),
                    int(r, 3).into(),
                    actions(buttons),
                ]
            }),
        ))
        .text("Disabled types aren't offered for new links. Deleting one keeps it on the links that used it.")
        .form(
            Form::new("add_type", "Add fleet type")
                .field(Field::text("name", "Name", MAX_TYPE_NAME).required()),
        );
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

// ---- settings --------------------------------------------------------------

/// aa-afat's Setting, one row (defaults as aa-afat's if it can't be read).
struct Settings {
    expiry_minutes: i64,
    reopen_grace_minutes: i64,
    reopen_duration_minutes: i64,
    log_days: i64,
    /// aa-afat's `use_doctrines_from_fittings_module`.
    doctrines_from_fittings: bool,
}

fn settings() -> Result<Settings, PageError> {
    let rows = query(
        "SELECT expiry_minutes, reopen_grace_minutes, reopen_duration_minutes, log_days, \
                use_doctrines_from_fittings \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let row = rows.first();
    let or = |i: usize| row.map_or(60, |r| int(r, i));
    Ok(Settings {
        expiry_minutes: or(0),
        reopen_grace_minutes: or(1),
        reopen_duration_minutes: or(2),
        log_days: or(3),
        doctrines_from_fittings: row
            .and_then(|r| r.get(4))
            .and_then(Db::as_bool)
            .unwrap_or(false),
    })
}

/// The doctrine: free text, or with aa-afat's
/// `use_doctrines_from_fittings_module` one of the doctrines Fittings
/// shares that the FC may see (checked again on posting, by
/// [`doctrine_offered`]). `current`, a link's doctrine, stays offered.
/// While Fittings shares none the FC may see (it isn't installed or
/// running, say), it's typed in, as aa-afat's without Fittings.
fn doctrine_field(from_fittings: bool, current: Option<&str>) -> Field {
    let mut names = if from_fittings {
        shared_doctrines()
    } else {
        Vec::new()
    };
    if names.is_empty() {
        let mut field = Field::text("doctrine", "Doctrine", MAX_DOCTRINE);
        if from_fittings {
            field = field.help("Fittings shares no doctrines with you: type it in.");
        }
        return match current {
            Some(value) => field.value(value),
            None => field,
        };
    }
    // As the fleet types: an explicit None, so the list is never empty.
    let mut options = vec![(String::new(), "None".to_owned())];
    if let Some(value) = current.filter(|v| !names.iter().any(|n| n == v)) {
        names.insert(0, value.to_owned());
    }
    options.extend(names.into_iter().take(98).map(|n| (n.clone(), n)));
    let field = Field::select("doctrine", "Doctrine", options)
        .help("The doctrines Fittings shares with you.");
    match current {
        Some(value) => field.value(value),
        None => field,
    }
}

/// The names of the doctrines Fittings shares that the viewer may see.
fn shared_doctrines() -> Vec<String> {
    match doctrines::published() {
        Ok(shared) => {
            let mut names: Vec<String> = Vec::new();
            for d in shared {
                if d.name.chars().count() <= MAX_DOCTRINE as usize
                    && !names.iter().any(|n| n.eq_ignore_ascii_case(&d.name))
                {
                    names.push(d.name);
                }
            }
            names
        }
        Err(err) => {
            log::warn(format!("reading Fittings' doctrines: {err:?}"));
            Vec::new()
        }
    }
}

/// Whether a posted doctrine is one the form offered: anything while it's
/// typed in (Fittings sharing none the viewer may see included); with
/// doctrines from Fittings none, the link's own, or one the viewer may
/// see.
fn doctrine_offered(from_fittings: bool, doctrine: Option<&str>, current: Option<&str>) -> bool {
    match doctrine {
        _ if !from_fittings => true,
        None => true,
        Some(name) if Some(name) == current => true,
        Some(name) => {
            let shared = shared_doctrines();
            shared.is_empty() || shared.iter().any(|n| n == name)
        }
    }
}

/// What "Use doctrines from Fittings" does, and, while it's on but
/// Fittings shares no doctrines the viewer may see, that FCs type the
/// doctrine in until it does, with the fix.
fn doctrines_help(on: bool) -> String {
    let what = "New FAT link offers the doctrines Fittings shares that the FC may see, instead of \
                a text field. Default: off.";
    if on && shared_doctrines().is_empty() {
        format!(
            "{what} Fittings shares no doctrines with you now, so FCs type the doctrine in: \
             install or start Fittings (Administration, Apps), or add a doctrine there that FCs \
             may see."
        )
    } else {
        what.to_owned()
    }
}

/// aa-afat's settings (in Django's admin there), for `manage_afat`.
fn settings_page() -> Result<Page, PageError> {
    let settings = settings()?;
    let minutes = |name: &str, label: &str, value: i64, min: f64, help: &str| {
        Field::number(name, label)
            .range(Some(min), Some(MAX_EXPIRY_MINUTES as f64), true)
            .value(value.to_string())
            .help(help)
            .required()
    };
    Ok(Page::new("Settings")
        .description("For every FAT link. Defaults are aa-afat's.")
        .settings(
            SettingsForm::new("settings")
                .group(
                    SettingsGroup::new("FAT links")
                        .field(minutes(
                            "expiry_minutes",
                            "Default FAT link expiry time (minutes)",
                            settings.expiry_minutes,
                            1.0,
                            "What New FAT link offers; the FC can change it. Default: 60.",
                        ))
                        .field(minutes(
                            "reopen_grace_minutes",
                            "Default FAT link reopen grace time (minutes)",
                            settings.reopen_grace_minutes,
                            0.0,
                            "How long after closing a link can be reopened (once). 0: never. Default: 60.",
                        ))
                        .field(minutes(
                            "reopen_duration_minutes",
                            "Default FAT link reopen duration (minutes)",
                            settings.reopen_duration_minutes,
                            1.0,
                            "How long a reopened link stays open. Default: 60.",
                        ))
                        .field(
                            Field::checkbox(
                                "use_doctrines_from_fittings",
                                "Use doctrines from Fittings",
                                settings.doctrines_from_fittings,
                            )
                            .help(doctrines_help(settings.doctrines_from_fittings)),
                        ),
                )
                .group(
                    SettingsGroup::new("Log").field(
                        Field::number("log_days", "Default log duration (days)")
                            .range(Some(1.0), Some(3650.0), true)
                            .value(settings.log_days.to_string())
                            .help("How long log entries are kept. Default: 60.")
                            .required(),
                    ),
                ),
        ))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !viewer.can("manage_afat") {
        return Err(PageError::Forbidden);
    }
    // The host checks the form's ranges; the table's checks back them up.
    let number = |name: &str| -> Result<i64, PageError> {
        submission
            .value(name)
            .parse::<i64>()
            .map_err(|_| PageError::Failed(format!("{name} wasn't a whole number")))
    };
    let (expiry, grace, duration, days) = (
        number("expiry_minutes")?,
        number("reopen_grace_minutes")?,
        number("reopen_duration_minutes")?,
        number("log_days")?,
    );
    let from_fittings = submission.value("use_doctrines_from_fittings") == "true";
    run(
        &[
            Statement::new(
                "UPDATE settings SET expiry_minutes = $1, reopen_grace_minutes = $2, \
                 reopen_duration_minutes = $3, log_days = $4, use_doctrines_from_fittings = $5 \
                 WHERE id = 1",
                vec![
                    expiry.into(),
                    grace.into(),
                    duration.into(),
                    days.into(),
                    from_fittings.into(),
                ],
            ),
            log_entry(
                viewer,
                "Settings Changed",
                None,
                format!(
                    "Settings: expiry {expiry} minutes, reopen grace {grace} minutes, reopen \
                     duration {duration} minutes, logs kept {days} days, doctrines from \
                     Fittings {}",
                    if from_fittings { "on" } else { "off" }
                ),
            ),
        ],
        "saving the settings",
    )?;
    Ok(SubmitResult::Redirect("settings".into()))
}

// ---- logs ------------------------------------------------------------------

fn logs_page(viewer: &Viewer) -> Result<Page, PageError> {
    let rows = query(
        "SELECT g.at, g.event, g.actor_name, g.link_hash, g.description, l.hash IS NOT NULL, \
                g.actor_id \
         FROM logs g LEFT JOIN links l ON l.hash = g.link_hash \
         ORDER BY g.at DESC, g.id DESC LIMIT 500",
        &[],
    )?;
    let links = can_create(viewer);
    let days = settings()?.log_days;
    Ok(Page::new("Logs")
        .description(format!(
            "What FCs and managers did, newest first. Kept for {days} days."
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
                    character(int(r, 6), text(r, 2)).into(),
                    text(r, 4).into(),
                    target,
                ]
            }),
        )))
}

// ---- jobs ------------------------------------------------------------------

/// Secure Groups' FAT filter: each character's FATs in the last `days`,
/// for every setting a smart group uses.
fn report_filters() -> Result<(), JobError> {
    fill_in_affiliations()?;
    for setting in tether_plugin_sdk::filters::wanted() {
        if setting.name != "fats" {
            continue;
        }
        let days = serde_json::from_str::<serde_json::Value>(&setting.config)
            .ok()
            .and_then(|c| c.get("days").and_then(serde_json::Value::as_i64))
            .filter(|d| (1..=3650).contains(d));
        let Some(days) = days else {
            log::warn(format!("ignoring a FAT filter setting: {}", setting.config));
            continue;
        };
        let rows = storage::query(
            "SELECT character_id, count(*) FROM fats \
             WHERE created_at > now() - make_interval(days => $1::int) GROUP BY character_id",
            &[Db::Integer(days)],
        )
        .map_err(|e| JobError::Retry(format!("counting FATs: {e:?}")))?;
        let values: Vec<(i64, i64)> = rows.rows.iter().map(|r| (int(r, 0), int(r, 1))).collect();
        tether_plugin_sdk::filters::report(&setting.name, &setting.config, &values)
            .map_err(|e| JobError::Retry(format!("reporting FATs: {e:?}")))?;
    }
    Ok(())
}

/// Daily: logs older than the settings' days go.
fn housekeeping() -> Result<(), JobError> {
    let removed = storage::execute(
        "DELETE FROM logs WHERE at < now() - make_interval(days => \
             coalesce((SELECT log_days FROM settings WHERE id = 1), 60))",
        &[],
    )
    .map_err(|e| JobError::Retry(format!("clearing old logs: {e:?}")))?;
    if removed > 0 {
        log::info(format!("cleared {removed} old log entries"));
    }
    // A tracking job that gave up after its retries leaves fleets marked as
    // tracked; queue it again (it stops what's closed or over the cap).
    let tracked = storage::query(
        "SELECT 1 FROM links WHERE esi_state = 'tracking' LIMIT 1",
        &[],
    )
    .map_err(|e| JobError::Retry(format!("reading tracked fleets: {e:?}")))?;
    if !tracked.rows.is_empty() {
        queue_poll(None).map_err(|e| JobError::Retry(format!("queuing tracking: {e:?}")))?;
    }
    Ok(())
}

// ---- ESI fleet tracking ----------------------------------------------------

/// Queues the tracking job, now or at `at`. Under one key, so queuing it
/// again moves the queued one instead of adding another.
fn queue_poll(at: Option<DateTime<Utc>>) -> Result<(), jobs::Error> {
    let job = NewJob::new(TRACK_JOB).key(TRACK_JOB);
    jobs::enqueue(match at {
        Some(at) => job.at(rfc3339(at)),
        None => job,
    })
}

/// From a form: read tracked fleets now.
fn poll_now() -> Result<(), PageError> {
    queue_poll(None).map_err(|e| failed("queuing fleet tracking", e))
}

/// Stops a link's tracking and logs why, if it was still tracking. A link
/// without expiry closes with it.
fn stop_statement(
    link_id: i64,
    reason: &str,
    actor_id: i64,
    actor_name: &str,
    description: &str,
) -> Statement {
    Statement::new(
        "WITH stopped AS ( \
             UPDATE links SET esi_state = 'stopped', esi_stop_reason = $2, \
                    expires_at = coalesce(expires_at, now()) \
             WHERE id = $1 AND esi_state = 'tracking' RETURNING hash) \
         INSERT INTO logs (event, actor_id, actor_name, link_hash, description) \
         SELECT 'ESI Fleet Tracking Stopped', $3, $4, hash, $5 FROM stopped",
        vec![
            link_id.into(),
            reason.into(),
            actor_id.into(),
            actor_name.into(),
            description.into(),
        ],
    )
}

/// A link being tracked, as the job reads it.
struct Tracked {
    id: i64,
    fleet: String,
    character_id: i64,
    character_name: String,
}

/// The job stops a link's tracking.
fn stop(link: &Tracked, reason: &str) -> Result<(), JobError> {
    let why = stop_text(reason, &link.character_name);
    storage::transaction(&[
        stop_statement(
            link.id,
            reason,
            link.character_id,
            TRACKER,
            &format!("\"{}\": {why}", link.fleet),
        ),
        // A read just now: resuming waits a minute from here.
        Statement::new(
            "UPDATE links SET esi_polled_at = now() WHERE id = $1",
            vec![link.id.into()],
        ),
    ])
    .map_err(|e| JobError::Retry(format!("stopping tracking: {e:?}")))?;
    log::info(format!("stopped tracking link {}: {reason}", link.id));
    Ok(())
}

/// Every minute while any fleet is tracked: each tracked link's fleet,
/// through its FC's data-source character, adding a FAT for every member.
fn track_fleets() -> Result<(), JobError> {
    let retry = |what: &str, e: storage::Error| JobError::Retry(format!("{what}: {e:?}"));
    let rows = storage::query(
        "SELECT id, fleet, esi_character_id, esi_character_name, \
                coalesce(expires_at > now(), true), \
                esi_started_at > $2 \
         FROM links WHERE esi_state = 'tracking' \
         ORDER BY esi_polled_at NULLS FIRST, id LIMIT $1",
        &[
            TRACKED_PER_RUN.into(),
            Db::timestamp(rfc3339(Utc::now() - TRACK_CAP)),
        ],
    )
    .map_err(|e| retry("reading tracked fleets", e))?;
    let sources: Vec<i64> = esi::data_sources().iter().map(|s| s.id).collect();
    // Each character read once a run (the database allows one tracking link
    // per character already).
    let mut read: Vec<i64> = Vec::new();
    for row in &rows.rows {
        let link = Tracked {
            id: int(row, 0),
            fleet: text(row, 1),
            character_id: int(row, 2),
            character_name: text(row, 3),
        };
        if read.contains(&link.character_id) {
            continue;
        }
        read.push(link.character_id);
        if !flag(row, 4) {
            stop(&link, "closed")?;
        } else if !flag(row, 5) {
            stop(&link, "cap")?;
        } else if !sources.contains(&link.character_id) {
            stop(&link, "data_source")?;
        } else {
            poll(&link)?;
        }
    }
    // Again in a minute while any fleet is still tracked.
    let left = storage::query(
        "SELECT 1 FROM links WHERE esi_state = 'tracking' LIMIT 1",
        &[],
    )
    .map_err(|e| retry("reading tracked fleets", e))?;
    if !left.rows.is_empty() {
        queue_poll(Some(Utc::now() + TRACK_EVERY))
            .map_err(|e| JobError::Retry(format!("queuing the next poll: {e:?}")))?;
    }
    Ok(())
}

/// Reads one fleet, and adds FATs or stops tracking.
fn poll(link: &Tracked) -> Result<(), JobError> {
    let answer = match esi::get(
        "fleet-members",
        Subject::DataSource(link.character_id),
        &[],
        None,
    ) {
        Ok(response) => response.body,
        Err(esi::Error::Status(403)) => return stop(link, "refused"),
        Err(esi::Error::Status(404)) => return stop(link, "fleet_ended"),
        Err(esi::Error::NotADataSource | esi::Error::NotAllowed(_)) => {
            return stop(link, "data_source");
        }
        Err(esi::Error::Token | esi::Error::NotRegistered) => return stop(link, "token"),
        Err(err) => {
            // ESI or Tether trouble that may pass: try again next minute.
            log::warn(format!("reading the fleet of link {}: {err:?}", link.id));
            return touch(link);
        }
    };
    let answer: serde_json::Value = serde_json::from_str(&answer).unwrap_or_default();
    if answer["in_fleet"].as_bool() != Some(true) {
        return stop(link, "fleet_ended");
    }
    if answer["boss"].as_bool() != Some(true) {
        return stop(link, "not_boss");
    }
    let members: Vec<(i64, Option<i64>, Option<i64>)> = answer["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .filter_map(|m| {
                    Some((
                        m["character_id"].as_i64().filter(|id| *id > 0)?,
                        m["ship_type_id"].as_i64(),
                        m["solar_system_id"].as_i64(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    // Who each flies for now (public, one call): the fleet's affiliation,
    // for pilots the app hasn't seen too.
    let affiliated = affiliations(&members.iter().map(|m| m.0).collect::<Vec<_>>());
    let of = |c: i64| affiliated.iter().find(|a| a.0 == c).map(|a| (a.1, a.2));
    // Names for pilots, ships, systems, corporations and alliances we don't
    // know yet.
    learn_names(
        &members
            .iter()
            .flat_map(|(c, s, y)| [*c, s.unwrap_or_default(), y.unwrap_or_default()])
            .chain(
                affiliated
                    .iter()
                    .flat_map(|a| [a.1, a.2.unwrap_or_default()]),
            )
            .collect::<Vec<_>>(),
    );
    let rows: Vec<serde_json::Value> = members
        .iter()
        .map(|(c, s, y)| {
            let (corporation, alliance) = of(*c).map_or((None, None), |(co, al)| (Some(co), al));
            serde_json::json!({
                "character_id": c, "ship_type_id": s, "solar_system_id": y,
                "corporation_id": corporation, "alliance_id": alliance,
            })
        })
        .collect();
    // A FAT for everyone not on the link yet; one already there (clicked,
    // or added by hand) gains the ship, system and affiliation it lacked.
    // Affiliation is ESI's now, else what the app last saw.
    storage::transaction(&[
        Statement::new(
            "INSERT INTO fats (link_id, character_id, character_name, corporation_id, alliance_id, \
                               ship_type_id, system_id, esi) \
             SELECT l.id, x.character_id, \
                    coalesce(c.name, n.name, 'Character ' || x.character_id::text), \
                    coalesce(x.corporation_id, c.corporation_id), \
                    CASE WHEN x.corporation_id IS NOT NULL THEN x.alliance_id ELSE c.alliance_id END, \
                    x.ship_type_id, x.solar_system_id, true \
             FROM json_to_recordset($1::json) AS x(character_id bigint, ship_type_id bigint, \
                  solar_system_id bigint, corporation_id bigint, alliance_id bigint) \
             JOIN links l ON l.id = $2 AND l.esi_state = 'tracking' \
                  AND coalesce(l.expires_at > now(), true) \
             LEFT JOIN characters c ON c.character_id = x.character_id \
             LEFT JOIN names n ON n.id = x.character_id \
             ON CONFLICT (link_id, character_id) DO UPDATE SET \
                 ship_type_id = coalesce(fats.ship_type_id, EXCLUDED.ship_type_id), \
                 system_id = coalesce(fats.system_id, EXCLUDED.system_id), \
                 alliance_id = CASE WHEN fats.corporation_id IS NULL THEN EXCLUDED.alliance_id \
                     ELSE fats.alliance_id END, \
                 corporation_id = coalesce(fats.corporation_id, EXCLUDED.corporation_id)",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                link.id.into(),
            ],
        ),
        Statement::new(
            "UPDATE links SET esi_polled_at = now() WHERE id = $1",
            vec![link.id.into()],
        ),
    ])
    .map_err(|e| JobError::Retry(format!("storing fleet members: {e:?}")))?;
    Ok(())
}

/// Marks a link read (so the others get their turn) without changes.
fn touch(link: &Tracked) -> Result<(), JobError> {
    storage::execute(
        "UPDATE links SET esi_polled_at = now() WHERE id = $1",
        &[link.id.into()],
    )
    .map(|_| ())
    .map_err(|e| JobError::Retry(format!("marking a fleet read: {e:?}")))
}
