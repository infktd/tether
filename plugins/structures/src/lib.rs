//! Structures (aa-structures; PRD F23).
//!
//! - Owners are corporations, added through a data source (AA's "Add
//!   Structure Owner"): a character with the in-game Station Manager role,
//!   added by a holder of `add_structure_owner`, in use at once.
//! - The structure list: name, type, system and region, fuel and time
//!   left, services, state and its timer, reinforce hour; filtered by
//!   owner, low fuel and reinforced, and seen by permission (the viewer's
//!   corporation, alliance, or all).
//! - Owners' structure notifications (attacks, reinforcements, fuel,
//!   services, power, anchoring, moon drills) go to the Discord channels a
//!   manager picks, once each, filtered by type and pinging by severity as
//!   aa-structures' webhooks do (danger and warning pings mention the
//!   Discord roles of the states the settings name, standing in for
//!   @everyone and @here); aa-structures' fuel alert configs add low-fuel
//!   alerts, any number, each with its range, repeat and ping.
//! - Unanchoring times only for `view_all_unanchoring_status`, as
//!   aa-structures.
//! - Up to 10 sync characters per owner, rotated, cut the notification
//!   delay from ten minutes to about one (aa-structures').
//! - Timers from notifications and structures' states, and moon
//!   extractions' chunks ready (aa-structures'
//!   STRUCTURES_MOON_EXTRACTION_TIMERS_ENABLED), are listed here and
//!   published for Structure Timers after every sync (friendly, and
//!   corporation-only if a manager says so, as aa-structures'
//!   STRUCTURES_TIMERS_ARE_CORP_RESTRICTED).
//! - A Jump gates tab: Ansiblex gates with their liquid ozone
//!   (aa-structures' STRUCTURES_SHOW_JUMP_GATES).
//! - Admin notices (aa-structures' STRUCTURES_ADMIN_NOTIFICATIONS_ENABLED):
//!   holders of `manage` and superusers hear of owners added, and of an
//!   owner's reads stopping, coming back or first working (`admin`).
//! - ESI is read gently: notifications at most every 10 minutes and
//!   structures every hour (their cache times), and an owner ESI answers
//!   403 for (a lost role) is left alone for an hour, doubling to a day.

mod admin;
mod card;
mod detail;
mod notification;
mod orbitals;
mod routing;
mod tags;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;
use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Action, Column, Field, Form, Page, PageError, Plugin, Request, Section, SettingsForm,
    SettingsGroup, Stat, Submission, SubmitResult, Table, Tone, Toolbar, Value, action, actions,
    alliance, badge, character, corporation, countdown, defenses, item_type, link, log, time,
};

use crate::notification::{Category, Context, Fields};
use crate::routing::Routes;

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
/// Rows per table on a page (the host caps values per page at 10,000:
/// 14 columns of Upwell structures, 12 of starbases, 10 of orbitals, and
/// the short tables, jump gates' 11 among them).
const LIST_ROWS: i64 = 250;
const STARBASE_ROWS: i64 = 120;
const ORBITAL_ROWS: i64 = 150;
const SHORT_ROWS: i64 = 60;
const GATE_ROWS: i64 = 50;
const TIMER_ROWS: i64 = 80;
const OWNER_ROWS: i64 = 60;
/// Rows of the public customs office list (6 columns).
const POCO_ROWS: i64 = 500;
/// Fuel alert configs' hours: a year at most.
const MAX_ALERT_HOURS: i64 = 8760;
/// Fuel alerts queued per run.
const FUEL_ALERTS_PER_RUN: usize = 200;
/// Fuel alert configs a page lists (aa-structures has no limit; this is
/// the page's).
const MAX_FUEL_CONFIGS: i64 = 100;
/// Sync characters used per owner, rotated (aa-structures').
const MAX_SYNC_CHARACTERS: i64 = 10;
/// The job that reads notifications between syncs while an owner has more
/// than one sync character, keyed so there's only ever one.
const NOTIFICATIONS_JOB: &str = "notifications";
/// How often it runs: about aa-structures' one-minute delay.
const NOTIFICATIONS_GAP: Duration = Duration::seconds(60);
/// Timers published for Structure Timers (the host's limit).
const MAX_PUBLISHED: i64 = 500;
/// A relayed message's characters, at most: under the host's 1,500.
const MAX_MESSAGE_CHARS: usize = 1_400;
/// The one-off job that publishes timers at once (after a settings change).
const PUBLISH_JOB: &str = "publish_timers";
/// The assets read again a minute after a sync, while Tether still reads a
/// corporation's assets in the background (`orbitals::read_assets`).
const ASSETS_JOB: &str = "assets_again";

struct Structures;

impl Plugin for Structures {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        Ok(with_chips(render_page(&request, &viewer)?, &request.path))
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        Ok(match submit_form(&submission, &viewer)? {
            SubmitResult::Page(page) => {
                SubmitResult::Page(with_chips(page, &submission.request.path))
            }
            other => other,
        })
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" => sync(),
            "relay" => relay(),
            NOTIFICATIONS_JOB => notifications_between_syncs(),
            PUBLISH_JOB => publish_timers(),
            ASSETS_JOB => assets_again(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Structures);

/// The settings pages' own pages (aa-structures' settings and tags), as
/// chips under the Manage bar Tether draws.
fn with_chips(page: Page, path: &str) -> Page {
    if path == "settings" || path.starts_with("settings/") {
        page.link("General", "settings")
            .link("Tags", "settings/tags")
    } else {
        page
    }
}

fn render_page(request: &Request, viewer: &Viewer) -> Result<Page, PageError> {
    let path = request.path.as_str();
    if path.is_empty() {
        // The toolbar's tag filter (any of them), in the address.
        let tags: Vec<i64> = request
            .params("tag")
            .iter()
            .filter_map(|id| id.parse::<i32>().ok())
            .filter(|id| *id > 0)
            .map(i64::from)
            .take(30)
            .collect();
        return list_page(
            viewer,
            Filter {
                owner: None,
                tags: (!tags.is_empty()).then_some(tags),
            },
        );
    }
    if let Some(corp) = path.strip_prefix("owner/") {
        let corp: i64 = corp.parse().map_err(|_| PageError::NotFound)?;
        return list_page(
            viewer,
            Filter {
                owner: Some(corp),
                tags: None,
            },
        );
    }
    if let Some(ids) = path.strip_prefix("tags/") {
        let ids = tags::parse_filter(ids).ok_or(PageError::NotFound)?;
        return list_page(
            viewer,
            Filter {
                owner: None,
                tags: Some(ids),
            },
        );
    }
    if let Some(id) = path.strip_prefix("structure/") {
        let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
        return detail::page(viewer, id);
    }
    if let Some(corp) = path.strip_prefix("settings/owner/") {
        let corp: i64 = corp.parse().map_err(|_| PageError::NotFound)?;
        return owner_settings_page(corp);
    }
    match path {
        "settings" => settings_page(None),
        "settings/tags" => tags::settings_page(None),
        "pocos" => pocos_page(viewer),
        _ => Err(PageError::NotFound),
    }
}

fn submit_form(submission: &Submission, viewer: &Viewer) -> Result<SubmitResult, PageError> {
    let path = submission.request.path.as_str();
    // Every form's page checks the viewer may open it (the host), and
    // managers' forms need manage (the host, for settings pages; here,
    // for the structure page's tags).
    if let Some(id) = path.strip_prefix("structure/") {
        let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
        if submission.form != "structure_tags"
            || !viewer.can("manage")
            || !detail::visible_structure(viewer, id)?
        {
            return Err(PageError::NotFound);
        }
        return tags::save_structure_tags(viewer, id, submission);
    }
    // The settings pages' forms and row buttons are managers': checked
    // here too, not only by the host's page rule.
    if path.starts_with("settings") && !viewer.can("manage") {
        return Err(PageError::Forbidden);
    }
    if let Some(corp) = path.strip_prefix("settings/owner/") {
        let corp: i64 = corp.parse().map_err(|_| PageError::NotFound)?;
        return match submission.form.as_str() {
            "owner_routes" => save_owner_settings(viewer, corp, submission),
            _ => Err(PageError::NotFound),
        };
    }
    match (path, submission.form.as_str()) {
        ("settings", "settings") => save_settings(viewer, submission),
        ("settings", "add_fuel_alert") => add_fuel_alert(viewer, submission),
        ("settings", "delete_fuel_alert") => delete_fuel_alert(viewer, submission),
        ("settings", "edit_fuel_alert") => edit_fuel_alert(viewer, submission),
        ("settings", "toggle_fuel_alert") => toggle_fuel_alert(viewer, submission),
        ("settings", "edit_jump_fuel_alert") => edit_jump_fuel_alert(viewer, submission),
        ("settings", "toggle_jump_fuel_alert") => toggle_jump_fuel_alert(viewer, submission),
        ("settings", "add_jump_fuel_alert") => add_jump_fuel_alert(viewer, submission),
        ("settings", "delete_jump_fuel_alert") => delete_jump_fuel_alert(viewer, submission),
        // A row's Retry now, in the settings' owner table.
        ("settings", "retry") => retry_owner(viewer, submission),
        ("settings/tags", "save_tag") => tags::save_tag(viewer, submission),
        ("settings/tags", "delete_tag") => tags::delete_tag(viewer, submission),
        _ => Err(PageError::NotFound),
    }
}

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

/// An instant: counting down while it's ahead, else the EVE time it was.
fn upcoming(t: DateTime<Utc>, now: DateTime<Utc>) -> Value {
    if t > now {
        countdown(rfc3339(t))
    } else {
        time(rfc3339(t))
    }
}

/// A type's icon and name, when its id is known.
fn typed(id: i64, name: impl Into<String>) -> Value {
    if id > 0 {
        item_type(id, name).into()
    } else {
        name.into().into()
    }
}

/// A corporation's logo and name, when its id is known.
fn owner_value(id: i64, name: impl Into<String>) -> Value {
    if id > 0 {
        corporation(id, name).into()
    } else {
        name.into().into()
    }
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
    sov: Option<String>,
    war: Option<String>,
    corp: Option<String>,
    /// aa-structures' default pings: danger and warning notifications
    /// mention the roles below.
    default_pings: bool,
    /// The states whose Discord roles stand in for @everyone (danger) and
    /// @here (warning).
    danger_ping: Option<String>,
    warning_ping: Option<String>,
    /// The types sent; none: every type.
    notification_types: Option<Vec<String>>,
    /// Published timers are seen only by the owning corporation.
    timers_corporation_only: bool,
    /// The list shows structures with a default tag unless filtered.
    default_tags_filter: bool,
    /// The largest enabled fuel alert's start: the Low fuel tab's hours.
    low_fuel_hours: i64,
    /// A moon extraction started gives a timer for its chunk
    /// (aa-structures' STRUCTURES_MOON_EXTRACTION_TIMERS_ENABLED).
    moon_extraction_timers: bool,
    /// The list's Jump gates tab (aa-structures' STRUCTURES_SHOW_JUMP_GATES).
    show_jump_gates: bool,
    /// Admins hear of owners added and services down or back
    /// (aa-structures' STRUCTURES_ADMIN_NOTIFICATIONS_ENABLED).
    admin_notifications: bool,
}

impl Settings {
    fn channel(&self, category: Category) -> Option<&str> {
        match category {
            Category::Attack => self.attack.as_deref(),
            Category::Fuel => self.fuel.as_deref(),
            Category::State => self.state.as_deref(),
            Category::Moon => self.moon.as_deref(),
            Category::Sov => self.sov.as_deref(),
            Category::War => self.war.as_deref(),
            Category::Corp => self.corp.as_deref(),
        }
    }
}

fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT attack_channel, fuel_channel, state_channel, moon_channel, danger_ping, default_pings, \
                timers_corporation_only, default_tags_filter, warning_ping, \
                array_to_string(notification_types, ','), \
                coalesce((SELECT max(start_hours) FROM fuel_alert_configs WHERE enabled), 72)::bigint, \
                sov_channel, war_channel, corp_channel, moon_extraction_timers, show_jump_gates, \
                admin_notifications \
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
        sov: channel(11),
        war: channel(12),
        corp: channel(13),
        danger_ping: channel(4),
        warning_ping: channel(8),
        notification_types: routing::type_list(row.and_then(|r| r.get(9))),
        low_fuel_hours: row.map_or(72, |r| int(r, 10)),
        default_pings: row
            .and_then(|r| r.get(5))
            .and_then(Db::as_bool)
            .unwrap_or(false),
        timers_corporation_only: row
            .and_then(|r| r.get(6))
            .and_then(Db::as_bool)
            .unwrap_or(false),
        default_tags_filter: row
            .and_then(|r| r.get(7))
            .and_then(Db::as_bool)
            .unwrap_or(false),
        moon_extraction_timers: row
            .and_then(|r| r.get(14))
            .and_then(Db::as_bool)
            .unwrap_or(true),
        show_jump_gates: row
            .and_then(|r| r.get(15))
            .and_then(Db::as_bool)
            .unwrap_or(true),
        admin_notifications: row
            .and_then(|r| r.get(16))
            .and_then(Db::as_bool)
            .unwrap_or(false),
    })
}

// ---- jobs ------------------------------------------------------------------

/// Calls left this run, and the most one paged read may have.
struct Budget {
    left: usize,
    most: usize,
}

impl Budget {
    fn new(calls: usize) -> Self {
        Budget {
            left: calls,
            most: calls,
        }
    }

    /// The hourly reads begin: every corporation's notifications have
    /// been read, so no paged read can have more than what's left. One
    /// longer than that can't be made this run even if it went first.
    fn hourly(&mut self) {
        self.most = self.left;
    }

    fn take(&mut self) -> bool {
        self.take_n(1)
    }

    fn take_n(&mut self, n: usize) -> bool {
        if self.left < n {
            return false;
        }
        self.left -= n;
        true
    }

    /// Whether `n` calls are left.
    fn has(&self, n: usize) -> bool {
        self.left >= n
    }
}

/// What a call costs of the host's per-run limit (the host's `esi_cost`):
/// the structure assets come from a read in the background, a system with
/// its constellation.
fn esi_cost(endpoint: &str) -> usize {
    match endpoint {
        "corporation-structure-assets" | "universe-system" => 2,
        _ => 1,
    }
}

/// What ESI said, for an owner's record.
enum Outcome {
    Ok(Vec<String>),
    /// Leave this owner alone for a while (a lost role or token).
    BackOff(String),
    /// Try again next run.
    Later(String),
    /// Tether is reading it in the background (the corporation's assets):
    /// ask again on a later run, with nothing wrong to show.
    Waiting,
}

fn call(
    budget: &mut Budget,
    endpoint: &str,
    subject: Subject,
    params: &[(String, String)],
    paged: bool,
) -> Outcome {
    let describe = |err: esi::Error| match err {
        esi::Error::Status(403) => Outcome::BackOff(
            "ESI said 403: the character lacks the in-game role (Station Manager for structures, \
             Director for starbases, customs offices and assets) or left the corporation"
                .to_owned(),
        ),
        esi::Error::Token => Outcome::BackOff(
            "the character's token is gone or lacks this read's scope: its owner must offer it \
             again (log in with it)"
                .to_owned(),
        ),
        esi::Error::NotADataSource => Outcome::BackOff("no longer a data source in use".to_owned()),
        // Every page of the assets is read in the background (the host's
        // `corporation-structure-assets`): ready on a later run.
        esi::Error::Unavailable if endpoint == "corporation-structure-assets" => Outcome::Waiting,
        // More pages of assets than Tether reads (about two million
        // items): asking again soon won't change it.
        esi::Error::Invalid(why) if why.contains("pages of assets") => Outcome::BackOff(why),
        other => Outcome::Later(format!("{other:?}")),
    };
    let cost = esi_cost(endpoint);
    if !budget.take_n(cost) {
        return Outcome::Later("out of ESI calls this run".to_owned());
    }
    let first = match esi::get(endpoint, subject, params, paged.then_some(1)) {
        Ok(first) => first,
        Err(err) => return describe(err),
    };
    // Every page or none (a part isn't kept): pages that can't all be read
    // aren't started, so they don't use up the run's calls for nothing.
    let pages = usize::try_from(first.pages).unwrap_or(usize::MAX);
    let what = match endpoint {
        "corporation-structures" => "the corporation's structures",
        "corporation-customs-offices" => "the corporation's customs offices",
        _ => "ESI's answer",
    };
    // Judged against what the hourly reads had after every corporation's
    // notifications: a read that couldn't fit even going first would
    // otherwise be tried, and its first page paid for, every sync.
    if paged && pages.saturating_mul(cost) > budget.most {
        return Outcome::BackOff(format!(
            "{what}: {pages} pages at ESI, more than one sync can read ({} after the \
             notifications); what was read before stays",
            budget.most / cost
        ));
    }
    if paged && !budget.has(pages.saturating_sub(1) * cost) {
        return Outcome::Later(format!(
            "{what}: {pages} pages, more than this run had ESI calls left for; tried again \
             next sync"
        ));
    }
    let mut bodies = vec![first.body];
    if paged {
        for page in 2..=first.pages {
            if !budget.take_n(cost) {
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
    /// Starbases and their fuel (Director).
    Starbases,
    /// Customs offices (Director).
    Offices,
    /// What's in structures' slots and bays, and skyhooks (Director).
    Assets,
}

impl Read {
    /// Column prefix in `owners` (fixed names, never data).
    fn column(self) -> &'static str {
        match self {
            Read::Structures => "structures",
            Read::Notifications => "notifications",
            Read::Starbases => "starbases",
            Read::Offices => "offices",
            Read::Assets => "assets",
        }
    }

    fn every(self) -> &'static str {
        match self {
            Read::Notifications => NOTIFICATIONS_EVERY,
            // ESI caches all of these for an hour.
            Read::Structures | Read::Starbases | Read::Offices | Read::Assets => STRUCTURES_EVERY,
        }
    }
}

/// An owner's sync characters: the first [`MAX_SYNC_CHARACTERS`] added
/// (aa-structures uses up to 10). Takes `$1`, the corporation.
fn sync_characters() -> String {
    format!(
        "(SELECT character_id FROM (SELECT character_id, row_number() \
         OVER (ORDER BY added_at, character_id) AS n FROM owners WHERE corporation_id = $1) x \
         WHERE n <= {MAX_SYNC_CHARACTERS})"
    )
}

/// The owner character to read a corporation with now, if its last read
/// is old enough (the ESI cache) and one isn't backing off.
///
/// Notifications rotate through the owner's sync characters, as
/// aa-structures': ESI caches each character's for ten minutes, so with N
/// of them the corporation is read every 9/N minutes, each character
/// still at most every 9.
fn pick_owner(corp: i64, read: Read) -> Result<Option<i64>, JobError> {
    let c = read.column();
    let sc = sync_characters();
    let (gap, own) = match read {
        Read::Notifications => (
            format!(
                "make_interval(secs => 540.0 / greatest(1, (SELECT count(*) FROM owners u \
                 WHERE u.corporation_id = $1 AND u.character_id IN {sc} \
                   AND (u.{c}_retry_at IS NULL OR u.{c}_retry_at <= now()))))"
            ),
            format!(
                "AND (o.{c}_at IS NULL OR o.{c}_at <= now() - interval '{}')",
                read.every()
            ),
        ),
        _ => (format!("interval '{}'", read.every()), String::new()),
    };
    let rows = storage::query(
        &format!(
            "SELECT character_id FROM owners o \
             WHERE corporation_id = $1 AND character_id IN {sc} \
               AND ({c}_retry_at IS NULL OR {c}_retry_at <= now()) {own} \
               AND NOT EXISTS (SELECT 1 FROM owners f WHERE f.corporation_id = $1 \
                   AND f.{c}_at > now() - {gap}) \
             ORDER BY {c}_failures, {c}_at NULLS FIRST, character_id LIMIT 1"
        ),
        &[corp.into()],
    )
    .map_err(|e| retry("picking an owner", e))?;
    Ok(rows.rows.first().map(|r| int(r, 0)))
}

/// Reads one corporation's notifications if one of its sync characters
/// is due. Whether any were read.
fn read_notifications(budget: &mut Budget, corp: i64) -> Result<bool, JobError> {
    let Some(owner) = pick_owner(corp, Read::Notifications)? else {
        return Ok(false);
    };
    let outcome = call(
        budget,
        "corporation-structure-notifications",
        Subject::DataSource(owner),
        &[],
        false,
    );
    if let Outcome::Ok(bodies) = &outcome {
        store_notifications(corp, owner, bodies)?;
    }
    record(owner, Read::Notifications, &outcome)?;
    Ok(matches!(outcome, Outcome::Ok(_)))
}

/// Owners with more than one usable sync character: those whose
/// notifications are read between syncs.
fn rotating_owners() -> Result<Vec<i64>, JobError> {
    let rows = storage::query(
        "SELECT corporation_id FROM owners \
         WHERE notifications_retry_at IS NULL OR notifications_retry_at <= now() \
         GROUP BY corporation_id HAVING count(*) > 1",
        &[],
    )
    .map_err(|e| retry("reading owners", e))?;
    Ok(rows.rows.iter().map(|r| int(r, 0)).collect())
}

/// Queues the between-syncs notification reads in a minute while an owner
/// has more than one sync character.
fn queue_notifications() -> Result<(), JobError> {
    if rotating_owners()?.is_empty() {
        return Ok(());
    }
    jobs::enqueue(
        NewJob::new(NOTIFICATIONS_JOB)
            .key(NOTIFICATIONS_JOB)
            .at(rfc3339(Utc::now() + NOTIFICATIONS_GAP)),
    )
    .map_err(|e| retry("queuing notification reads", e))
}

/// Between syncs: notifications of owners with several sync characters,
/// relayed at once (aa-structures' rotation, about a minute's delay).
/// Structures, timers and alerts wait for the sync.
fn notifications_between_syncs() -> Result<(), JobError> {
    let mut budget = Budget::new(ESI_BUDGET);
    let mut read = false;
    for corp in rotating_owners()? {
        read |= read_notifications(&mut budget, corp)?;
    }
    if read {
        handle_notifications()?;
        queue_relay(None)?;
    }
    queue_notifications()
}

fn record(owner: i64, read: Read, outcome: &Outcome) -> Result<(), JobError> {
    let c = read.column();
    let (sql, params): (String, Vec<Db>) = match outcome {
        // The problem shown goes only once no other read of this owner's is
        // backing off or waiting to try again: one read working mustn't
        // hide another's trouble.
        Outcome::Ok(_) => {
            let others = [
                "structures",
                "notifications",
                "starbases",
                "offices",
                "assets",
            ]
            .iter()
            .filter(|other| **other != c)
            .map(|other| format!("{other}_failures = 0 AND {other}_retry_at IS NULL"))
            .collect::<Vec<_>>()
            .join(" AND ");
            (
                format!(
                    "UPDATE owners SET {c}_at = now(), {c}_failures = 0, {c}_retry_at = NULL, \
                     last_error = CASE WHEN {others} THEN NULL ELSE last_error END \
                     WHERE character_id = $1"
                ),
                vec![owner.into()],
            )
        }
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
        Outcome::Waiting => {
            log::info(format!(
                "{c} for owner {owner}: Tether is reading the corporation's assets; \
                 they come in on a later sync"
            ));
            (
                format!("UPDATE owners SET {c}_retry_at = NULL WHERE character_id = $1"),
                vec![owner.into()],
            )
        }
        Outcome::Later(why) => {
            log::warn(format!("{c} for owner {owner}: {why}"));
            // A short pause for this character and read (without counting
            // a failure): the next character in turn, or the next sync,
            // tries instead of this one again every minute.
            (
                format!(
                    "UPDATE owners SET last_error = $2, {c}_retry_at = now() + interval '5 minutes' \
                     WHERE character_id = $1"
                ),
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
/// step failing, or owners gone), so what it shows follows at once; and
/// admins hear of owners' services however the steps went (a step that
/// failed is when they most need to).
fn sync() -> Result<(), JobError> {
    let synced = sync_steps();
    let published = publish_timers();
    if let Err(err) = admin::notices() {
        log::warn(format!("admin notices weren't sent: {err:?}"));
    }
    synced?;
    published?;
    queue_relay(None)?;
    if let Err(err) = queue_notifications() {
        log::warn(format!(
            "notification reads between syncs weren't queued: {err:?}"
        ));
    }
    Ok(())
}

fn sync_steps() -> Result<(), JobError> {
    let corporations = sync_owners()?;
    if corporations.is_empty() {
        log::info("no structure owners yet: add one");
        return Ok(());
    }
    let mut budget = Budget::new(ESI_BUDGET);
    // Every corporation's notifications first: an hourly read below can be
    // long (a large corporation's assets), and must never keep another's
    // attacks from being read.
    for &corp in &corporations {
        read_notifications(&mut budget, corp)?;
    }
    budget.hourly();
    // Assets Tether is still reading in the background: asked again a
    // minute on.
    let mut waiting = false;
    // Then the hourly reads, the corporation read longest ago first, so
    // one this run's calls didn't reach goes first next time.
    for corp in stalest_first(&corporations)? {
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
        if let Some(owner) = pick_owner(corp, Read::Starbases)? {
            let outcome = orbitals::read_starbases(&mut budget, corp, owner)?;
            record(owner, Read::Starbases, &outcome)?;
        }
        if let Some(owner) = pick_owner(corp, Read::Offices)? {
            let outcome = orbitals::read_offices(&mut budget, corp, owner)?;
            record(owner, Read::Offices, &outcome)?;
        }
        if let Some(owner) = pick_owner(corp, Read::Assets)? {
            let outcome = orbitals::read_assets(&mut budget, corp, owner)?;
            waiting |= matches!(outcome, Outcome::Waiting);
            record(owner, Read::Assets, &outcome)?;
        }
    }
    if waiting {
        queue_assets_again()?;
    }
    orbitals::learn_sovereignty(&mut budget)?;
    learn_systems(&mut budget)?;
    orbitals::learn_planets(&mut budget)?;
    orbitals::learn_moons(&mut budget)?;
    orbitals::resolve_orbitals()?;
    learn_names(&mut budget)?;
    orbitals::compute_fuel()?;
    tags::apply_generated()?;
    handle_notifications()?;
    orbitals::starbase_reinforcements()?;
    fuel_alerts()?;
    refuelled()?;
    jump_fuel_alerts()?;
    storage::execute(
        "DELETE FROM timers WHERE at < now() - interval '7 days'",
        &[],
    )
    .map_err(|e| retry("expiring timers", e))?;
    storage::execute(
        "DELETE FROM sov_timers WHERE at < now() - interval '7 days'",
        &[],
    )
    .map_err(|e| retry("expiring sovereignty timers", e))?;
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
    storage::transaction(&[
        Statement::new(
            "DELETE FROM structure_tags t WHERE NOT EXISTS \
             (SELECT 1 FROM structures s WHERE s.structure_id = t.structure_id)",
            vec![],
        ),
        Statement::new(
            "DELETE FROM structure_items i WHERE NOT EXISTS \
             (SELECT 1 FROM structures s WHERE s.structure_id = i.structure_id)",
            vec![],
        ),
    ])
    .map_err(|e| retry("expiring tags and items", e))?;
    Ok(())
}

/// The corporations, the one whose oldest hourly read (structures,
/// starbases, customs offices or assets; never read counts as oldest) is
/// furthest back first, then by id.
fn stalest_first(corporations: &[i64]) -> Result<Vec<i64>, JobError> {
    let rows = storage::query(
        "SELECT corporation_id FROM owners \
         WHERE corporation_id = ANY(string_to_array($1, ',')::bigint[]) \
         GROUP BY corporation_id \
         ORDER BY least(coalesce(max(structures_at), '-infinity'), \
                        coalesce(max(starbases_at), '-infinity'), \
                        coalesce(max(offices_at), '-infinity'), \
                        coalesce(max(assets_at), '-infinity')), corporation_id",
        &[id_list(corporations).into()],
    )
    .map_err(|e| retry("ordering owners", e))?;
    Ok(rows.rows.iter().map(|r| int(r, 0)).collect())
}

/// The owners table follows the host's data sources in use; a
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
                 (they go if none is listed for an hour)",
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
             corporation_since = CASE WHEN owners.corporation_id IS DISTINCT FROM EXCLUDED.corporation_id \
                 THEN now() ELSE owners.corporation_since END, \
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
        // An owner gone and added again is new again to its admins.
        Statement::new(
            "DELETE FROM owner_status WHERE corporation_id NOT IN (SELECT corporation_id FROM owners)",
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
    #[serde(default)]
    sender_id: Option<i64>,
}

/// FNV-1a: a stable key for a notification that names no structure (the
/// same event reaches each owner character with its own id).
fn text_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// New notifications, stored once by id (and once per event, whichever
/// of the corporation's owner characters saw it). Ones about a
/// corporation from before `owner` joined this one are its last
/// corporation's, and left out.
fn store_notifications(corp: i64, owner: i64, bodies: &[String]) -> Result<(), JobError> {
    let since = storage::query(
        "SELECT corporation_since FROM owners WHERE character_id = $1",
        &[owner.into()],
    )
    .map_err(|e| retry("reading the owner", e))?;
    let since = since.rows.first().and_then(|r| when(r, 0));
    let mut rows = Vec::new();
    for body in bodies {
        let Ok(items) = serde_json::from_str::<Vec<Notification>>(body) else {
            log::warn(format!(
                "notifications for corporation {corp}: unexpected answer"
            ));
            continue;
        };
        for n in items {
            // Types Structures sends, but not ones it makes itself.
            if notification::category(&n.kind).is_none()
                || notification::GENERATED.contains(&n.kind.as_str())
            {
                continue;
            }
            let Some(at) = parse_time(&n.timestamp) else {
                continue;
            };
            let text = n.text.unwrap_or_default();
            let fields = Fields::parse(&text);
            let related = notification::structure_related(&n.kind);
            if !related && since.is_some_and(|since| at < since) {
                continue;
            }
            let event_key = if related {
                let about = fields
                    .structure_id()
                    .or_else(|| fields.moon_id())
                    .or_else(|| fields.planet_id());
                format!(
                    "{}:{}:{}",
                    n.kind,
                    about.unwrap_or_default(),
                    at.timestamp()
                )
            } else {
                // Per corporation: each of an alliance's gets its own copy.
                format!(
                    "{}:{corp}:{:x}:{}",
                    n.kind,
                    text_hash(&text),
                    at.timestamp()
                )
            };
            rows.push(serde_json::json!({
                "notification_id": n.notification_id,
                "type": n.kind,
                "at": rfc3339(at),
                "structure_id": fields.structure_id(),
                "moon_id": fields.moon_id(),
                "planet_id": fields.planet_id(),
                "event_key": event_key,
                "text": text,
                "sender_id": n.sender_id,
                "structure_related": related,
            }));
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    storage::execute(
        "INSERT INTO notifications (notification_id, corporation_id, type, at, structure_id, moon_id, \
             planet_id, event_key, text, sender_id, structure_related) \
         SELECT notification_id, $2, type, at, structure_id, moon_id, planet_id, event_key, text, \
             sender_id, structure_related \
         FROM json_to_recordset($1::json) AS x(notification_id bigint, type text, at timestamptz, \
              structure_id bigint, moon_id bigint, planet_id bigint, event_key text, text text, \
              sender_id bigint, structure_related boolean) \
         ON CONFLICT DO NOTHING",
        &[
            Db::json(serde_json::Value::Array(rows).to_string()),
            corp.into(),
        ],
    )
    .map_err(|e| retry("storing notifications", e))?;
    Ok(())
}

/// The corporation's Upwell structures, replaced whole (gone ones were
/// destroyed or unanchored), and timers from their states. A Metenox's
/// fuel is the earlier of its blocks (ESI's) and its magmatic gas (from
/// the assets).
fn store_structures(corp: i64, bodies: &[String]) -> Result<(), JobError> {
    let mut statements = vec![
        Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, kind, name, type_id, system_id, fuel_expires, \
                 blocks_expires, state, state_timer_start, state_timer_end, unanchors_at, reinforce_hour, \
                 next_reinforce_hour, next_reinforce_apply, services, last_online, updated_at) \
             SELECT structure_id, $2, 'upwell', coalesce(name, 'Structure ' || structure_id::text), type_id, \
                 system_id, fuel_expires, fuel_expires, coalesce(state, 'unknown'), state_timer_start, \
                 state_timer_end, unanchors_at, reinforce_hour, next_reinforce_hour, next_reinforce_apply, \
                 coalesce(services, '[]'::jsonb), \
                 CASE WHEN EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(services, '[]'::jsonb)) v \
                                   WHERE v ->> 'state' = 'online') THEN now() END, \
                 now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, name text, type_id bigint, \
                 system_id bigint, fuel_expires timestamptz, state text, state_timer_start timestamptz, \
                 state_timer_end timestamptz, unanchors_at timestamptz, reinforce_hour integer, \
                 next_reinforce_hour integer, next_reinforce_apply timestamptz, services jsonb) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                 kind = 'upwell', name = EXCLUDED.name, type_id = EXCLUDED.type_id, system_id = EXCLUDED.system_id, \
                 blocks_expires = EXCLUDED.fuel_expires, \
                 fuel_expires = least(EXCLUDED.fuel_expires, structures.gas_expires), state = EXCLUDED.state, \
                 state_timer_start = EXCLUDED.state_timer_start, state_timer_end = EXCLUDED.state_timer_end, \
                 unanchors_at = EXCLUDED.unanchors_at, reinforce_hour = EXCLUDED.reinforce_hour, \
                 next_reinforce_hour = EXCLUDED.next_reinforce_hour, \
                 next_reinforce_apply = EXCLUDED.next_reinforce_apply, services = EXCLUDED.services, \
                 last_online = coalesce(EXCLUDED.last_online, structures.last_online), \
                 updated_at = now()",
            vec![Db::json(concat(bodies)), corp.into()],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id = $1 AND kind = 'upwell' AND updated_at < now()",
            vec![corp.into()],
        ),
    ];
    statements.extend(seen_and_timers(corp));
    storage::transaction(&statements).map_err(|e| retry("storing structures", e))?;
    Ok(())
}

/// After a corporation's structures (of any kind) are stored: which
/// corporation each was seen in, kept a while after it's gone
/// (notifications are relayed only for these), and the timers their
/// states show (reinforced until, anchoring until, unanchoring), unless a
/// notification gave the same one.
fn seen_and_timers(corp: i64) -> Vec<Statement> {
    vec![
        Statement::new(
            "INSERT INTO structure_owners (structure_id, corporation_id, seen_at) \
             SELECT structure_id, corporation_id, now() FROM structures WHERE corporation_id = $1 \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, seen_at = now()",
            vec![corp.into()],
        ),
        Statement::new(
            "INSERT INTO timers (structure_id, kind, at, corporation_id) \
             SELECT s.structure_id, k.kind, k.at, s.corporation_id FROM structures s \
             CROSS JOIN LATERAL (VALUES \
                 (CASE s.state WHEN 'armor_reinforce' THEN 'Armor' WHEN 'hull_reinforce' THEN 'Hull' \
                      WHEN 'anchoring' THEN 'Anchoring' WHEN 'reinforced' THEN 'Final' END, s.state_timer_end), \
                 ('Unanchoring', s.unanchors_at)) AS k(kind, at) \
             WHERE s.corporation_id = $1 AND k.kind IS NOT NULL AND k.at > now() \
               AND NOT EXISTS (SELECT 1 FROM timers t WHERE t.structure_id = s.structure_id \
                   AND t.kind = k.kind AND abs(extract(epoch FROM t.at - k.at)) < 300) \
             ON CONFLICT DO NOTHING",
            vec![corp.into()],
        ),
    ]
}

#[derive(Deserialize)]
struct System {
    system_id: i64,
    name: String,
    security_status: f64,
    region_id: i64,
    #[serde(default)]
    planets: Option<Vec<i64>>,
}

/// Security, region (and, where a customs office or skyhook is, the
/// planets) for systems with structures, once each.
fn learn_systems(budget: &mut Budget) -> Result<(), JobError> {
    let missing = storage::query(
        "SELECT DISTINCT ON (s.system_id) s.system_id, s.corporation_id FROM structures s \
         WHERE NOT EXISTS (SELECT 1 FROM systems y WHERE y.system_id = s.system_id \
             AND (y.planet_ids IS NOT NULL OR s.kind NOT IN ('customs_office', 'skyhook'))) \
         ORDER BY s.system_id LIMIT 30",
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
                    "INSERT INTO systems (system_id, name, security_status, region_id, planet_ids) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (system_id) DO UPDATE SET planet_ids = coalesce(EXCLUDED.planet_ids, systems.planet_ids)",
                    &[
                        s.system_id.into(),
                        s.name.into(),
                        s.security_status.into(),
                        s.region_id.into(),
                        s.planets.as_deref().map(id_list).into(),
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
             UNION SELECT alliance_id FROM owners WHERE alliance_id IS NOT NULL \
             UNION SELECT type_id FROM structure_items \
             UNION SELECT (f ->> 'type_id')::bigint FROM structures, \
                 jsonb_array_elements(CASE WHEN jsonb_typeof(details -> 'fuels') = 'array' \
                     THEN details -> 'fuels' ELSE '[]'::jsonb END) f \
                 WHERE kind = 'starbase') i \
         WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = i.id)",
        &[],
    )
    .map_err(|e| retry("finding names", e))?;
    let mut ids: Vec<i64> = known.rows.iter().map(|r| int(r, 0)).collect();
    let pending = storage::query(
        "SELECT text, sender_id, type FROM notifications WHERE NOT handled ORDER BY at LIMIT 500",
        &[],
    )
    .map_err(|e| retry("reading notifications", e))?;
    for row in &pending.rows {
        ids.extend(Fields::parse(&text(row, 0)).ids());
        // Only a holder of sovereignty is named by its sender (a sender
        // may be one /universe/names doesn't know, failing the batch).
        if notification::names_sender(&text(row, 2)) {
            ids.extend(opt_int(row, 1));
        }
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
    let routes = Routes::load(&settings).map_err(|e| retry("reading routes", e))?;
    let now = Utc::now();
    // Starbase notifications name the moon, customs offices' and
    // skyhooks' the planet: which of the corporation's is it.
    storage::execute(
        "UPDATE notifications n SET structure_id = s.structure_id FROM structures s \
         WHERE NOT n.handled AND n.structure_id IS NULL AND s.corporation_id = n.corporation_id \
           AND ((n.type LIKE 'Tower%' AND s.kind = 'starbase' AND s.moon_id = n.moon_id) \
             OR (n.type LIKE 'Orbital%' AND s.kind = 'customs_office' AND s.planet_id = n.planet_id) \
             OR (n.type LIKE 'Skyhook%' AND s.kind = 'skyhook' AND s.planet_id = n.planet_id))",
        &[],
    )
    .map_err(|e| retry("matching notifications", e))?;
    let rows = storage::query(
        "SELECT n.notification_id, n.type, n.at, n.text, n.structure_id, n.corporation_id, s.name, \
                so.structure_id IS NOT NULL, n.sender_id, n.structure_related, \
                (SELECT a.alliance_id FROM owners a WHERE a.corporation_id = n.corporation_id \
                     AND a.alliance_id IS NOT NULL LIMIT 1), \
                w.alliance_main, s.type_id \
         FROM notifications n \
         LEFT JOIN structure_owners so ON so.structure_id = n.structure_id AND so.corporation_id = n.corporation_id \
         LEFT JOIN structures s ON s.structure_id = n.structure_id AND s.corporation_id = n.corporation_id \
         LEFT JOIN owner_settings w ON w.corporation_id = n.corporation_id \
         WHERE NOT n.handled AND (so.structure_id IS NOT NULL OR NOT n.structure_related \
             OR n.at < now() - interval '2 hours') \
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
            // Moons and planets Structures named itself.
            ids.extend(fields.moon_id());
            // The owner, above its card.
            ids.push(int(r, 5));
            ids.extend(fields.planet_id());
            if notification::names_sender(&text(r, 1)) {
                ids.extend(opt_int(r, 8));
            }
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
        // structure list, then is dropped. One about the corporation
        // (sovereignty, wars, members) is the owner's own.
        let related = row.get(9).and_then(Db::as_bool).unwrap_or(true);
        let ours = !related || row.get(7).and_then(Db::as_bool).unwrap_or(false);
        // Alliance-wide types go only through the alliance's main owner,
        // as aa-structures' is_alliance_main.
        let alliance = opt_int(row, 10);
        let alliance_elsewhere = notification::alliance_level(&kind)
            && alliance.is_some()
            && opt_int(row, 11) != alliance;
        // Members, applications and projects: of this corporation only
        // (the host checks too).
        let other_corporation = notification::category(&kind) == Some(Category::Corp)
            && fields
                .int("corpID")
                .or_else(|| fields.int("corporation_id"))
                != Some(corp);
        let ours = ours && !alliance_elsewhere && !other_corporation;
        let sender = opt_int(row, 8).and_then(lookup);
        let mut statements = vec![Statement::new(
            "UPDATE notifications SET handled = true WHERE notification_id = $1",
            vec![id.into()],
        )];
        // A moon extraction's timer only while the setting is on.
        let timer = notification::timer(&kind, fields, at)
            .filter(|t| t.kind != notification::EXTRACTION || settings.moon_extraction_timers);
        if let (Some(timer), Some(structure)) = (timer, opt_int(row, 4))
            && ours
            && timer.at > now
        {
            let moon = fields
                .moon_id()
                .filter(|_| timer.kind == notification::EXTRACTION);
            statements.push(Statement::new(
                "INSERT INTO timers (structure_id, kind, at, corporation_id, moon_id) \
                 SELECT $1, $2, $3, $4, $5::bigint WHERE NOT EXISTS (SELECT 1 FROM timers t \
                     WHERE t.structure_id = $1 AND t.kind = $2 AND abs(extract(epoch FROM t.at - $3::timestamptz)) < 300) \
                 ON CONFLICT DO NOTHING",
                vec![
                    structure.into(),
                    timer.kind.into(),
                    Db::timestamp(rfc3339(timer.at)),
                    corp.into(),
                    moon.into(),
                ],
            ));
        }
        // An extraction cancelled: its chunk won't come (aa-structures
        // removes the started extraction's timer).
        if kind == "MoonminingExtractionCancelled"
            && ours
            && let Some(structure) = opt_int(row, 4)
        {
            statements.push(Statement::new(
                "DELETE FROM timers WHERE structure_id = $1 AND kind = $2 AND at > $3",
                vec![
                    structure.into(),
                    notification::EXTRACTION.into(),
                    Db::timestamp(rfc3339(at)),
                ],
            ));
        }
        if let (Some((structure, decloak)), Some(system)) =
            (notification::sov_timer(&kind, fields), fields.system_id())
            && ours
            && decloak > now
        {
            statements.push(Statement::new(
                "INSERT INTO sov_timers (system_id, structure, at, corporation_id, holder) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                vec![
                    system.into(),
                    structure.into(),
                    Db::timestamp(rfc3339(decloak)),
                    corp.into(),
                    sender.clone().unwrap_or_default().into(),
                ],
            ));
        }
        let category = notification::category(&kind);
        let channel = category.and_then(|c| routes.channel(corp, c));
        if let (Some(_), Some(channel)) = (category, channel)
            && ours
            && now - at <= RELAY_WITHIN
            && routes.sends(corp, &kind)
        {
            let cx = Context {
                structure: row.get(6).and_then(Db::as_text).map(str::to_owned),
                sender: sender.clone(),
                name: &lookup,
            };
            if let Some(message) = notification::message(&kind, fields, at, &cx)
                .map(|m| notification::clip(&m, MAX_MESSAGE_CHARS))
            {
                let card = card::Card {
                    kind: kind.clone(),
                    at: card::at(at),
                    corporation_id: corp,
                    corporation: lookup(corp),
                    structure: cx
                        .structure
                        .clone()
                        .or_else(|| fields.text("structureName").map(str::to_owned)),
                    type_id: fields
                        .type_id()
                        .or_else(|| fields.sov_type_id())
                        .or_else(|| opt_int(row, 12)),
                    system: fields.system_id().and_then(lookup),
                    moon: fields.moon_id().map(|_| fields.moon(&lookup)),
                };
                statements.push(Statement::new(
                    "INSERT INTO outbox (key, channel, message, mention_state, card) \
                     VALUES ($1, $2, $3, $4, $5) ON CONFLICT (key) DO NOTHING",
                    vec![
                        format!("notification:{id}").into(),
                        channel.into(),
                        message.into(),
                        routes
                            .ping(corp, notification::severity(&kind))
                            .map(str::to_owned)
                            .into(),
                        card_json(&card),
                    ],
                ));
            }
        }
        storage::transaction(&statements).map_err(|e| retry("handling a notification", e))?;
    }
    Ok(())
}

/// aa-structures' fuel alert configs, for Upwell structures (a Metenox's
/// gas counted) and starbases: an alert when a structure's fuel runs out
/// within a config's range, again every `repeat` hours while it stays
/// there (never if 0), pinging at the config's level (with the owner's
/// pings on). Out of the range (refuelled, or burnt past it), the config
/// may alert again later. Sent to the owner's fuel channel, if it sends
/// fuel alerts (the StructureFuelAlert or TowerResourceAlertMsg type); a
/// structure alerted nowhere isn't marked, so it's alerted once a channel
/// is picked.
fn fuel_alerts() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let routes = Routes::load(&settings).map_err(|e| retry("reading routes", e))?;
    storage::execute(
        "DELETE FROM fuel_alerts_sent a WHERE NOT EXISTS (SELECT 1 FROM structures s \
             JOIN fuel_alert_configs c ON c.id = a.config_id \
             WHERE s.structure_id = a.structure_id AND s.fuel_expires IS NOT NULL \
               AND s.fuel_expires <= now() + make_interval(hours => c.start_hours) \
               AND s.fuel_expires > now() + make_interval(hours => c.end_hours))",
        &[],
    )
    .map_err(|e| retry("resetting fuel alerts", e))?;
    let due = storage::query(
        "SELECT s.structure_id, s.corporation_id, c.id, c.start_hours, c.ping, s.kind, \
                coalesce(extract(epoch FROM a.sent_at)::bigint, 0) \
         FROM structures s JOIN fuel_alert_configs c ON c.enabled \
         LEFT JOIN fuel_alerts_sent a ON a.structure_id = s.structure_id AND a.config_id = c.id \
         WHERE s.kind IN ('upwell', 'starbase') AND s.fuel_expires IS NOT NULL AND s.fuel_expires > now() \
           AND s.fuel_expires <= now() + make_interval(hours => c.start_hours) \
           AND s.fuel_expires > now() + make_interval(hours => c.end_hours) \
           AND (a.sent_at IS NULL OR (c.repeat_hours > 0 \
               AND a.sent_at <= now() - make_interval(hours => c.repeat_hours))) \
         ORDER BY s.fuel_expires, c.id LIMIT 2000",
        &[],
    )
    .map_err(|e| retry("finding low fuel", e))?;
    let now = Utc::now();
    // At most this many alerts a run; pairs that go nowhere (no channel,
    // or the type not sent) don't count, so they can't crowd out the rest.
    let mut queued = 0;
    for alert in &due.rows {
        if queued >= FUEL_ALERTS_PER_RUN {
            break;
        }
        let (structure, corp, config) = (int(alert, 0), int(alert, 1), int(alert, 2));
        let kind = if text(alert, 5) == "starbase" {
            "TowerResourceAlertMsg"
        } else {
            "StructureFuelAlert"
        };
        let Some(channel) = routes.channel(corp, Category::Fuel) else {
            continue;
        };
        if !routes.sends(corp, kind) {
            continue;
        }
        let ping = match text(alert, 4).as_str() {
            "danger" => routes.ping(corp, notification::Severity::Danger),
            "warning" => routes.ping(corp, notification::Severity::Warning),
            _ => None,
        };
        let rows = storage::query(
            "SELECT s.name, coalesce(t.name, ''), coalesce(y.name, n.name, ''), s.fuel_expires, s.kind, \
                 s.gas_expires IS NOT NULL AND s.gas_expires < coalesce(s.blocks_expires, 'infinity') \
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
        let what = if row.get(5).and_then(Db::as_bool).unwrap_or(false) {
            "magmatic gas"
        } else {
            "fuel"
        };
        let message = format!(
            "Low fuel: {place} runs out of {what} in {} ({} EVE), under the {}-hour alert.",
            left(expires - now),
            expires.format("%Y-%m-%d %H:%M"),
            int(alert, 3)
        );
        storage::transaction(&[
            Statement::new(
                "INSERT INTO outbox (key, channel, message, mention_state, card) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (key) DO NOTHING",
                vec![
                    // Once per alert, low-fuel episode (the expiry) and
                    // repeat (after the last one sent).
                    format!(
                        "fuel:{structure}:{config}:{}:{}",
                        expires.timestamp(),
                        int(alert, 6)
                    )
                    .into(),
                    channel.into(),
                    message.into(),
                    ping.map(str::to_owned).into(),
                    structure_card(kind, structure, corp)?,
                ],
            ),
            Statement::new(
                "INSERT INTO fuel_alerts_sent (structure_id, config_id) VALUES ($1, $2) \
                 ON CONFLICT (structure_id, config_id) DO UPDATE SET sent_at = now()",
                vec![structure.into(), config.into()],
            ),
        ])
        .map_err(|e| retry("queuing a fuel alert", e))?;
        queued += 1;
    }
    Ok(())
}

/// A structure as messages name it: its name, type and system.
fn place_of(structure: i64) -> Result<Option<String>, JobError> {
    let rows = storage::query(
        "SELECT s.name, coalesce(t.name, ''), coalesce(y.name, n.name, '') \
         FROM structures s LEFT JOIN names t ON t.id = s.type_id \
         LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names n ON n.id = s.system_id \
         WHERE s.structure_id = $1",
        &[structure.into()],
    )
    .map_err(|e| retry("reading a structure", e))?;
    Ok(rows.rows.first().map(|row| {
        let (name, type_name, system) = (text(row, 0), text(row, 1), text(row, 2));
        let mut place = notification::escape(&name);
        if !type_name.is_empty() {
            place.push_str(&format!(" ({type_name})"));
        }
        if !system.is_empty() {
            place.push_str(&format!(" in {system}"));
        }
        place
    }))
}

/// A card's JSON for the outbox.
fn card_json(card: &card::Card) -> Db {
    Db::json(serde_json::to_string(card).unwrap_or_else(|_| "null".to_owned()))
}

/// The card for Structures' own alerts about one structure, as now.
fn structure_card(kind: &str, structure: i64, corp: i64) -> Result<Db, JobError> {
    let rows = storage::query(
        "SELECT s.name, s.type_id, coalesce(y.name, n.name), m.name, c.name \
         FROM structures s LEFT JOIN systems y ON y.system_id = s.system_id \
         LEFT JOIN names n ON n.id = s.system_id LEFT JOIN names m ON m.id = s.moon_id \
         LEFT JOIN names c ON c.id = $2 WHERE s.structure_id = $1",
        &[structure.into(), corp.into()],
    )
    .map_err(|e| retry("reading a structure", e))?;
    let row = rows.rows.first();
    let name = |i: usize| {
        row.and_then(|r| r.get(i))
            .and_then(Db::as_text)
            .map(str::to_owned)
    };
    Ok(card_json(&card::Card {
        kind: kind.to_owned(),
        at: card::at(Utc::now()),
        corporation_id: corp,
        corporation: name(4),
        structure: name(0),
        type_id: row.and_then(|r| opt_int(r, 1)),
        system: name(2),
        moon: name(3),
    }))
}

/// aa-structures' refueled notifications (StructureRefueledExtra and
/// TowerRefueledExtra): a structure burning fuel whose fuel now lasts
/// longer than when last seen, by more than half an hour (Upwell) or two
/// hours (a starbase, whose expiry is estimated), as its thresholds.
fn refuelled() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let routes = Routes::load(&settings).map_err(|e| retry("reading routes", e))?;
    let due = storage::query(
        "SELECT s.structure_id, s.corporation_id, s.kind, s.fuel_expires FROM structures s \
         WHERE s.kind IN ('upwell', 'starbase') AND s.fuel_expires > now() AND s.refuel_seen IS NOT NULL \
           AND s.fuel_expires > s.refuel_seen + CASE WHEN s.kind = 'starbase' \
               THEN interval '2 hours' ELSE interval '30 minutes' END \
           AND (s.kind = 'upwell' OR s.state IN ('online', 'reinforced')) \
         ORDER BY s.structure_id LIMIT 200",
        &[],
    )
    .map_err(|e| retry("finding refuelled structures", e))?;
    for row in &due.rows {
        let (structure, corp) = (int(row, 0), int(row, 1));
        let Some(expires) = when(row, 3) else {
            continue;
        };
        let (kind, what) = if text(row, 2) == "starbase" {
            ("TowerRefueledExtra", "Starbase refuelled")
        } else {
            ("StructureRefueledExtra", "Refuelled")
        };
        let Some(channel) = routes.channel(corp, Category::Fuel) else {
            continue;
        };
        if !routes.sends(corp, kind) {
            continue;
        }
        let Some(place) = place_of(structure)? else {
            continue;
        };
        storage::execute(
            "INSERT INTO outbox (key, channel, message, card) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (key) DO NOTHING",
            &[
                format!("refuel:{structure}:{}", expires.timestamp()).into(),
                channel.into(),
                format!(
                    "{what}: {place} was refuelled; its fuel lasts until {} EVE.",
                    expires.format("%Y-%m-%d %H:%M")
                )
                .into(),
                structure_card(kind, structure, corp)?,
            ],
        )
        .map_err(|e| retry("queuing a refuel notice", e))?;
    }
    // What's seen now is what the next run compares with (the first time
    // too, which sends nothing).
    storage::execute(
        "UPDATE structures SET refuel_seen = fuel_expires \
         WHERE fuel_expires IS NOT NULL AND refuel_seen IS DISTINCT FROM fuel_expires",
        &[],
    )
    .map_err(|e| retry("noting fuel", e))?;
    Ok(())
}

/// An Ansiblex Jump Bridge, and the liquid ozone it burns.
const JUMP_GATE: i64 = 35841;
const LIQUID_OZONE: i64 = 16273;

/// aa-structures' jump fuel alert configs: once a jump gate burning fuel
/// has less liquid ozone in its fuel bay than a config's threshold (as the
/// corporation's assets last said), an alert, until it's topped up above
/// it.
fn jump_fuel_alerts() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let routes = Routes::load(&settings).map_err(|e| retry("reading routes", e))?;
    let ozone = format!(
        "(SELECT structure_id, sum(quantity)::bigint AS quantity FROM structure_items \
          WHERE type_id = {LIQUID_OZONE} AND flag = 'StructureFuel' GROUP BY structure_id)"
    );
    storage::execute(
        &format!(
            "DELETE FROM jump_fuel_alerts_sent a USING jump_fuel_alert_configs c, {ozone} q \
             WHERE c.id = a.config_id AND q.structure_id = a.structure_id AND q.quantity >= c.threshold"
        ),
        &[],
    )
    .map_err(|e| retry("resetting jump fuel alerts", e))?;
    let due = storage::query(
        &format!(
            "SELECT s.structure_id, s.corporation_id, c.id, c.threshold, c.ping, q.quantity \
             FROM structures s JOIN {ozone} q ON q.structure_id = s.structure_id \
             JOIN jump_fuel_alert_configs c ON c.enabled \
             LEFT JOIN jump_fuel_alerts_sent a ON a.structure_id = s.structure_id AND a.config_id = c.id \
             WHERE s.kind = 'upwell' AND s.type_id = {JUMP_GATE} AND s.fuel_expires > now() \
               AND q.quantity > 0 AND q.quantity < c.threshold AND a.structure_id IS NULL \
             ORDER BY q.quantity, c.id LIMIT 200"
        ),
        &[],
    )
    .map_err(|e| retry("finding jump gates low on liquid ozone", e))?;
    for alert in &due.rows {
        let (structure, corp, config) = (int(alert, 0), int(alert, 1), int(alert, 2));
        let kind = "StructureJumpFuelAlert";
        let Some(channel) = routes.channel(corp, Category::Fuel) else {
            continue;
        };
        if !routes.sends(corp, kind) {
            continue;
        }
        let Some(place) = place_of(structure)? else {
            continue;
        };
        let ping = match text(alert, 4).as_str() {
            "danger" => routes.ping(corp, notification::Severity::Danger),
            "warning" => routes.ping(corp, notification::Severity::Warning),
            _ => None,
        };
        let (threshold, quantity) = (int(alert, 3), int(alert, 5));
        storage::transaction(&[
            Statement::new(
                "INSERT INTO outbox (key, channel, message, mention_state, card) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (key) DO NOTHING",
                vec![
                    format!("jump-fuel:{structure}:{config}:{quantity}").into(),
                    channel.into(),
                    format!(
                        "Jump gate low on liquid ozone: {place} has {quantity} units left, below \
                         the {threshold}-unit alert."
                    )
                    .into(),
                    ping.map(str::to_owned).into(),
                    structure_card(kind, structure, corp)?,
                ],
            ),
            Statement::new(
                "INSERT INTO jump_fuel_alerts_sent (structure_id, config_id) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
                vec![structure.into(), config.into()],
            ),
        ])
        .map_err(|e| retry("queuing a jump fuel alert", e))?;
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
                     coalesce(o.name, '') AS owner, s.corporation_id, coalesce(m.name, '') AS moon \
                 FROM timers t JOIN structures s ON s.structure_id = t.structure_id \
                 LEFT JOIN names tn ON tn.id = s.type_id \
                 LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
                 LEFT JOIN names o ON o.id = s.corporation_id LEFT JOIN names m ON m.id = t.moon_id \
                 WHERE t.at > now() - interval '1 day' AND t.kind <> 'Unanchoring' \
                 ORDER BY t.structure_id, t.kind, t.at DESC) latest \
             ORDER BY latest.kind = $1, 3, 1, 2 LIMIT {MAX_PUBLISHED}"
        ),
        // Moon chunks after every other timer, so routine extractions
        // never crowd a reinforcement out of the host's 500.
        &[notification::EXTRACTION.into()],
    )
    .map_err(|e| retry("publishing timers: reading them", e))?;
    let timers: Vec<tether_plugin_sdk::timers::Timer> = rows
        .rows
        .iter()
        .filter_map(|r| {
            let (structure, kind, at) = (int(r, 0), text(r, 1).to_lowercase(), when(r, 2)?);
            let extraction = text(r, 1) == notification::EXTRACTION;
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
            let title = if extraction {
                // aa-structures' "Moon Mining Cycle" timer, "Extraction
                // ready" at its moon.
                let moon = one_line(&text(r, 8), 100);
                let cycle = if moon.is_empty() {
                    "Moon Mining Cycle: extraction ready. ".to_owned()
                } else {
                    format!("Moon Mining Cycle at {moon}: extraction ready. ")
                };
                details.insert_str(0, &cycle);
                details.push_str("From its notification.");
                format!("{name}: extraction ready")
            } else {
                details.push_str("From the structure's state or its notifications.");
                format!("{name}: {kind} timer")
            };
            Some(tether_plugin_sdk::timers::Timer {
                key: format!("{structure}:{kind}"),
                title,
                at: rfc3339(at),
                system: one_line(&text(r, 5), 100),
                details,
                objective: "friendly".to_owned(),
                corporation_id: settings.timers_corporation_only.then(|| int(r, 7)),
            })
        })
        .collect();
    let mut timers = timers;
    // Sovereignty timers (a TCU or IHub reinforced), as aa-structures'
    // "Sov timer": when its command nodes decloak.
    let sov = storage::query(
        &format!(
            "SELECT t.system_id, t.structure, t.at, coalesce(y.name, n.name, ''), t.holder, t.corporation_id \
             FROM sov_timers t LEFT JOIN systems y ON y.system_id = t.system_id \
             LEFT JOIN names n ON n.id = t.system_id \
             WHERE t.at > now() - interval '1 day' ORDER BY t.at LIMIT {MAX_PUBLISHED}"
        ),
        &[],
    )
    .map_err(|e| retry("publishing timers: reading sovereignty timers", e))?;
    timers.extend(sov.rows.iter().filter_map(|r| {
        let at = when(r, 2)?;
        let (system, structure) = (one_line(&text(r, 3), 100), one_line(&text(r, 1), 20));
        let holder = one_line(&text(r, 4), 100);
        Some(tether_plugin_sdk::timers::Timer {
            key: format!("sov:{}:{structure}:{}", int(r, 0), at.timestamp()),
            title: format!("{structure} in {system}: sov timer"),
            at: rfc3339(at),
            system,
            details: if holder.is_empty() {
                "Sov timer: its command nodes decloak. From its notification.".to_owned()
            } else {
                format!("Sov timer of {holder}: its command nodes decloak. From its notification.")
            },
            objective: "friendly".to_owned(),
            corporation_id: settings.timers_corporation_only.then(|| int(r, 5)),
        })
    }));
    // Extractions last for the cut, as above; then by time.
    let extraction = |t: &tether_plugin_sdk::timers::Timer| t.key.ends_with(":extraction");
    timers.sort_by(|a, b| (extraction(a), &a.at).cmp(&(extraction(b), &b.at)));
    timers.truncate(usize::try_from(MAX_PUBLISHED).unwrap_or(500));
    timers.sort_by(|a, b| a.at.cmp(&b.at));
    tether_plugin_sdk::timers::publish(&timers).map_err(|e| retry("publishing timers", e))
}

/// Queues the relay when messages are waiting.
/// Asks again for the assets a minute on (one waiting at a time).
fn queue_assets_again() -> Result<(), JobError> {
    jobs::enqueue(
        NewJob::new(ASSETS_JOB)
            .key(ASSETS_JOB)
            .at(rfc3339(Utc::now() + Duration::minutes(1))),
    )
    .map_err(|e| retry("queuing the assets read", e))
}

/// The assets of corporations Tether was still reading in the background
/// at the last sync (due, not read since): stored once ready, with what
/// the sync works out from them (skyhooks' places, fuel and its alerts),
/// and asked again a minute on while any still isn't.
fn assets_again() -> Result<(), JobError> {
    let corporations = storage::query("SELECT DISTINCT corporation_id FROM owners ORDER BY 1", &[])
        .map_err(|e| retry("reading the owners", e))?;
    let mut budget = Budget::new(ESI_BUDGET);
    let mut waiting = false;
    for row in &corporations.rows {
        let corp = int(row, 0);
        if let Some(owner) = pick_owner(corp, Read::Assets)? {
            let outcome = orbitals::read_assets(&mut budget, corp, owner)?;
            waiting |= matches!(outcome, Outcome::Waiting);
            record(owner, Read::Assets, &outcome)?;
        }
    }
    if waiting {
        queue_assets_again()?;
    }
    learn_systems(&mut budget)?;
    orbitals::learn_planets(&mut budget)?;
    orbitals::resolve_orbitals()?;
    learn_names(&mut budget)?;
    orbitals::compute_fuel()?;
    // The alerts the assets tell (fuel bays, a jump gate's liquid ozone),
    // as a sync would.
    fuel_alerts()?;
    refuelled()?;
    jump_fuel_alerts()?;
    queue_relay(None)
}

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
///
/// A mention the host refuses because no Discord role is mapped to the
/// state is sent again without it. The state is then remembered for the
/// rest of the run, so later messages skip the refused try. A refusal on
/// the run's last send releases the message: it goes first next run, where
/// the plain send fits. It's never failed for want of a role.
fn relay() -> Result<(), JobError> {
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND created_at < now() - interval '1 day'",
        &[],
    )
    .map_err(|e| retry("expiring messages", e))?;
    let waiting = storage::query(
        "SELECT id, channel, message, coalesce(mention_state, CASE WHEN mention THEN 'Member' END), \
                card::text \
         FROM outbox WHERE sent_at IS NULL AND failed IS NULL ORDER BY id LIMIT $1",
        &[count(SENDS_PER_RUN).into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    let mut sends = 0;
    let mut later = Utc::now() + RELAY_GAP;
    // States the host found no Discord role for this run.
    let mut unmapped: Vec<String> = Vec::new();
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
        let mention = row
            .get(3)
            .and_then(Db::as_text)
            .map(str::to_owned)
            .filter(|state| !unmapped.contains(state));
        // A card around the message, when it was queued with one.
        let embed = row
            .get(4)
            .and_then(Db::as_text)
            .and_then(|c| serde_json::from_str::<card::Card>(c).ok())
            .map(|c| card::embed(&message, &c));
        let post = |mention: Mention| match &embed {
            Some(embed) => discord::send_embed(&channel, embed, mention),
            None => discord::send(&channel, &message, mention),
        };
        sends += 1;
        let mut result = post(match &mention {
            Some(state) => Mention::State(state.clone()),
            None => Mention::None,
        });
        // No role mapped to that state: send it without the mention, now
        // if a send is left, else first thing next run.
        if let Some(state) = &mention
            && matches!(result, Err(discord::Error::NotAllowed(_)))
        {
            if sends >= SENDS_PER_RUN {
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL WHERE id = $1",
                    &[id.into()],
                )
                .map_err(|e| retry("releasing a message", e))?;
                break;
            }
            sends += 1;
            result = post(Mention::None);
            // Only once the plain send went: a channel the host refuses
            // fails both, and says nothing about the role.
            if result.is_ok() {
                unmapped.push(state.clone());
            }
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

/// A starbase's state as the viewer may see it: "unanchoring" is
/// view_all_unanchoring_status's (aa-structures'); others see it online.
fn visible_state(state: &str, unanchoring: bool) -> &str {
    if state == "unanchoring" && !unanchoring {
        "online"
    } else {
        state
    }
}

/// Shield, armor and hull as EVE's rings, from the state ESI gives (it
/// gives no hit points): a structure in its armor timer has lost its
/// shield, in its hull timer its shield and armor. Reinforced, its core
/// pulses. States with no defenses to show (customs offices, skyhooks)
/// draw nothing.
fn defenses_for(state: &str) -> Value {
    let (shield, armor, alarm) = match state {
        "armor_reinforce" => (0.0, 1.0, true),
        "armor_vulnerable" => (0.0, 1.0, true),
        "hull_reinforce" => (0.0, 0.0, true),
        "hull_vulnerable" => (0.0, 0.0, true),
        "reinforced" => (0.0, 1.0, true),
        "none" | "" => return "".into(),
        _ => (1.0, 1.0, false),
    };
    defenses(shield, armor, 1.0, alarm)
}

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
        // Starbases.
        "online" => ("Online", Tone::Success),
        "onlining" => ("Onlining", Tone::Warning),
        "offline" => ("Offline", Tone::Warning),
        "reinforced" => ("Reinforced", Tone::Danger),
        "unanchoring" => ("Unanchoring", Tone::Warning),
        // Customs offices and skyhooks: ESI gives none.
        "none" => return "".into(),
        _ => ("Unknown", Tone::Neutral),
    };
    badge(label, tone).into()
}

/// What a structure is, for people.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "starbase" => "Starbase",
        "customs_office" => "Customs office",
        "skyhook" => "Orbital Skyhook",
        _ => "Upwell structure",
    }
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
        s.next_reinforce_hour, s.next_reinforce_apply, coalesce(o.name, 'Corporation ' || s.corporation_id::text), \
        s.kind, s.has_core, \
        coalesce((SELECT string_agg(g.name, ', ' ORDER BY g.sort_order, g.name) FROM structure_tags st \
            JOIN tags g ON g.id = st.tag_id WHERE st.structure_id = s.structure_id), ''), \
        coalesce(m.name, s.planet_name, pl.name, ''), s.strontium, s.details::text, s.unanchors_at, \
        s.corporation_id, coalesce(s.type_id, 0), s.last_online \
     FROM structures s \
     LEFT JOIN names t ON t.id = s.type_id \
     LEFT JOIN systems y ON y.system_id = s.system_id \
     LEFT JOIN names sn ON sn.id = s.system_id \
     LEFT JOIN names r ON r.id = y.region_id \
     LEFT JOIN names o ON o.id = s.corporation_id \
     LEFT JOIN names m ON m.id = s.moon_id \
     LEFT JOIN names pl ON pl.id = s.planet_id";

fn system_text(row: &[Db]) -> String {
    match row.get(4).and_then(Db::as_float) {
        Some(sec) => format!("{} ({sec:.1})", text(row, 3)),
        None => text(row, 3),
    }
}

fn name_link(row: &[Db]) -> Value {
    link(text(row, 1), format!("structure/{}", int(row, 0))).into()
}

/// After this long without a service online, an Upwell structure out of
/// fuel is Abandoned (aa-structures').
const ABANDONED_AFTER: Duration = Duration::days(7);

/// aa-structures' power modes (`models/structures_1.py:527-545`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Power {
    Full,
    Low,
    Abandoned,
    /// "Abandoned?": out of fuel, and never seen with a service online.
    MaybeAbandoned,
}

impl Power {
    fn badge(self) -> Value {
        match self {
            Power::Full => badge("Full power", Tone::Success),
            Power::Low => badge("Low power", Tone::Warning),
            Power::Abandoned => badge("Abandoned", Tone::Danger),
            Power::MaybeAbandoned => badge("Abandoned?", Tone::Warning),
        }
        .into()
    }
}

/// An Upwell structure's power mode: full power while its fuel lasts;
/// out of fuel, low power for 7 days after a service was last seen
/// online, then abandoned; never seen online, low power while anchoring,
/// else "Abandoned?".
fn power_mode(
    fuel_expires: Option<DateTime<Utc>>,
    last_online: Option<DateTime<Utc>>,
    state: &str,
    now: DateTime<Utc>,
) -> Power {
    if fuel_expires.is_some_and(|t| t > now) {
        return Power::Full;
    }
    match last_online {
        Some(at) if at >= now - ABANDONED_AFTER => Power::Low,
        Some(_) => Power::Abandoned,
        None if matches!(state, "anchoring" | "anchor_vulnerable") => Power::Low,
        None => Power::MaybeAbandoned,
    }
}

/// Fuel expiry and time left, toned by the first alert; out of fuel, an
/// Upwell structure's power mode.
fn fuel_cells(row: &[Db], now: DateTime<Utc>, alert: i64) -> (Value, Value) {
    let upwell = text(row, 14) == "upwell";
    let fuel = when(row, 6);
    if upwell && fuel.is_none_or(|t| t <= now) {
        let mode = power_mode(fuel, when(row, 23), &text(row, 8), now);
        let expires = fuel.map_or_else(|| "".into(), |t| time(rfc3339(t)));
        return (expires, mode.badge());
    }
    match fuel {
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
        None => ("".into(), "Unknown".into()),
    }
}

/// A structure's row. `unanchoring`: the viewer holds
/// view_all_unanchoring_status (aa-structures' Unanchoring until).
fn structure_row(row: &[Db], now: DateTime<Utc>, alert: i64, unanchoring: bool) -> Vec<Value> {
    let (expires, remaining) = fuel_cells(row, now, alert);
    let reinforce = match (opt_int(row, 10), opt_int(row, 11), when(row, 12)) {
        (Some(h), Some(next), Some(from)) => format!(
            "{h:02}:00 ({next:02}:00 from {})",
            from.format("%Y-%m-%d %H:%M")
        ),
        (Some(h), _, _) => format!("{h:02}:00"),
        _ => String::new(),
    };
    let upwell = text(row, 14) == "upwell";
    vec![
        defenses_for(&text(row, 8)),
        owner_value(int(row, 21), text(row, 13)),
        name_link(row),
        typed(int(row, 22), text(row, 2)),
        system_text(row).into(),
        text(row, 5).into(),
        expires,
        remaining,
        if upwell {
            services_text(&text(row, 7)).into()
        } else {
            "".into()
        },
        match when(row, 20).filter(|_| unanchoring) {
            Some(_) => badge("Unanchoring", Tone::Warning).into(),
            None => state_badge(&text(row, 8)),
        },
        when(row, 9)
            .or_else(|| when(row, 20).filter(|_| unanchoring))
            .map_or_else(|| "".into(), |t| upcoming(t, now)),
        reinforce.into(),
        if upwell {
            detail::core_badge(row.get(15).and_then(Db::as_bool))
        } else {
            "".into()
        },
        text(row, 16).into(),
    ]
}

fn structure_table(title: &str, empty: &str, rows: Vec<Vec<Value>>) -> Table {
    with_rows(
        Table::new(vec![
            Column::text(""),
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
            Column::text("Core"),
            Column::text("Tags"),
        ])
        .title(title)
        .empty(empty),
        rows,
    )
}

fn starbase_row(row: &[Db], now: DateTime<Utc>, alert: i64, unanchoring: bool) -> Vec<Value> {
    let (expires, remaining) = fuel_cells(row, now, alert);
    // Reinforced until, or (with view_all_unanchoring_status) unanchoring at.
    let timer = when(row, 9).or_else(|| when(row, 20).filter(|_| unanchoring));
    vec![
        defenses_for(visible_state(&text(row, 8), unanchoring)),
        owner_value(int(row, 21), text(row, 13)),
        name_link(row),
        typed(int(row, 22), text(row, 2)),
        system_text(row).into(),
        text(row, 17).into(),
        expires,
        remaining,
        opt_int(row, 18).map_or_else(|| "".into(), Value::from),
        state_badge(visible_state(&text(row, 8), unanchoring)),
        timer.map_or_else(|| "".into(), |t| upcoming(t, now)),
        text(row, 16).into(),
    ]
}

fn starbase_table(rows: Vec<Vec<Value>>) -> Table {
    with_rows(
        Table::new(vec![
            Column::text(""),
            Column::text("Owner"),
            Column::text("Name"),
            Column::text("Type"),
            Column::text("System"),
            Column::text("Moon"),
            Column::numeric("Fuel expires"),
            Column::text("Fuel left"),
            Column::numeric("Strontium"),
            Column::text("State"),
            Column::numeric("State timer"),
            Column::text("Tags"),
        ])
        .title("Starbases")
        .empty("No starbases. They're read with a data source's Director role."),
        rows,
    )
}

fn rate(details: &serde_json::Value, key: &str) -> Value {
    details[key]
        .as_f64()
        .map_or_else(|| "".into(), |v| format!("{:.1}%", v * 100.0).into())
}

fn orbital_row(row: &[Db]) -> Vec<Value> {
    let details: serde_json::Value = serde_json::from_str(&text(row, 19)).unwrap_or_default();
    let window = match (
        details["reinforce_exit_start"].as_i64(),
        details["reinforce_exit_end"].as_i64(),
    ) {
        (Some(a), Some(b)) => format!("{a:02}:00 to {b:02}:00"),
        _ => String::new(),
    };
    vec![
        owner_value(int(row, 21), text(row, 13)),
        name_link(row),
        typed(int(row, 22), text(row, 2)),
        system_text(row).into(),
        text(row, 5).into(),
        text(row, 17).into(),
        window.into(),
        rate(&details, "corporation_tax_rate"),
        rate(&details, "alliance_tax_rate"),
        text(row, 16).into(),
    ]
}

fn orbital_table(rows: Vec<Vec<Value>>) -> Table {
    with_rows(
        Table::new(vec![
            Column::text("Owner"),
            Column::text("Name"),
            Column::text("Type"),
            Column::text("System"),
            Column::text("Region"),
            Column::text("Planet"),
            Column::text("Reinforcement exit"),
            Column::numeric("Corporation tax"),
            Column::numeric("Alliance tax"),
            Column::text("Tags"),
        ])
        .title("Customs offices and skyhooks")
        .empty("No customs offices or skyhooks. They're read with a data source's Director role."),
        rows,
    )
}

/// aa-structures' Jump gates tab: the Ansiblex gates the viewer may see
/// (`gates`, of `total`), each with the liquid ozone in its fuel bay as the
/// corporation's assets last said: none read yet shows nothing, an empty
/// bay 0. The ozone needs no `view_structure_fit`, as in aa-structures'
/// list (its note says so).
fn jump_gate_tab(
    gates: &storage::Rows,
    total: i64,
    now: DateTime<Utc>,
    alert: i64,
    unanchoring: bool,
) -> Result<Vec<Section>, PageError> {
    let ids: Vec<i64> = gates.rows.iter().map(|r| int(r, 0)).collect();
    let ozone = storage::query(
        &format!(
            "SELECT s.structure_id, s.fuel_read_at IS NOT NULL, \
                 coalesce((SELECT sum(i.quantity) FROM structure_items i \
                     WHERE i.structure_id = s.structure_id AND i.type_id = {LIQUID_OZONE} \
                       AND i.flag = 'StructureFuel'), 0)::bigint \
             FROM structures s WHERE s.structure_id = ANY(string_to_array($1, ',')::bigint[])"
        ),
        &[id_list(&ids).into()],
    )
    .map_err(|e| failed("reading liquid ozone", e))?;
    let rows = gates.rows.iter().map(|r| {
        let id = int(r, 0);
        let units: Value = ozone
            .rows
            .iter()
            .find(|o| int(o, 0) == id)
            .filter(|o| o.get(1).and_then(Db::as_bool).unwrap_or(false))
            .map_or_else(|| "".into(), |o| int(o, 2).into());
        // A structure's row (its state and timer seen as for any other),
        // but the type (every one an Ansiblex), services, reinforce hour
        // and core; the ozone after its fuel.
        let mut cells: Vec<Value> = structure_row(r, now, alert, unanchoring)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| ![3, 8, 11, 12].contains(i))
            .map(|(_, cell)| cell)
            .collect();
        cells.insert(7, units);
        cells
    });
    let table = with_rows(
        Table::new(vec![
            Column::text(""),
            Column::text("Owner"),
            Column::text("Name"),
            Column::text("System"),
            Column::text("Region"),
            Column::numeric("Fuel expires"),
            Column::text("Fuel left"),
            Column::numeric("Liquid ozone"),
            Column::text("State"),
            Column::numeric("State timer"),
            Column::text("Tags"),
        ])
        .title("Jump gates")
        .empty("No jump gates."),
        rows,
    );
    let mut sections = vec![Section::Table(table)];
    sections.extend(cut(GATE_ROWS, total));
    sections.push(Section::Text(
        "Liquid ozone in each gate's fuel bay, as its corporation's assets last said. They're \
         read with a data source's Director role: until then the figure is empty."
            .to_owned(),
    ));
    Ok(sections)
}

/// A tab cut at `shown` rows of `total` says so (aa-structures pages
/// through every row, `views/structures.py:188-195`), and how to see the
/// rest: an owner's page or tags narrow the list.
fn cut(shown: i64, total: i64) -> Option<Section> {
    (total > shown).then(|| {
        Section::Text(format!(
            "Showing the first {shown} of {total}: pick an owner on the Owners tab, or tags, to \
             see theirs."
        ))
    })
}

/// A tab's sections: its table, then a line if it was cut.
fn tab_of(table: Table, shown: i64, total: i64) -> Vec<Section> {
    let mut sections = vec![Section::Table(table)];
    sections.extend(cut(shown, total));
    sections
}

/// What the list shows: an owner's, or those with any of some tags.
#[derive(Default)]
struct Filter {
    owner: Option<i64>,
    tags: Option<Vec<i64>>,
}

const LOW_FUEL: &str = "((s.kind = 'upwell' AND (s.fuel_expires IS NULL OR s.fuel_expires <= now() + make_interval(hours => $6::integer))) \
     OR (s.kind = 'starbase' AND s.fuel_expires <= now() + make_interval(hours => $6::integer)))";
const REINFORCED: &str = "s.state IN ('armor_reinforce', 'armor_vulnerable', 'hull_reinforce', 'hull_vulnerable', 'reinforced')";
/// Ansiblex jump gates ([`JUMP_GATE`]).
const JUMP_GATES: &str = "(s.kind = 'upwell' AND s.type_id = 35841)";

fn list_page(viewer: &Viewer, filter: Filter) -> Result<Page, PageError> {
    let Some(visible) = visibility(viewer) else {
        return Ok(Page::new("Structures").text(
            "You can open Structures but may not see any structures. An admin grants \
             view_corporation_structures, view_alliance_structures or view_all_structures.",
        ));
    };
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let alert = settings.low_fuel_hours;
    // aa-structures shows unanchoring only with this permission.
    let unanchoring = viewer.can("view_all_unanchoring_status");
    let all_tags = tags::all_visible(&visible).map_err(|e| failed("reading tags", e))?;
    let default_filter = settings.default_tags_filter && filter.tags.is_none();
    let mut params = visible.clone();
    params.push(filter.owner.into());
    params.push(id_list(filter.tags.as_deref().unwrap_or_default()).into());
    params.push(alert.into());
    params.push(default_filter.into());
    let now = Utc::now();
    let scope = format!(
        "WHERE {VISIBLE} AND ($4::bigint IS NULL OR s.corporation_id = $4) \
           AND ($5::text = '' OR EXISTS (SELECT 1 FROM structure_tags g WHERE g.structure_id = s.structure_id \
               AND g.tag_id = ANY(string_to_array($5, ',')::integer[]))) \
           AND (NOT $7::boolean OR EXISTS (SELECT 1 FROM structure_tags g JOIN tags d ON d.id = g.tag_id \
               WHERE g.structure_id = s.structure_id AND d.is_default)) \
           AND $6::integer > 0"
    );
    let owner_name = match filter.owner {
        Some(corp) => {
            // Only an owner the viewer may see.
            let mut p = visible.clone();
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
    let query = |condition: &str, order: &str, limit: i64| {
        let mut p = params.clone();
        p.push(limit.into());
        storage::query(
            &format!("{STRUCTURE_ROW} {scope} AND {condition} ORDER BY {order} LIMIT $8"),
            &p,
        )
        .map_err(|e| failed("reading structures", e))
    };
    let upwell = query("s.kind = 'upwell'", "o.name, s.name", LIST_ROWS)?;
    let starbases = query("s.kind = 'starbase'", "o.name, s.name", STARBASE_ROWS)?;
    let orbitals = query(
        "s.kind IN ('customs_office', 'skyhook')",
        "o.name, s.name",
        ORBITAL_ROWS,
    )?;
    let low = query(LOW_FUEL, "s.fuel_expires NULLS FIRST", SHORT_ROWS)?;
    let reinforced = query(REINFORCED, "s.state_timer_end NULLS LAST", SHORT_ROWS)?;
    let gates = if settings.show_jump_gates {
        Some(query(JUMP_GATES, "o.name, s.name", GATE_ROWS)?)
    } else {
        None
    };
    let counts = storage::query(
        &format!(
            "SELECT count(*) FILTER (WHERE s.kind = 'upwell'), \
                 count(*) FILTER (WHERE s.kind = 'starbase'), \
                 count(*) FILTER (WHERE s.kind IN ('customs_office', 'skyhook')), \
                 count(*) FILTER (WHERE {LOW_FUEL}), count(*) FILTER (WHERE {REINFORCED}), \
                 count(*) FILTER (WHERE {JUMP_GATES}) \
             FROM structures s {scope}"
        ),
        &params,
    )
    .map_err(|e| failed("counting structures", e))?;
    // Structures' timers first, then moon extractions in what room is left,
    // so routine chunks never crowd out a reinforcement.
    let timers_of = |extractions: bool, limit: i64| {
        storage::query(
            &format!(
                "SELECT t.kind, t.at, s.name, coalesce(y.name, sn.name, ''), coalesce(o.name, ''), \
                     s.corporation_id \
                 FROM timers t JOIN structures s ON s.structure_id = t.structure_id \
                 LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
                 LEFT JOIN names o ON o.id = s.corporation_id \
                 {scope} AND t.at > now() AND ($8 OR t.kind <> 'Unanchoring') \
                   AND (t.kind = 'Extraction') = $9 \
                 ORDER BY t.at LIMIT {limit}"
            ),
            &[params.clone(), vec![unanchoring.into(), extractions.into()]].concat(),
        )
        .map_err(|e| failed("reading timers", e))
    };
    let timer_count = storage::query(
        &format!(
            "SELECT count(*) FROM timers t JOIN structures s ON s.structure_id = t.structure_id \
             {scope} AND t.at > now() AND ($8 OR t.kind <> 'Unanchoring')"
        ),
        &[params.clone(), vec![unanchoring.into()]].concat(),
    )
    .map_err(|e| failed("counting timers", e))?
    .rows
    .first()
    .map_or(0, |r| int(r, 0));
    let structure_timers = timers_of(false, TIMER_ROWS)?;
    let extractions = timers_of(true, TIMER_ROWS - count(structure_timers.rows.len()))?;
    let mut timers: Vec<&Vec<Db>> = structure_timers
        .rows
        .iter()
        .chain(&extractions.rows)
        .collect();
    timers.sort_by_key(|r| when(r, 1));
    let owners = storage::query(
        &format!(
            "SELECT o.corporation_id, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
                 coalesce(a.name, ''), \
                 (SELECT count(*) FROM structures s WHERE s.corporation_id = o.corporation_id), \
                 max(o.structures_at), bool_and(o.structures_retry_at > now()), \
                 coalesce(max(o.alliance_id), 0) \
             FROM owners o LEFT JOIN names n ON n.id = o.corporation_id LEFT JOIN names a ON a.id = o.alliance_id \
             WHERE {} \
             GROUP BY o.corporation_id, n.name, a.name ORDER BY 2 LIMIT {OWNER_ROWS}",
            VISIBLE.replace("s.corporation_id", "o.corporation_id")
        ),
        &visible,
    )
    .map_err(|e| failed("reading owners", e))?;
    let owner_count = storage::query(
        &format!(
            "SELECT count(DISTINCT o.corporation_id) FROM owners o WHERE {}",
            VISIBLE.replace("s.corporation_id", "o.corporation_id")
        ),
        &visible,
    )
    .map_err(|e| failed("counting owners", e))?
    .rows
    .first()
    .map_or(0, |r| int(r, 0));

    let row = counts.rows.first();
    let count_of = |i: usize| row.map_or(0, |r| int(r, i));
    let (total, starbase_count, orbital_count) = (count_of(0), count_of(1), count_of(2));
    let (low_count, reinforced_count) = (count_of(3), count_of(4));
    // A structure's next timer: a moon chunk isn't one.
    let next = structure_timers.rows.first().and_then(|r| when(r, 1));
    let rows_of = |rows: &storage::Rows| -> Vec<Vec<Value>> {
        rows.rows
            .iter()
            .map(|r| structure_row(r, now, alert, unanchoring))
            .collect()
    };
    let mut list = Vec::new();
    if default_filter {
        list.push(Section::Text(
            "Showing structures with a default tag: pick tags on the Tags tab to see others."
                .to_owned(),
        ));
    }
    list.push(Section::Table(structure_table(
        "Structures",
        "No structures yet. Owners' structures appear within the hour.",
        rows_of(&upwell),
    )));
    list.extend(cut(LIST_ROWS, total));
    let timer_table = with_rows(
        Table::new(vec![
            Column::numeric("When (EVE)"),
            Column::numeric("Remaining"),
            Column::text("Timer"),
            Column::text("Structure"),
            Column::text("System"),
            Column::text("Owner"),
        ])
        .title("Upcoming timers")
        .empty("No upcoming timers."),
        timers.iter().map(|r| {
            let kind = text(r, 0);
            vec![
                when(r, 1).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                when(r, 1).map_or_else(|| "".into(), |t| countdown(rfc3339(t))),
                if kind == notification::EXTRACTION {
                    "Extraction (chunk ready)".into()
                } else {
                    kind.into()
                },
                text(r, 2).into(),
                text(r, 3).into(),
                owner_value(int(r, 5), text(r, 4)),
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
            let alliance_cell: Value = match int(r, 6) {
                id if id > 0 && !text(r, 2).is_empty() => alliance(id, text(r, 2)).into(),
                _ => text(r, 2).into(),
            };
            vec![
                link(text(r, 1), format!("owner/{}", int(r, 0))).into(),
                alliance_cell,
                int(r, 3).into(),
                when(r, 4).map_or_else(|| "".into(), |t| time(rfc3339(t))),
                status.into(),
            ]
        }),
    );
    let tag_names: Vec<String> = filter
        .tags
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| {
            all_tags
                .iter()
                .find(|t| t.id == *id)
                .map(|t| t.name.clone())
        })
        .collect();
    let title = match (&owner_name, tag_names.is_empty()) {
        (Some(name), _) => format!("Structures: {name}"),
        (None, false) => format!("Structures tagged {}", tag_names.join(" or ")),
        (None, true) => "Structures".to_owned(),
    };
    let mut page = Page::new(title)
        .description("Owners' Upwell structures, starbases, customs offices and skyhooks, from ESI hourly; timers and alerts from their notifications")
        .stats(vec![
            Stat::new("Structures", total),
            Stat::new("Starbases", starbase_count),
            Stat::new("Orbitals", orbital_count).caption("customs offices and skyhooks"),
            Stat::new("Low fuel", low_count).caption(format!("under {alert} hours, or low power")),
            Stat::new("Reinforced", reinforced_count),
            match next {
                Some(t) => Stat::new("Next timer", countdown(rfc3339(t)))
                    .caption(format!("{} EVE", t.format("%Y-%m-%d %H:%M"))),
                None => Stat::new("Next timer", "none"),
            },
        ])
        .tab("Structures", list)
        .tab(
            "Low fuel",
            tab_of(
                structure_table(
                    &format!("Under {alert} hours of fuel, or low power"),
                    "No structure is low on fuel.",
                    rows_of(&low),
                ),
                SHORT_ROWS,
                low_count,
            ),
        )
        .tab(
            "Reinforced",
            tab_of(
                structure_table(
                    "Reinforced or vulnerable",
                    "No structure is reinforced.",
                    rows_of(&reinforced),
                ),
                SHORT_ROWS,
                reinforced_count,
            ),
        )
        .tab("Timers", {
            let mut sections = tab_of(timer_table, TIMER_ROWS, timer_count);
            sections.push(Section::Text(
                "From structures' states and their notifications, and moon extractions' \
                 chunks if the settings say so. Structure Timers shows them too, as \
                 automatic timers (corporation-only if the settings say so)."
                    .to_owned(),
            ));
            sections
        })
        .tab(
            "Starbases",
            tab_of(
                starbase_table(
                    starbases
                        .rows
                        .iter()
                        .map(|r| starbase_row(r, now, alert, unanchoring))
                        .collect(),
                ),
                STARBASE_ROWS,
                starbase_count,
            ),
        )
        .tab(
            "Orbitals",
            tab_of(
                orbital_table(orbitals.rows.iter().map(|r| orbital_row(r)).collect()),
                ORBITAL_ROWS,
                orbital_count,
            ),
        );
    if let Some(gates) = &gates {
        page = page.tab(
            "Jump gates",
            jump_gate_tab(gates, count_of(5), now, alert, unanchoring)?,
        );
    }
    page = page.tab("Tags", vec![Section::Table(tags::tag_table(&all_tags))]);
    // The tag filter (aa-structures' "Filter by tag"): any of those
    // chosen, on the whole list (an owner's page has its own owner).
    if filter.owner.is_none() && !all_tags.is_empty() {
        page = page.toolbar(
            Toolbar::new().filter_any(
                "tag",
                "Tag",
                all_tags
                    .iter()
                    .take(100)
                    .map(|t| (t.id.to_string(), t.name.clone()))
                    .collect(),
            ),
        );
    }
    if filter.owner.is_none() && filter.tags.is_none() {
        let mut sections = vec![Section::Table(owner_table)];
        if owner_count > OWNER_ROWS {
            sections.push(Section::Text(format!(
                "Showing the first {OWNER_ROWS} of {owner_count} owners, by name."
            )));
        }
        page = page.tab(
            "Owners",
            [
                sections,
                vec![Section::Text(
                    "Add data source logs in with a character with the in-game Station Manager role. \
                     Its corporation's structures show \
                     here within the hour; starbases, customs offices, skyhooks and fittings need \
                     the Director role."
                        .to_owned(),
                )],
            ]
            .concat(),
        );
    }
    Ok(page)
}

/// aa-structures' public customs office list: every customs office of
/// owners that made theirs public, with whether the viewer's main may use
/// it and at what tax (by corporation and alliance; standings aren't
/// known to Tether).
fn pocos_page(viewer: &Viewer) -> Result<Page, PageError> {
    let rows = storage::query(
        &format!(
        "SELECT coalesce(o.name, 'Corporation ' || s.corporation_id::text), \
             coalesce(y.name, sn.name, 'System ' || s.system_id::text), y.security_status, \
             coalesce(r.name, ''), coalesce(s.planet_name, p.name, ''), s.details::text, s.corporation_id, \
             (SELECT a.alliance_id FROM owners a WHERE a.corporation_id = s.corporation_id LIMIT 1) \
         FROM structures s JOIN owner_settings w ON w.corporation_id = s.corporation_id AND w.pocos_public \
         LEFT JOIN names o ON o.id = s.corporation_id \
         LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
         LEFT JOIN names r ON r.id = y.region_id LEFT JOIN names p ON p.id = s.planet_id \
         WHERE s.kind = 'customs_office' ORDER BY 4, 2, 5 LIMIT {POCO_ROWS}"
        ),
        &[],
    )
    .map_err(|e| failed("reading customs offices", e))?;
    let total = storage::query(
        "SELECT count(*) FROM structures s \
         JOIN owner_settings w ON w.corporation_id = s.corporation_id AND w.pocos_public \
         WHERE s.kind = 'customs_office'",
        &[],
    )
    .map_err(|e| failed("counting customs offices", e))?
    .rows
    .first()
    .map_or(0, |r| int(r, 0));
    let (corp, alliance) = (viewer.main.corporation_id, viewer.main.alliance_id);
    let table = with_rows(
        Table::new(vec![
            Column::text("Owner"),
            Column::text("System"),
            Column::text("Region"),
            Column::text("Planet"),
            Column::text("Your access"),
            Column::numeric("Your tax"),
        ])
        .title("Customs offices")
        .empty("No owner has made its customs offices public."),
        rows.rows.iter().map(|r| {
            let details: serde_json::Value = serde_json::from_str(&text(r, 5)).unwrap_or_default();
            let (owner_corp, owner_alliance) = (int(r, 6), opt_int(r, 7));
            let (access, tax): (Value, Value) = if owner_corp == corp {
                (
                    badge("Yes", Tone::Success).into(),
                    rate(&details, "corporation_tax_rate"),
                )
            } else if alliance.is_some()
                && alliance == owner_alliance
                && details["allow_alliance_access"].as_bool().unwrap_or(false)
            {
                (
                    badge("Yes", Tone::Success).into(),
                    rate(&details, "alliance_tax_rate"),
                )
            } else if neutrals_admitted(&details) {
                // As aa-structures: the neutral rate, not confident (the
                // owner's standings towards the pilot aren't known).
                (
                    badge("Yes (?)", Tone::Warning).into(),
                    rate(&details, "neutral_standing_tax_rate"),
                )
            } else {
                (badge("No", Tone::Danger).into(), "".into())
            };
            let system = match r.get(2).and_then(Db::as_float) {
                Some(sec) => format!("{} ({sec:.1})", text(r, 1)),
                None => text(r, 1),
            };
            vec![
                owner_value(owner_corp, text(r, 0)),
                system.into(),
                text(r, 3).into(),
                text(r, 4).into(),
                access,
                tax,
            ]
        }),
    );
    let mut page = Page::new("Customs offices")
        .description("Customs offices their owners opened to everyone who may open Structures")
        .table(table);
    if total > POCO_ROWS {
        page = page.text(format!(
            "Showing the first {POCO_ROWS} of {total}, by region and system."
        ));
    }
    Ok(page.text(
        "Access and tax for your main's corporation and alliance. As aa-structures, access \
             and tax for pilots in neither (\"Yes (?)\": the neutral standing rate) may not be \
             accurate: they depend on the owner's standings towards you, which Tether doesn't \
             know.",
    ))
}

/// Whether a customs office lets in pilots of neither its corporation nor
/// its alliance, as aa-structures judges it: access with standings on, the
/// standing it asks for at most neutral, and a neutral rate set.
fn neutrals_admitted(details: &serde_json::Value) -> bool {
    details["allow_access_with_standings"]
        .as_bool()
        .unwrap_or(false)
        // A level ESI doesn't say (or one unknown) is aa-structures' "none",
        // the lowest.
        && !matches!(
            details["standing_level"].as_str(),
            Some("good" | "excellent")
        )
        && details["neutral_standing_tax_rate"].as_f64().is_some()
}

fn channel_field(name: &str, label: &str, help: &str, value: Option<&str>) -> Field {
    let mut options: Vec<(String, String)> = vec![(String::new(), "Not sent".to_owned())];
    options.extend(
        discord::channels()
            .into_iter()
            .map(|c| (c.id, format!("#{}", c.name))),
    );
    // A channel no longer assigned starts on "Not sent": a select can't
    // start on a value it doesn't list.
    let value = value
        .filter(|v| options.iter().any(|(id, _)| id == v))
        .unwrap_or_default()
        .to_owned();
    Field::select(name, label, options).value(value).help(help)
}

/// Whether the default types (none: every type) tick `kind`.
fn ticked(types: Option<&Vec<String>>, kind: &str) -> bool {
    types.is_none_or(|t| t.iter().any(|k| k == kind))
}

/// A kind's channel help, saying so when the default types tick none of
/// its types (as aa-structures' defaults leave moons, wars and members):
/// owners on the defaults send nothing there until one is ticked.
fn channel_help(category: Category, help: &str, types: Option<&Vec<String>>) -> String {
    let any = notification::TYPES
        .iter()
        .any(|(kind, _, _, c)| *c == category && ticked(types, kind));
    if any {
        help.to_owned()
    } else {
        format!(
            "{help} None of these types is ticked under Types: {} below, so owners on the \
             default types send nothing here until one is.",
            category.label()
        )
    }
}

fn settings_page(problem: Option<&str>) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let owners = storage::query(
        "SELECT o.character_id, o.character_name, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
             o.structures_at, o.notifications_at, o.last_error, \
             greatest(o.structures_retry_at, o.notifications_retry_at), o.corporation_id \
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
    let mut discord = SettingsGroup::new("Discord").description(
        "Each kind of notification goes to one of the channels an admin assigned Structures \
         (Admin → Apps), within a day of it happening, once. These are the defaults: an owner \
         can have its own (Owners' Discord routing below), as aa-structures' webhooks per \
         owner. Notifications and alerts go to these channels whatever the view permissions: \
         anyone who can read a channel sees the structures named in it.",
    );
    for (category, help) in [
        (
            Category::Attack,
            "Under attack, lost shields or armor (with the timer), destroyed.",
        ),
        (
            Category::Fuel,
            "EVE's fuel alerts, services offline, low power, refuelled structures, and the fuel \
             and jump fuel alerts below.",
        ),
        (
            Category::State,
            "Online, high power, anchoring and unanchoring, ownership transferred, reinforcement \
             hour changed.",
        ),
        (
            Category::Moon,
            "Extractions started, chunks arrived, fractures and cancellations.",
        ),
        (
            Category::Sov,
            "Sovereignty structures reinforced, destroyed or captured, claims, anchoring in \
             alliance space, and bills. Alliance-wide ones go only through the alliance main owner.",
        ),
        (
            Category::War,
            "Wars declared, allies, surrenders, CONCORD retracting them, war eligibility.",
        ),
        (
            Category::Corp,
            "Applications, members joining and leaving, corporation projects.",
        ),
    ] {
        discord = discord.field(channel_field(
            &format!("{}_channel", category.name()),
            category.label(),
            &channel_help(category, help, settings.notification_types.as_ref()),
            settings.channel(category),
        ));
    }
    let pings = SettingsGroup::new("Pings")
        .field(
            Field::checkbox("default_pings", "Default pings", settings.default_pings).help(
                "aa-structures' default pings (on in a fresh install): danger notifications ping \
                 @everyone and warnings @here. Tether's bot mentions the Discord roles of these \
                 states instead, once a role is mapped to them on Administration's Discord page. \
                 Until then messages go without a mention.",
            ),
        )
        .field(
            Field::text("danger_ping", "Danger pings mention the role of state", 64)
                .value(settings.danger_ping.clone().unwrap_or_default())
                .help("aa-structures' @everyone. Member by default. Empty: no mention."),
        )
        .field(
            Field::text(
                "warning_ping",
                "Warning pings mention the role of state",
                64,
            )
            .value(settings.warning_ping.clone().unwrap_or_default())
            .help(
                "aa-structures' @here. Member in a fresh install (a role mention reaches its \
                 offline members too). Empty: no mention.",
            ),
        );
    let shown = SettingsGroup::new("Timers and the list")
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
        )
        .field(
            Field::checkbox(
                "moon_extraction_timers",
                "Moon extraction timers",
                settings.moon_extraction_timers,
            )
            .help(
                "A moon extraction started gives Structure Timers a timer for when its chunk is \
                 ready, and cancelling it removes the timer. The timer names the drill, moon, \
                 system and owner, and everyone who may see Structure Timers sees it unless \
                 timers are corporation-only (above). Off: none, and those made are removed.",
            ),
        )
        .field(
            Field::checkbox(
                "default_tags_filter",
                "Show structures with a default tag",
                settings.default_tags_filter,
            )
            .help("The list shows only structures with a default tag until tags are picked."),
        )
        .field(
            Field::checkbox(
                "show_jump_gates",
                "Show the Jump gates tab",
                settings.show_jump_gates,
            )
            .help(
                "Ansiblex jump gates with their liquid ozone, on their own tab. Everyone who \
                 may see a gate sees its ozone, without the fitting permission.",
            ),
        );
    let admin = SettingsGroup::new("Admin notifications").field(
        Field::checkbox(
            "admin_notifications",
            "Notify admins",
            settings.admin_notifications,
        )
        .help(
            "Superusers and holders of manage get a notice in Tether's notifications when a \
             data source is added, and when an owner's reads stop (structures for 2 hours, \
             notifications for 40 minutes, assets once read for 2 hours), come back, or first \
             work. An owner can be left out on its own page.",
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
            owner_value(int(r, 7), text(r, 2)),
            character(int(r, 0), text(r, 1)).into(),
            when(r, 3).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            when(r, 4).map_or_else(|| "".into(), |t| time(rfc3339(t))),
            status.into(),
            text(r, 5).into(),
            backing_off.map_or_else(|| "".into(), |t| time(rfc3339(t))),
            // An owner ESI refused (a lost role or token) is left alone for
            // an hour, doubling up to a day: once it's fixed in game, a
            // manager retries it now.
            action("Retry now", "retry")
                .field("owner", int(r, 0).to_string())
                .into(),
        ]
    });
    let owner_table = with_rows(
        Table::new(vec![
            Column::text("Corporation"),
            Column::text("Character"),
            Column::numeric("Structures last read"),
            Column::numeric("Notifications last read"),
            Column::text("Status"),
            Column::text("Last problem"),
            Column::numeric("Next try"),
            Column::text(""),
        ])
        .title("Data sources")
        .empty("No data sources yet: Add data source logs in with a Station Manager."),
        owner_rows,
    );
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
    let routing = storage::query(
        "SELECT o.corporation_id, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
             (SELECT count(*) FROM owner_channels c WHERE c.corporation_id = o.corporation_id), \
             coalesce(w.mention, 'default'), coalesce(w.pocos_public, false), \
             cardinality(w.notification_types), \
             coalesce(w.alliance_main = (SELECT a.alliance_id FROM owners a \
                 WHERE a.corporation_id = o.corporation_id AND a.alliance_id IS NOT NULL LIMIT 1), false) \
         FROM (SELECT DISTINCT corporation_id FROM owners) o \
         LEFT JOIN names n ON n.id = o.corporation_id \
         LEFT JOIN owner_settings w ON w.corporation_id = o.corporation_id ORDER BY 2",
        &[],
    )
    .map_err(|e| failed("reading owners", e))?;
    let routing_table = with_rows(
        Table::new(vec![
            Column::text("Owner"),
            Column::text("Channels"),
            Column::text("Types"),
            Column::text("Pings"),
            Column::text("Customs offices public"),
            Column::text("Alliance main"),
        ])
        .title("Owners' Discord routing")
        .empty("No owners yet."),
        routing.rows.iter().map(|r| {
            let own = int(r, 2);
            vec![
                link(text(r, 1), format!("settings/owner/{}", int(r, 0))).into(),
                if own == 0 {
                    "The defaults above".to_owned()
                } else {
                    format!("Its own for {own} of {} kinds", Category::ALL.len())
                }
                .into(),
                match opt_int(r, 5) {
                    Some(n) => format!("Its own: {n}"),
                    None => "The defaults".to_owned(),
                }
                .into(),
                match text(r, 3).as_str() {
                    "on" => "On",
                    "off" => "Off",
                    _ => "The default",
                }
                .into(),
                if r.get(4).and_then(Db::as_bool).unwrap_or(false) {
                    "Yes"
                } else {
                    "No"
                }
                .into(),
                if r.get(6).and_then(Db::as_bool).unwrap_or(false) {
                    "Yes"
                } else {
                    "No"
                }
                .into(),
            ]
        }),
    );
    // One form, saved at once from Tether's save bar (DESIGN.md, Save bar):
    // the channels, pings and list, then which types are sent, by kind.
    let mut form = SettingsForm::new("settings")
        .group(discord)
        .group(pings)
        .group(shown)
        .group(admin);
    for (i, category) in Category::ALL.into_iter().enumerate() {
        let mut types = type_group(category, settings.notification_types.as_ref());
        if i == 0 {
            types = types.description(
                "Which notification types are sent (aa-structures' webhook filters), with their \
                 severity, by kind. These are the defaults; an owner can pick its own. A fresh \
                 install ticks aa-structures' defaults for new webhooks.",
            );
        }
        form = form.group(types);
    }
    let page = page.settings(form);
    let types = settings.notification_types.as_ref();
    Ok(page
        .table(fuel_alert_table()?)
        .form(fuel_alert_form(types))
        .form(edit_fuel_alert_form())
        .table(jump_fuel_alert_table()?)
        .form(jump_fuel_alert_form(types))
        .form(edit_jump_fuel_alert_form())
        .table(routing_table)
        .table(owner_table)
        .text(
            "A data source ESI refused (a lost role or token) is left alone for an hour, doubling \
             up to a day. Once it's fixed in game, Retry now reads it again. As aa-structures, a \
             corporation can have up to 10 data sources (Add data source with another of its \
             Station Managers): they take turns reading notifications, cutting the delay from \
             about ten minutes to one.",
        )
        .table(sent_table))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let channel = |name: &str| {
        let c = submission.value(name).trim();
        (!c.is_empty()).then(|| c.to_owned())
    };
    if ["danger_ping", "warning_ping"]
        .iter()
        .any(|name| submission.value(name).trim().chars().count() > 64)
    {
        return Ok(SubmitResult::Page(settings_page(Some(
            "A state's name is at most 64 characters.",
        ))?));
    }
    // Every type ticked is stored as none: every type, new ones included.
    let types = ticked_types(submission);
    let types = (types.len() < notification::TYPES.len()).then(|| types.join(","));
    let moon_timers = submission.checked("moon_extraction_timers");
    let mut statements = vec![Statement::new(
        "UPDATE settings SET attack_channel = $1, fuel_channel = $2, state_channel = $3, \
         moon_channel = $4, default_pings = $5, danger_ping = $6, warning_ping = $9, \
         timers_corporation_only = $7, default_tags_filter = $8, sov_channel = $10, \
         war_channel = $11, corp_channel = $12, \
         notification_types = string_to_array($13, ','), moon_extraction_timers = $14, \
         show_jump_gates = $15, admin_notifications = $16 WHERE id = 1",
        vec![
            channel("attack_channel").into(),
            channel("fuel_channel").into(),
            channel("state_channel").into(),
            channel("moon_channel").into(),
            submission.checked("default_pings").into(),
            channel("danger_ping").into(),
            submission.checked("timers_corporation_only").into(),
            submission.checked("default_tags_filter").into(),
            channel("warning_ping").into(),
            channel("sov_channel").into(),
            channel("war_channel").into(),
            channel("corp_channel").into(),
            types.clone().into(),
            moon_timers.into(),
            submission.checked("show_jump_gates").into(),
            submission.checked("admin_notifications").into(),
        ],
    )];
    if !moon_timers {
        // Off: the extraction timers made go too.
        statements.push(Statement::new(
            "DELETE FROM timers WHERE kind = $1",
            vec![notification::EXTRACTION.into()],
        ));
    }
    storage::transaction(&statements).map_err(|e| failed("saving settings", e))?;
    // Published again at once, so corporation-only (and extraction timers
    // turned off) take effect now.
    jobs::enqueue(NewJob::new(PUBLISH_JOB).key(PUBLISH_JOB))
        .map_err(|e| failed("queuing the timers", e))?;
    log::info(format!(
        "settings changed by {} ({}): attacks {:?}, fuel {:?}, state {:?}, moons {:?}, \
         sovereignty {:?}, wars {:?}, members {:?}, \
         default pings {} (danger {:?}, warning {:?}), timers corporation-only {}, \
         moon extraction timers {moon_timers}, jump gates tab {}, admin notices {}, types {}",
        viewer.main.name,
        viewer.main.id,
        channel("attack_channel"),
        channel("fuel_channel"),
        channel("state_channel"),
        channel("moon_channel"),
        channel("sov_channel"),
        channel("war_channel"),
        channel("corp_channel"),
        submission.checked("default_pings"),
        channel("danger_ping"),
        channel("warning_ping"),
        submission.checked("timers_corporation_only"),
        submission.checked("show_jump_gates"),
        submission.checked("admin_notifications"),
        types.as_deref().unwrap_or("every type"),
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// A notification type's checkbox name.
fn type_field(kind: &str) -> String {
    format!("t_{}", kind.to_ascii_lowercase())
}

fn severity_name(severity: notification::Severity) -> &'static str {
    match severity {
        notification::Severity::Danger => "danger",
        notification::Severity::Warning => "warning",
        notification::Severity::Info => "info",
    }
}

/// The types ticked on a types form, as stored (a comma list; all of
/// them is stored as none, meaning every type, new ones included).
/// A kind's notification types as a form of checkboxes, ticked as in
/// `list` (none: all of them).
/// One kind's types as a settings group, ticked as `list` has them (every
/// type when there's none).
fn type_group(category: Category, list: Option<&Vec<String>>) -> SettingsGroup {
    let mut group = SettingsGroup::new(format!("Types: {}", category.label()));
    for (kind, label, severity, _) in notification::TYPES
        .iter()
        .filter(|(_, _, _, c)| *c == category)
    {
        group = group.field(Field::checkbox(
            type_field(kind),
            format!("{label} ({})", severity_name(*severity)),
            ticked(list, kind),
        ));
    }
    group
}

/// The types a settings form ticked.
fn ticked_types(submission: &Submission) -> Vec<String> {
    notification::TYPES
        .iter()
        .filter(|(kind, _, _, _)| submission.checked(&type_field(kind)))
        .map(|(kind, _, _, _)| (*kind).to_owned())
        .collect()
}

// ---- fuel alert configs ------------------------------------------------------

/// aa-structures' fuel alert configs, each with Delete.
fn fuel_alert_table() -> Result<Table, PageError> {
    let rows = storage::query(
        &format!(
            "SELECT id, start_hours, end_hours, repeat_hours, ping, enabled FROM fuel_alert_configs \
             ORDER BY start_hours DESC, id LIMIT {}",
            MAX_FUEL_CONFIGS * 2
        ),
        &[],
    )
    .map_err(|e| failed("reading fuel alerts", e))?;
    Ok(with_rows(
        Table::new(vec![
            Column::numeric("Start (hours left)"),
            Column::numeric("End (hours left)"),
            Column::numeric("Repeat (hours)"),
            Column::text("Ping"),
            Column::text("Status"),
            Column::text(""),
        ])
        .title("Fuel alerts")
        .empty(
            "No fuel alerts, as aa-structures starts. EVE's own fuel alerts are relayed while \
             their types are ticked. Add one below for more.",
        ),
        rows.rows.iter().map(|r| {
            let (id, start, end, repeat) = (int(r, 0), int(r, 1), int(r, 2), int(r, 3));
            let enabled = r.get(5).and_then(Db::as_bool).unwrap_or(true);
            let ping = ping_label(&text(r, 4));
            vec![
                start.into(),
                end.into(),
                match repeat {
                    0 => "Once".into(),
                    h => h.into(),
                },
                ping.into(),
                enabled_badge(enabled),
                actions(vec![
                    // Opens the change form in a popup, for this one.
                    action("Edit", "edit_fuel_alert")
                        .field("config", id.to_string())
                        .confirm(format!(
                            "Change the alert between {start} and {end} hours of fuel left \
                             (repeat {}, ping {ping}).",
                            if repeat == 0 {
                                "once".to_owned()
                            } else {
                                format!("every {repeat}h")
                            }
                        )),
                    toggle_action("toggle_fuel_alert", id, enabled),
                    action("Delete", "delete_fuel_alert")
                        .field("config", id.to_string())
                        .tone(Tone::Danger)
                        .confirm(format!(
                            "The alert between {start} and {end} hours of fuel left is deleted."
                        )),
                ]),
            ]
        }),
    ))
}

/// A fuel or jump fuel alert's ping, in words.
fn ping_label(ping: &str) -> &'static str {
    match ping {
        "danger" => "Danger role",
        "warning" => "Warning role",
        _ => "None",
    }
}

/// aa-structures' is_enabled: a disabled config sends no new alerts.
fn enabled_badge(enabled: bool) -> Value {
    if enabled {
        badge("Enabled", Tone::Success)
    } else {
        badge("Disabled", Tone::Neutral)
    }
    .into()
}

/// A row's Disable or Enable.
fn toggle_action(form: &str, id: i64, enabled: bool) -> Action {
    action(if enabled { "Disable" } else { "Enable" }, form)
        .field("config", id.to_string())
        .field("enabled", if enabled { "off" } else { "on" })
}

/// A fuel or jump fuel alert's ping: aa-structures' @here unless picked.
fn alert_ping_field() -> Field {
    Field::select(
        "ping",
        "Ping",
        vec![
            ("none".to_owned(), "None".to_owned()),
            ("warning".to_owned(), "Warning role (@here)".to_owned()),
            ("danger".to_owned(), "Danger role (@everyone)".to_owned()),
        ],
    )
    .value("warning")
    .help(
        "The role of the state named under Pings above, mentioned only while default pings (or \
         the owner's own) are on, as aa-structures' webhook and owner pings override an alert's.",
    )
    .required()
}

fn fuel_alert_form(types: Option<&Vec<String>>) -> Form {
    let hours = |name: &str, label: &str, min: f64, help: &str| {
        Field::number(name, label)
            .range(Some(min), Some(MAX_ALERT_HOURS as f64), true)
            .help(help)
            .required()
    };
    let mut description = "aa-structures' fuel alert configs, any number: an alert on the owner's \
        fuel channel when a structure (Upwell or starbase) has at most Start and more than End \
        hours of fuel left, again every Repeat hours while it stays there. EVE's own fuel alerts \
        are relayed as well, as in aa-structures, so a structure in an alert's range is reported \
        by both."
        .to_owned();
    // Sent under EVE's types: one unticked sends none of its kind.
    for (kind, label, of) in [
        (
            "StructureFuelAlert",
            "Upwell structure fuel alert",
            "Upwell structures",
        ),
        ("TowerResourceAlertMsg", "Starbase fuel alert", "starbases"),
    ] {
        if !ticked(types, kind) {
            description.push_str(&format!(
                " Owners on the default types send none for {of} while '{label}' is unticked \
                 under Types: Fuel and services: tick it to send them."
            ));
        }
    }
    Form::new("add_fuel_alert", "Add fuel alert")
        .description(description)
        .field(hours("start_hours", "Start (hours left)", 1.0, "e.g. 48"))
        .field(hours(
            "end_hours",
            "End (hours left)",
            0.0,
            "Less than Start, e.g. 0",
        ))
        .field(
            Field::number("repeat_hours", "Repeat (hours)")
                .range(Some(0.0), Some(MAX_ALERT_HOURS as f64), true)
                .value("0")
                .help("0: once in the range.")
                .required(),
        )
        .field(alert_ping_field())
}

/// A fuel alert config's start, end and repeat hours, and its ping.
type FuelAlert = (i64, i64, i64, String);

/// A fuel alert form's values: start, end, repeat and ping, or what's
/// wrong with them (for the settings page).
fn fuel_alert_values(submission: &Submission) -> Result<Result<FuelAlert, String>, PageError> {
    let number = |name: &str| -> Result<i64, PageError> {
        submission
            .value(name)
            .parse()
            .map_err(|_| PageError::Failed(format!("{name} wasn't a whole number")))
    };
    let (start, end, repeat) = (
        number("start_hours")?,
        number("end_hours")?,
        number("repeat_hours")?,
    );
    let ping = submission.value("ping");
    if !matches!(ping, "none" | "warning" | "danger") {
        return Err(PageError::NotFound);
    }
    if !(1..=MAX_ALERT_HOURS).contains(&start)
        || !(0..=MAX_ALERT_HOURS).contains(&end)
        || !(0..=MAX_ALERT_HOURS).contains(&repeat)
    {
        return Ok(Err(format!(
            "A fuel alert's hours are whole numbers up to {MAX_ALERT_HOURS} (a year)."
        )));
    }
    if end >= start {
        return Ok(Err(
            "A fuel alert's End must be less than its Start: it alerts between them.".to_owned(),
        ));
    }
    Ok(Ok((start, end, repeat, ping.to_owned())))
}

fn add_fuel_alert(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let (start, end, repeat, ping) = match fuel_alert_values(submission)? {
        Ok(values) => values,
        Err(problem) => return Ok(SubmitResult::Page(settings_page(Some(&problem))?)),
    };
    let ping = ping.as_str();
    let added = storage::execute(
        &format!(
            "INSERT INTO fuel_alert_configs (start_hours, end_hours, repeat_hours, ping) \
             SELECT $1, $2, $3, $4 WHERE (SELECT count(*) FROM fuel_alert_configs) < {MAX_FUEL_CONFIGS}"
        ),
        &[start.into(), end.into(), repeat.into(), ping.into()],
    )
    .map_err(|e| failed("adding the fuel alert", e))?;
    if added == 0 {
        return Ok(SubmitResult::Page(settings_page(Some(&format!(
            "At most {MAX_FUEL_CONFIGS} fuel alerts: delete one first."
        )))?));
    }
    log::info(format!(
        "fuel alert {start}h to {end}h (repeat {repeat}h, ping {ping}) added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// The popup a fuel alert's Edit opens: its new values (aa-structures'
/// admin edits a config in place).
fn edit_fuel_alert_form() -> Form {
    let hours = |name: &str, label: &str, min: f64| {
        Field::number(name, label)
            .range(Some(min), Some(MAX_ALERT_HOURS as f64), true)
            .required()
    };
    Form::new("edit_fuel_alert", "Save fuel alert")
        .title("Change fuel alert")
        .field(hours("start_hours", "Start (hours left)", 1.0))
        .field(hours("end_hours", "End (hours left)", 0.0))
        .field(
            Field::number("repeat_hours", "Repeat (hours)")
                .range(Some(0.0), Some(MAX_ALERT_HOURS as f64), true)
                .value("0")
                .help("0: once in the range.")
                .required(),
        )
        .field(alert_ping_field())
}

/// A config's id from a row's hidden field.
fn config_id(submission: &Submission) -> Result<i64, PageError> {
    submission
        .value("config")
        .parse()
        .map_err(|_| PageError::NotFound)
}

/// Changes a fuel alert config. As aa-structures, a new range or repeat
/// forgets what it sent, so structures in the new range are told again.
fn edit_fuel_alert(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let config = config_id(submission)?;
    let (start, end, repeat, ping) = match fuel_alert_values(submission)? {
        Ok(values) => values,
        Err(problem) => return Ok(SubmitResult::Page(settings_page(Some(&problem))?)),
    };
    let params: Vec<Db> = vec![
        config.into(),
        start.into(),
        end.into(),
        repeat.into(),
        ping.as_str().into(),
    ];
    storage::transaction(&[
        Statement::new(
            "DELETE FROM fuel_alerts_sent WHERE config_id = $1 AND EXISTS (SELECT 1 FROM fuel_alert_configs c \
             WHERE c.id = $1 AND (c.start_hours, c.end_hours, c.repeat_hours) \
                 IS DISTINCT FROM ($2::integer, $3::integer, $4::integer))",
            params[..4].to_vec(),
        ),
        Statement::new(
            "UPDATE fuel_alert_configs SET start_hours = $2, end_hours = $3, repeat_hours = $4, ping = $5 \
             WHERE id = $1",
            params,
        ),
    ])
    .map_err(|e| failed("changing the fuel alert", e))?;
    log::info(format!(
        "fuel alert {config} changed to {start}h to {end}h (repeat {repeat}h, ping {ping}) by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// The posted enabled state of a row's Enable or Disable.
fn enabled_value(submission: &Submission) -> Result<bool, PageError> {
    match submission.value("enabled") {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(PageError::NotFound),
    }
}

fn toggle_fuel_alert(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let config = config_id(submission)?;
    let enabled = enabled_value(submission)?;
    storage::execute(
        "UPDATE fuel_alert_configs SET enabled = $2 WHERE id = $1",
        &[config.into(), enabled.into()],
    )
    .map_err(|e| failed("changing the fuel alert", e))?;
    log::info(format!(
        "fuel alert {config} {} by {} ({})",
        if enabled { "enabled" } else { "disabled" },
        viewer.main.name,
        viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

fn delete_fuel_alert(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let config: i64 = submission
        .value("config")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "DELETE FROM fuel_alert_configs WHERE id = $1",
        &[config.into()],
    )
    .map_err(|e| failed("deleting the fuel alert", e))?;
    log::info(format!(
        "fuel alert {config} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// Liquid ozone a jump fuel alert may name, at most.
const MAX_OZONE: i64 = 1_000_000;

fn jump_fuel_alert_table() -> Result<Table, PageError> {
    let rows = storage::query(
        &format!(
            "SELECT id, threshold, ping, enabled FROM jump_fuel_alert_configs ORDER BY threshold DESC, id LIMIT {}",
            MAX_FUEL_CONFIGS * 2
        ),
        &[],
    )
    .map_err(|e| failed("reading jump fuel alerts", e))?;
    Ok(with_rows(
        Table::new(vec![
            Column::numeric("Below (units of liquid ozone)"),
            Column::text("Ping"),
            Column::text("Status"),
            Column::text(""),
        ])
        .title("Jump fuel alerts")
        .empty("No jump fuel alerts: add one below."),
        rows.rows.iter().map(|r| {
            let (id, threshold) = (int(r, 0), int(r, 1));
            let enabled = r.get(3).and_then(Db::as_bool).unwrap_or(true);
            let ping = ping_label(&text(r, 2));
            vec![
                threshold.into(),
                ping.into(),
                enabled_badge(enabled),
                actions(vec![
                    action("Edit", "edit_jump_fuel_alert")
                        .field("config", id.to_string())
                        .confirm(format!(
                            "Change the alert below {threshold} units of liquid ozone (ping {ping})."
                        )),
                    toggle_action("toggle_jump_fuel_alert", id, enabled),
                    action("Delete", "delete_jump_fuel_alert")
                        .field("config", id.to_string())
                        .tone(Tone::Danger)
                        .confirm(format!(
                            "The alert below {threshold} units of liquid ozone is deleted."
                        )),
                ]),
            ]
        }),
    ))
}

fn jump_fuel_alert_form(types: Option<&Vec<String>>) -> Form {
    let mut description = "aa-structures' jump fuel alert configs: an alert on the owner's fuel \
        channel once a jump gate's fuel bay has less liquid ozone than this (as the corporation's \
        assets last said), until it's topped up above it."
        .to_owned();
    if !ticked(types, "StructureJumpFuelAlert") {
        description.push_str(
            " Owners on the default types send none while 'Jump gate low on liquid ozone' is \
             unticked under Types: Fuel and services (aa-structures' defaults leave it off): \
             tick it to send them.",
        );
    }
    Form::new("add_jump_fuel_alert", "Add jump fuel alert")
        .description(description)
        .field(
            Field::number("threshold", "Below (units of liquid ozone)")
                .range(Some(1.0), Some(MAX_OZONE as f64), true)
                .help("e.g. 100000")
                .required(),
        )
        .field(alert_ping_field())
}

fn add_jump_fuel_alert(
    viewer: &Viewer,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let threshold: i64 = submission
        .value("threshold")
        .parse()
        .map_err(|_| PageError::Failed("threshold wasn't a whole number".to_owned()))?;
    let ping = submission.value("ping");
    if !matches!(ping, "none" | "warning" | "danger") {
        return Err(PageError::NotFound);
    }
    if !(1..=MAX_OZONE).contains(&threshold) {
        return Ok(SubmitResult::Page(settings_page(Some(&format!(
            "A jump fuel alert's threshold is a whole number from 1 to {MAX_OZONE}."
        )))?));
    }
    let added = storage::execute(
        &format!(
            "INSERT INTO jump_fuel_alert_configs (threshold, ping) \
             SELECT $1, $2 WHERE (SELECT count(*) FROM jump_fuel_alert_configs) < {MAX_FUEL_CONFIGS}"
        ),
        &[threshold.into(), ping.into()],
    )
    .map_err(|e| failed("adding the jump fuel alert", e))?;
    if added == 0 {
        return Ok(SubmitResult::Page(settings_page(Some(&format!(
            "At most {MAX_FUEL_CONFIGS} jump fuel alerts: delete one first."
        )))?));
    }
    log::info(format!(
        "jump fuel alert below {threshold} (ping {ping}) added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// The popup a jump fuel alert's Edit opens.
fn edit_jump_fuel_alert_form() -> Form {
    Form::new("edit_jump_fuel_alert", "Save jump fuel alert")
        .title("Change jump fuel alert")
        .field(
            Field::number("threshold", "Below (units of liquid ozone)")
                .range(Some(1.0), Some(MAX_OZONE as f64), true)
                .required(),
        )
        .field(alert_ping_field())
}

/// Changes a jump fuel alert config. As aa-structures, a new threshold
/// forgets what it sent.
fn edit_jump_fuel_alert(
    viewer: &Viewer,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let config = config_id(submission)?;
    let threshold: i64 = submission
        .value("threshold")
        .parse()
        .map_err(|_| PageError::Failed("threshold wasn't a whole number".to_owned()))?;
    let ping = submission.value("ping");
    if !matches!(ping, "none" | "warning" | "danger") {
        return Err(PageError::NotFound);
    }
    if !(1..=MAX_OZONE).contains(&threshold) {
        return Ok(SubmitResult::Page(settings_page(Some(&format!(
            "A jump fuel alert's threshold is a whole number from 1 to {MAX_OZONE}."
        )))?));
    }
    let params: Vec<Db> = vec![config.into(), threshold.into(), ping.into()];
    storage::transaction(&[
        Statement::new(
            "DELETE FROM jump_fuel_alerts_sent WHERE config_id = $1 AND EXISTS (SELECT 1 \
             FROM jump_fuel_alert_configs c WHERE c.id = $1 AND c.threshold <> $2::integer)",
            params[..2].to_vec(),
        ),
        Statement::new(
            "UPDATE jump_fuel_alert_configs SET threshold = $2, ping = $3 WHERE id = $1",
            params,
        ),
    ])
    .map_err(|e| failed("changing the jump fuel alert", e))?;
    log::info(format!(
        "jump fuel alert {config} changed to below {threshold} (ping {ping}) by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

fn toggle_jump_fuel_alert(
    viewer: &Viewer,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let config = config_id(submission)?;
    let enabled = enabled_value(submission)?;
    storage::execute(
        "UPDATE jump_fuel_alert_configs SET enabled = $2 WHERE id = $1",
        &[config.into(), enabled.into()],
    )
    .map_err(|e| failed("changing the jump fuel alert", e))?;
    log::info(format!(
        "jump fuel alert {config} {} by {} ({})",
        if enabled { "enabled" } else { "disabled" },
        viewer.main.name,
        viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

fn delete_jump_fuel_alert(
    viewer: &Viewer,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let config: i64 = submission
        .value("config")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute(
        "DELETE FROM jump_fuel_alert_configs WHERE id = $1",
        &[config.into()],
    )
    .map_err(|e| failed("deleting the jump fuel alert", e))?;
    log::info(format!(
        "jump fuel alert {config} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}

/// A channel choice for an owner: the default, not sent, or a channel.
fn owner_channel_field(
    category: Category,
    default: Option<&str>,
    channels: &[discord::Channel],
    value: &str,
) -> Field {
    let named = |id: &str| {
        channels.iter().find(|c| c.id == id).map_or_else(
            || "a channel no longer assigned".to_owned(),
            |c| format!("#{}", c.name),
        )
    };
    let mut options: Vec<(String, String)> = vec![
        (
            "default".to_owned(),
            match default {
                Some(id) => format!("Default ({})", named(id)),
                None => "Default (not sent)".to_owned(),
            },
        ),
        ("none".to_owned(), "Not sent".to_owned()),
    ];
    options.extend(
        channels
            .iter()
            .map(|c| (c.id.clone(), format!("#{}", c.name))),
    );
    Field::select(
        format!("{}_channel", category.name()),
        category.label(),
        options,
    )
    .value(value)
    .required()
}

/// An owner corporation's name: one the sync knows, or one a
/// data source is in (routing can be set before the first sync).
fn owner_name(corp: i64) -> Result<Option<String>, PageError> {
    let known = storage::query(
        "SELECT coalesce(n.name, 'Corporation ' || o.corporation_id::text) FROM owners o \
         LEFT JOIN names n ON n.id = o.corporation_id WHERE o.corporation_id = $1 LIMIT 1",
        &[corp.into()],
    )
    .map_err(|e| failed("reading owners", e))?;
    if let Some(row) = known.rows.first() {
        return Ok(Some(text(row, 0)));
    }
    Ok(esi::data_sources()
        .iter()
        .any(|s| s.corporation_id == corp)
        .then(|| format!("Corporation {corp}")))
}

/// An owner's own Discord routing (aa-structures' webhooks per owner),
/// mentions, and whether its customs offices are on the public list.
fn owner_settings_page(corp: i64) -> Result<Page, PageError> {
    let settings = settings().map_err(|e| failed("reading settings", e))?;
    let name = owner_name(corp)?.ok_or(PageError::NotFound)?;
    let routes = storage::query(
        "SELECT category, channel FROM owner_channels WHERE corporation_id = $1",
        &[corp.into()],
    )
    .map_err(|e| failed("reading routes", e))?;
    let own = storage::query(
        "SELECT mention, pocos_public, array_to_string(notification_types, ','), \
             alliance_main IS NOT NULL AND alliance_main = (SELECT o.alliance_id FROM owners o \
                 WHERE o.corporation_id = $1 AND o.alliance_id IS NOT NULL LIMIT 1), \
             in_service_status \
         FROM owner_settings WHERE corporation_id = $1",
        &[corp.into()],
    )
    .map_err(|e| failed("reading owner settings", e))?;
    let own_types = own.rows.first().and_then(|r| routing::type_list(r.get(2)));
    let in_service_status = own
        .rows
        .first()
        .and_then(|r| r.get(4))
        .and_then(Db::as_bool)
        .unwrap_or(true);
    let mention = own
        .rows
        .first()
        .map_or_else(|| "default".to_owned(), |r| text(r, 0));
    let public = own
        .rows
        .first()
        .and_then(|r| r.get(1))
        .and_then(Db::as_bool)
        .unwrap_or(false);
    let alliance_main = own
        .rows
        .first()
        .and_then(|r| r.get(3))
        .and_then(Db::as_bool)
        .unwrap_or(false);
    let channels = discord::channels();
    let mut discord = SettingsGroup::new("Discord").description(
        "Where this owner's notifications and alerts go. Default follows the settings' \
         channels; pick another channel, or Not sent, to route this owner on its own.",
    );
    for category in Category::ALL {
        let value = match routes.rows.iter().find(|r| text(r, 0) == category.name()) {
            None => "default".to_owned(),
            Some(r) => r
                .get(1)
                .and_then(Db::as_text)
                .filter(|c| !c.is_empty())
                .map_or_else(|| "none".to_owned(), str::to_owned),
        };
        discord = discord.field(owner_channel_field(
            category,
            settings.channel(category),
            &channels,
            &value,
        ));
    }
    let owner = SettingsGroup::new("Pings and customs offices")
        .field(
            Field::select(
                "mention",
                "Pings",
                vec![
                    (
                        "default".to_owned(),
                        format!(
                            "Default ({})",
                            if settings.default_pings { "on" } else { "off" }
                        ),
                    ),
                    ("on".to_owned(), "On".to_owned()),
                    ("off".to_owned(), "Off".to_owned()),
                ],
            )
            .value(mention)
            .help("aa-structures' default pings for this owner: danger and warning notifications mention the settings' roles.")
            .required(),
        )
        .field(
            Field::checkbox("pocos_public", "Customs offices are public", public).help(
                "List this owner's customs offices, with access and tax, for everyone who may \
                 open Structures (aa-structures' public customs offices).",
            ),
        )
        .field(
            Field::checkbox("alliance_main", "Alliance main", alliance_main).help(
                "Send alliance-wide notifications (sovereignty, most wars, bills) through this \
                 owner, as aa-structures' is alliance main: every corporation of the alliance gets \
                 them, so only one owner of it should send them. Ticking it unticks the \
                 alliance's others.",
            ),
        );
    let mut status_help = "aa-structures' is included in service status: admins are told when \
        this owner's reads stop, come back or first work."
        .to_owned();
    if !settings.admin_notifications {
        status_help.push_str(
            " Nothing is sent while Notify admins is unticked in the settings: tick it there.",
        );
    }
    let notices = SettingsGroup::new("Admin notices").field(
        Field::checkbox(
            "in_service_status",
            "Included in service status",
            in_service_status,
        )
        .help(status_help),
    );
    let types = SettingsGroup::new("Notification types")
        .description(
            "Which types this owner sends: the settings' defaults, or its own, ticked below.",
        )
        .field(
            Field::select(
                "types_from",
                "Types",
                vec![
                    ("default".to_owned(), "The defaults".to_owned()),
                    ("own".to_owned(), "Its own, ticked below".to_owned()),
                ],
            )
            .value(if own_types.is_some() {
                "own"
            } else {
                "default"
            })
            .required(),
        );
    let shown = own_types.or_else(|| settings.notification_types.clone());
    // One form, saved at once from Tether's save bar (DESIGN.md, Save bar).
    let mut form = SettingsForm::new("owner_routes")
        .group(discord)
        .group(owner)
        .group(notices)
        .group(types);
    for category in Category::ALL {
        form = form.group(type_group(category, shown.as_ref()));
    }
    Ok(Page::new(format!("Structures owner: {name}"))
        .description("One owner's Discord routing, pings, customs offices and admin notices")
        .settings(form))
}

fn save_owner_settings(
    viewer: &Viewer,
    corp: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    if owner_name(corp)?.is_none() {
        return Err(PageError::NotFound);
    }
    let mut statements = vec![Statement::new(
        "DELETE FROM owner_channels WHERE corporation_id = $1",
        vec![corp.into()],
    )];
    let mut summary = Vec::new();
    for category in Category::ALL {
        let value = submission.value(&format!("{}_channel", category.name()));
        let channel: Option<&str> = match value {
            "default" => {
                summary.push(format!("{} default", category.name()));
                continue;
            }
            "none" => None,
            id => Some(id),
        };
        summary.push(format!("{} {channel:?}", category.name()));
        statements.push(Statement::new(
            "INSERT INTO owner_channels (corporation_id, category, channel) VALUES ($1, $2, $3)",
            vec![corp.into(), category.name().into(), channel.into()],
        ));
    }
    let mention = submission.value("mention");
    if !matches!(mention, "default" | "on" | "off") {
        return Err(PageError::NotFound);
    }
    let alliance_main = submission.checked("alliance_main");
    // Its types: the defaults (none of its own), or those ticked.
    let types = match submission.value("types_from") {
        "default" => None,
        "own" => Some(ticked_types(submission).join(",")),
        _ => return Err(PageError::NotFound),
    };
    // The alliance it's main of (none without one).
    let alliance = "(SELECT o.alliance_id FROM owners o WHERE o.corporation_id = $1 \
                    AND o.alliance_id IS NOT NULL LIMIT 1)";
    if alliance_main {
        // One alliance main per alliance, as aa-structures.
        statements.push(Statement::new(
            format!(
                "UPDATE owner_settings SET alliance_main = NULL \
                 WHERE corporation_id <> $1 AND alliance_main = {alliance}"
            ),
            vec![corp.into()],
        ));
    }
    statements.push(Statement::new(
        format!(
            "INSERT INTO owner_settings (corporation_id, mention, pocos_public, alliance_main, \
                 notification_types, in_service_status) \
             VALUES ($1, $2, $3, CASE WHEN $4 THEN {alliance} END, string_to_array($5, ','), $6) \
             ON CONFLICT (corporation_id) DO UPDATE SET mention = EXCLUDED.mention, \
                 pocos_public = EXCLUDED.pocos_public, alliance_main = EXCLUDED.alliance_main, \
                 notification_types = EXCLUDED.notification_types, \
                 in_service_status = EXCLUDED.in_service_status"
        ),
        vec![
            corp.into(),
            mention.into(),
            submission.checked("pocos_public").into(),
            alliance_main.into(),
            types.clone().into(),
            submission.checked("in_service_status").into(),
        ],
    ));
    storage::transaction(&statements).map_err(|e| failed("saving the owner", e))?;
    log::info(format!(
        "owner {corp} routing set by {} ({}): {}, mention {mention}, customs offices public {}, \
         alliance main {alliance_main}, in service status {}, types {}",
        viewer.main.name,
        viewer.main.id,
        summary.join(", "),
        submission.checked("pocos_public"),
        submission.checked("in_service_status"),
        types.as_deref().unwrap_or("the defaults"),
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
         notifications_failures = 0, notifications_retry_at = NULL, \
         starbases_failures = 0, starbases_retry_at = NULL, offices_failures = 0, \
         offices_retry_at = NULL, assets_failures = 0, assets_retry_at = NULL, last_error = NULL \
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
    fn neutrals_are_admitted_as_aa_structures_judges() {
        let office = |standing: &str, rate: serde_json::Value| {
            serde_json::json!({
                "allow_access_with_standings": true,
                "standing_level": standing,
                "neutral_standing_tax_rate": rate,
            })
        };
        assert!(neutrals_admitted(&office("neutral", 0.1.into())));
        assert!(neutrals_admitted(&office("terrible", 0.1.into())));
        // Good standing asked for: neutrals stay out.
        assert!(!neutrals_admitted(&office("good", 0.1.into())));
        assert!(!neutrals_admitted(&office(
            "neutral",
            serde_json::Value::Null
        )));
        assert!(!neutrals_admitted(&serde_json::json!({
            "allow_access_with_standings": false, "standing_level": "neutral",
            "neutral_standing_tax_rate": 0.1,
        })));
    }

    #[test]
    fn the_jump_gates_tab_lists_ansiblexes() {
        assert!(JUMP_GATES.contains(&format!("s.type_id = {JUMP_GATE})")));
    }

    #[test]
    fn time_left_reads_short() {
        assert_eq!(left(Duration::hours(76)), "3d 4h");
        assert_eq!(left(Duration::minutes(312)), "5h 12m");
        assert_eq!(left(Duration::minutes(-1)), "none");
    }

    #[test]
    fn power_modes_as_aa_structures() {
        let now = Utc::now();
        let day = Duration::days(1);
        // Fuel left: full power, whatever else.
        assert_eq!(
            power_mode(Some(now + day), None, "online", now),
            Power::Full
        );
        // Out of fuel: low power for 7 days after a service was online.
        assert_eq!(power_mode(None, Some(now - day), "online", now), Power::Low);
        assert_eq!(
            power_mode(Some(now - day), Some(now - day * 6), "online", now),
            Power::Low
        );
        assert_eq!(
            power_mode(None, Some(now - day * 8), "online", now),
            Power::Abandoned
        );
        // Never seen online: "Abandoned?", unless it's anchoring.
        assert_eq!(
            power_mode(None, None, "shield_vulnerable", now),
            Power::MaybeAbandoned
        );
        assert_eq!(power_mode(None, None, "anchoring", now), Power::Low);
        assert_eq!(power_mode(None, None, "anchor_vulnerable", now), Power::Low);
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
