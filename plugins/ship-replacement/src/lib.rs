//! Ship Replacement (Alliance Auth's srp; PRD F23).
//!
//! AA's permissions and rules (Jay, 2026-09-27):
//!
//! - **SRP fleets** (`add_srpfleetmain` or `srp_management` adds them): a
//!   fleet name, doctrine, fleet commander and EVE time, an after action
//!   report, and an SRP code pilots request with. Open until SRP managers
//!   mark them Completed.
//! - Everyone with `access_srp` sees the open fleets with their Total ISK
//!   Cost and pending requests, and opens any fleet's requests (pilots,
//!   ships, amounts, status), as AA, every one of them, a page at a time.
//!   All fleets (AA's View All, completed fleets too) is open to every
//!   `access_srp` holder, as AA's view is.
//! - **Request SRP** (`access_srp`): a pilot pastes a zKillboard link for
//!   a loss on an open fleet. The loss comes from ESI's public killmail
//!   endpoint (through Tether), its value from zKillboard (over the app's
//!   approved HTTPS host). The victim must be one of the pilot's own
//!   characters; each loss is requested once while its request exists.
//! - **Managing** (`srp_management`, AA's `auth.srp_management`): approve
//!   (the payout defaults to zKillboard's value) or reject with a comment,
//!   update the payout at any time, mark approved ones paid (aa-srp's),
//!   complete and remove fleets. As AA, nothing stops a manager deciding
//!   their own request.
//! - **SRP team channel** (aa-srp's `srp_team_discord_channel_id`, none by
//!   default): Settings, for `manage` (aa-srp's setting is changed in
//!   Django's admin), picks one of the Discord channels an admin assigned
//!   the app. Each new request is posted there as a card, pinging nobody:
//!   queued with the request and sent by a relay job, so a burst of
//!   requests or a slow Discord loses none and never fails the pilot's
//!   request (aa-srp queues its message too).
//!
//! zKillboard asks for gentle use: a kill's value is fetched once and
//! kept, and only when a pilot requests SRP for it.

mod killmail;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use tether_plugin_sdk::discord::{self, Embed, Image, Mention};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::http;
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Action, Badge, Card, Column, Field, Form, Page, PageError, Plugin, Request, SettingsForm,
    SettingsGroup, Stat, Submission, SubmitResult, Table, Tone, Value, action, actions, badge,
    character, isk, item_type, link, log, share, time,
};

const MAX_NAME: u32 = 150;
const MAX_DOCTRINE: u32 = 200;
const MAX_FC: u32 = 60;
const MAX_AAR: u32 = 500;
const MAX_LINK: u32 = 300;
const MAX_INFO: u32 = 1000;
/// At most 4 bytes a character: one page value (2 KiB) always holds it.
const MAX_COMMENT: u32 = 500;
const MAX_COMMENTS: i64 = 100;
/// Rows on one page (the host allows 500). A fleet's requests go on as
/// many pages as they need.
const FLEET_ROWS: i64 = 200;
const REQUEST_ROWS: i64 = 400;
const MY_ROWS: i64 = 100;
/// The biggest payout a reviewer may set: more than any ship is worth.
const MAX_PAYOUT: f64 = killmail::MAX_VALUE;
/// Links that don't check out, per pilot, before they wait a while.
const MAX_FAILED_LOOKUPS: i64 = 5;
/// The job posting new requests to the SRP team's channel.
const RELAY: &str = "relay";
/// Discord messages per relay run (the host's limit), and the gaps.
const SENDS_PER_RUN: i64 = 5;
const RELAY_GAP_SECONDS: i64 = 15;
const RELAY_BACKOFF_SECONDS: i64 = 60;
/// A card not sent by then isn't news any more.
const STALE_HOURS: i64 = 6;
/// Additional info on a card, cut before escaping so it stays within the
/// host's 2,000 characters.
const CARD_INFO: usize = 900;

struct ShipReplacement;

impl Plugin for ShipReplacement {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        // The app's settings, for `manage` (the manifest's rule): no New
        // SRP fleet button there.
        if parts.as_slice() == ["settings"] {
            return settings_page();
        }
        let page = match parts.as_slice() {
            [""] => srp_fleets(&viewer, false),
            ["all"] => srp_fleets(&viewer, true),
            ["add"] => add_page(&viewer, None),
            ["fleet", fleet] => fleet_page(&viewer, id(fleet)?, 1, None),
            ["fleet", fleet, "page", n] => fleet_page(&viewer, id(fleet)?, id(n)?, None),
            ["request", code] => request_page(code, None),
            ["review", request] => review_page(&viewer, id(request)?, None),
            _ => Err(PageError::NotFound),
        }?;
        Ok(with_add(page, &viewer))
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        if let (["settings"], "settings") = (parts.as_slice(), submission.form.as_str()) {
            return save_settings(&viewer, &submission);
        }
        let result = match (parts.as_slice(), submission.form.as_str()) {
            (["add"], "add_fleet") => add_fleet(&viewer, &submission),
            (["fleet", fleet], _) => fleet_action(&viewer, id(fleet)?, 1, &submission),
            (["fleet", fleet, "page", n], _) => {
                fleet_action(&viewer, id(fleet)?, id(n)?, &submission)
            }
            (["request", code], "request") => request_srp(&viewer, code, &submission),
            (["review", request], _) => review_action(&viewer, id(request)?, &submission),
            _ => Err(PageError::NotFound),
        }?;
        Ok(match result {
            SubmitResult::Page(page) => SubmitResult::Page(with_add(page, &viewer)),
            other => other,
        })
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            RELAY => relay(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(ShipReplacement);

/// New SRP fleet (AA's Add SRP Fleet) as the header's button, beside the
/// views Tether draws from the manifest. The app adds it rather than the
/// manifest's `[action]`, which shows by one page rule: AA lets
/// `add_srpfleetmain` or `srp_management` add fleets.
fn with_add(page: Page, viewer: &Viewer) -> Page {
    if can_add(viewer) {
        page.button("New SRP fleet", "add")
    } else {
        page
    }
}

/// A character's portrait and name (the name alone if the id is unknown).
fn pilot(id: i64, name: &str) -> Value {
    if id > 0 {
        character(id, name).into()
    } else {
        name.into()
    }
}

/// A ship's icon and name.
fn ship(type_id: i64, name: &str) -> Value {
    if type_id > 0 {
        item_type(type_id, name).into()
    } else {
        name.into()
    }
}

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

/// A positive id from a path segment.
fn id(segment: &str) -> Result<i64, PageError> {
    segment
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PageError::NotFound)
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

fn float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
}

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

fn flag(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or_default()
}

fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn time_or(t: Option<DateTime<Utc>>, otherwise: &str) -> Value {
    t.map_or_else(|| otherwise.into(), |t| time(rfc3339(t)))
}

fn isk_or(amount: Option<f64>, otherwise: &str) -> Value {
    amount.map_or_else(|| otherwise.into(), isk)
}

fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

/// Text cut to `max` characters, for page values.
fn cut(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// An EVE time as typed: `2026-09-30 18:00` (or with `.` in the date, as
/// the game shows it), seconds optional.
fn parse_eve_time(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim().trim_end_matches('Z').replace('T', " ");
    let (date, clock) = text.split_once(' ')?;
    let normal = format!("{} {}", date.replace('.', "-"), clock.trim());
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(&normal, format).ok())
        .map(|t| t.and_utc())
}

/// AA's `auth.srp_management`: decides, prices and pays requests, and
/// completes and removes fleets.
fn manager(viewer: &Viewer) -> bool {
    viewer.can("srp_management")
}

/// AA's Add SRP Fleet: `srp_management` or `add_srpfleetmain`.
fn can_add(viewer: &Viewer) -> bool {
    manager(viewer) || viewer.can("add_srpfleetmain")
}

/// Pages behind the `""` rule need `access_srp` too; staff pages more.
fn need(allowed: bool) -> Result<(), PageError> {
    if allowed {
        Ok(())
    } else {
        Err(PageError::Forbidden)
    }
}

// ---- fleets ------------------------------------------------------------------

struct Fleet {
    id: i64,
    name: String,
    doctrine: String,
    fleet_commander: String,
    time: Option<DateTime<Utc>>,
    aar: String,
    code: String,
    completed: bool,
    created_by: String,
}

const FLEET_COLUMNS: &str = "f.id, f.name, f.doctrine, f.fleet_commander, f.fleet_time, \
     f.aar, f.srp_code, f.completed, f.created_by_name";
const FLEET_SELECT: &str = "SELECT f.id, f.name, f.doctrine, f.fleet_commander, f.fleet_time, \
     f.aar, f.srp_code, f.completed, f.created_by_name FROM fleets f";

fn fleet(row: &[Db]) -> Fleet {
    Fleet {
        id: int(row, 0),
        name: text(row, 1),
        doctrine: text(row, 2),
        fleet_commander: text(row, 3),
        time: when(row, 4),
        aar: text(row, 5),
        code: text(row, 6),
        completed: flag(row, 7),
        created_by: text(row, 8),
    }
}

impl Fleet {
    fn status(&self) -> Badge {
        if self.completed {
            badge("Completed", Tone::Neutral)
        } else {
            badge("Open", Tone::Success)
        }
    }
}

fn fleet_by_id(fleet_id: i64) -> Result<Fleet, PageError> {
    query(
        &format!("{FLEET_SELECT} WHERE f.id = $1"),
        &[fleet_id.into()],
    )?
    .first()
    .map(|r| fleet(r))
    .ok_or(PageError::NotFound)
}

/// A fleet by its SRP code (letters and digits only).
fn fleet_by_code(code: &str) -> Result<Fleet, PageError> {
    if code.is_empty() || code.len() > 40 || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(PageError::NotFound);
    }
    query(
        &format!("{FLEET_SELECT} WHERE f.srp_code = $1"),
        &[code.into()],
    )?
    .first()
    .map(|r| fleet(r))
    .ok_or(PageError::NotFound)
}

/// Sums over a fleet's requests (or a list's): requested (zKillboard's
/// value), AA's Total ISK Cost (every payout set), approved and paid
/// payouts, and counts.
struct Totals {
    requests: i64,
    pending: i64,
    requested: f64,
    cost: f64,
    approved: f64,
    paid: f64,
}

const TOTALS: &str = "SELECT count(*), count(*) FILTER (WHERE status = 'pending'), \
     coalesce(sum(kb_total_loss), 0)::float8, \
     coalesce(sum(payout), 0)::float8, \
     coalesce(sum(payout) FILTER (WHERE status = 'approved'), 0)::float8, \
     coalesce(sum(payout) FILTER (WHERE status = 'approved' AND paid), 0)::float8 \
     FROM requests";

fn totals(row: &[Db]) -> Totals {
    Totals {
        requests: int(row, 0),
        pending: int(row, 1),
        requested: float(row, 2).unwrap_or_default(),
        cost: float(row, 3).unwrap_or_default(),
        approved: float(row, 4).unwrap_or_default(),
        paid: float(row, 5).unwrap_or_default(),
    }
}

/// The totals' cards; `over` says which fleets they add up ("open
/// fleets", "every fleet", "this fleet").
fn total_stats(t: &Totals, over: &str) -> Vec<Stat> {
    vec![
        Stat::new("Requests", t.requests).caption(format!("on {over}")),
        Stat::new("Pending", t.pending).caption("waiting for a decision"),
        Stat::new("Losses", isk(t.requested)).caption("zKillboard's value"),
        Stat::new("Total ISK Cost", isk(t.cost)).caption("every payout set"),
        Stat::new("Paid", isk(t.paid)).caption(format!("on {over}")),
        Stat::new("Outstanding", isk(t.approved - t.paid)).caption("approved, not paid"),
    ]
}

/// AA's SRP fleet list: the open fleets, or (All fleets) every fleet, with
/// their Total ISK Cost and pending requests, for everyone with
/// `access_srp`.
fn srp_fleets(viewer: &Viewer, all: bool) -> Result<Page, PageError> {
    let fleets: Vec<(Fleet, i64, f64)> = query(
        &format!(
            "SELECT {FLEET_COLUMNS}, \
             (SELECT count(*) FROM requests r WHERE r.fleet_id = f.id AND r.status = 'pending'), \
             (SELECT coalesce(sum(r.payout), 0)::float8 FROM requests r WHERE r.fleet_id = f.id) \
             FROM fleets f WHERE $2 OR NOT f.completed \
             ORDER BY f.completed, f.fleet_time DESC, f.id DESC LIMIT $1"
        ),
        &[FLEET_ROWS.into(), all.into()],
    )?
    .iter()
    .map(|r| (fleet(r), int(r, 9), float(r, 10).unwrap_or_default()))
    .collect();
    let columns = vec![
        Column::text("Fleet Name"),
        Column::numeric("Fleet Time"),
        Column::text("Doctrine"),
        Column::text("Fleet Commander"),
        Column::text("Status"),
        Column::numeric("Pending"),
        Column::numeric("Total ISK Cost"),
        Column::text("SRP"),
    ];
    let mut table = Table::new(columns)
        .title(if all { "All SRP fleets" } else { "SRP fleets" })
        .empty(if all {
            "No SRP fleets yet."
        } else {
            "No open SRP fleets."
        });
    for (f, pending, cost) in &fleets {
        let mut row = vec![
            link(f.name.clone(), format!("fleet/{}", f.id)).into(),
            time_or(f.time, ""),
            f.doctrine.clone().into(),
            f.fleet_commander.clone().into(),
            f.status().into(),
            (*pending).into(),
            isk(*cost),
        ];
        row.push(if f.completed {
            "Closed".into()
        } else {
            link("Request SRP", format!("request/{}", f.code)).into()
        });
        table = table.row(row);
    }

    let mine: Vec<Req> = query(
        &format!("{REQUEST_SELECT} WHERE r.account_id = $1 ORDER BY r.created_at DESC LIMIT $2"),
        &[viewer.account_id.into(), MY_ROWS.into()],
    )?
    .iter()
    .map(|r| request(r))
    .collect();
    let mut my = Table::new(vec![
        Column::text("Fleet"),
        Column::text("Character"),
        Column::text("Ship"),
        Column::numeric("Lost"),
        Column::numeric("Loss value"),
        Column::numeric("Payout"),
        Column::text("Status"),
    ])
    .title("My SRP requests")
    .empty("You haven't requested SRP yet.");
    for r in &mine {
        my = my.row(vec![
            r.fleet_name.clone().into(),
            pilot(r.character_id, &r.character_name),
            ship(r.ship_type_id, &r.ship_name),
            time_or(r.killmail_time, ""),
            isk_or(r.kb_total_loss, "unknown"),
            isk_or(r.payout, ""),
            r.status().into(),
        ]);
    }

    // Overview is the open fleets; All fleets is every one, completed too.
    let mut page = if all {
        Page::new("All fleets")
            .description("Every SRP fleet, open or completed, and your requests for your losses")
    } else {
        Page::new("Ship Replacement")
            .description("Open SRP fleets, and your requests for your losses on them")
    };
    // AA's Total ISK Cost, over the fleets listed.
    let sums = query(
        &format!("{TOTALS} WHERE $1 OR fleet_id IN (SELECT id FROM fleets WHERE NOT completed)"),
        &[all.into()],
    )?;
    if let Some(row) = sums.first() {
        page = page.stats(total_stats(
            &totals(row),
            if all { "every fleet" } else { "open fleets" },
        ));
    }
    Ok(page.table(table).table(my))
}

fn add_page(viewer: &Viewer, note: Option<&str>) -> Result<Page, PageError> {
    need(can_add(viewer))?;
    let now = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let form = Form::new("add_fleet", "Create SRP fleet")
        .description("Pilots request SRP with the fleet's SRP code, until you mark it Completed.")
        .field(Field::text("name", "Fleet Name", MAX_NAME).required())
        .field(Field::text("doctrine", "Fleet Doctrine", MAX_DOCTRINE).required())
        .field(
            Field::text("fleet_commander", "Fleet Commander", MAX_FC)
                .required()
                // Whoever they act as: an FC's own alt, say.
                .value(
                    tether_plugin_sdk::identity::acting()
                        .map_or_else(|| viewer.main.name.clone(), |c| c.name),
                ),
        )
        .field(
            Field::text("fleet_time", "Fleet Time (EVE)", 20)
                .required()
                .value(now)
                .help("YYYY-MM-DD HH:MM"),
        )
        .field(
            Field::textarea("aar", "After Action Report", MAX_AAR)
                .help("Optional: what happened, or where the report is."),
        );
    let mut page = Page::new("New SRP fleet");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page.form(form))
}

fn add_fleet(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(can_add(viewer))?;
    let again = |text: &str| Ok(SubmitResult::Page(add_page(viewer, Some(text))?));
    let name = submission.value("name").trim().to_owned();
    let doctrine = submission.value("doctrine").trim().to_owned();
    let fc = submission.value("fleet_commander").trim().to_owned();
    if name.is_empty() || doctrine.is_empty() || fc.is_empty() {
        return again("Give the fleet's name, doctrine and fleet commander.");
    }
    let Some(at) = parse_eve_time(submission.value("fleet_time")) else {
        return again("Write the fleet time as YYYY-MM-DD HH:MM, in EVE time.");
    };
    if (at - Utc::now()).num_days().abs() > 366 {
        return again("That fleet time is more than a year away: check the date.");
    }
    // A random code from Postgres (gen_random_uuid is a strong random
    // source), 16 letters and digits like AA's.
    let added = storage::query(
        "INSERT INTO fleets (name, doctrine, fleet_commander, fleet_time, aar, srp_code, \
                             created_by_account_id, created_by_name) \
         VALUES ($1, $2, $3, $4, $5, upper(substr(replace(gen_random_uuid()::text, '-', ''), 1, 16)), \
                 $6, $7) \
         RETURNING id",
        &[
            name.clone().into(),
            doctrine.into(),
            fc.into(),
            Db::timestamp(rfc3339(at)),
            submission.value("aar").trim().to_owned().into(),
            viewer.account_id.into(),
            viewer.main.name.clone().into(),
        ],
    )
    .map_err(|e| failed("adding the fleet", e))?;
    let fleet_id = added
        .rows
        .first()
        .map(|r| int(r, 0))
        .ok_or_else(|| PageError::Failed("the new fleet has no id".to_owned()))?;
    log::info(format!(
        "SRP fleet {fleet_id} ({name}) added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(format!("fleet/{fleet_id}")))
}

// ---- requests ----------------------------------------------------------------

struct Req {
    id: i64,
    fleet_id: i64,
    fleet_name: String,
    character_name: String,
    character_id: i64,
    ship_type_id: i64,
    killmail_id: i64,
    link: String,
    ship_name: String,
    killmail_time: Option<DateTime<Utc>>,
    kb_total_loss: Option<f64>,
    payout: Option<f64>,
    status: String,
    paid: bool,
    info: String,
    reviewer: String,
    created_at: Option<DateTime<Utc>>,
    decided_at: Option<DateTime<Utc>>,
    paid_at: Option<DateTime<Utc>>,
}

const REQUEST_SELECT: &str = "SELECT r.id, r.fleet_id, f.name, r.character_name, r.killmail_id, \
     r.killboard_link, r.ship_name, r.killmail_time, r.kb_total_loss, r.payout, r.status, r.paid, \
     r.additional_info, coalesce(r.reviewer_name, ''), r.created_at, r.decided_at, r.paid_at, \
     r.account_id, f.fleet_time, r.character_id, r.ship_type_id \
     FROM requests r JOIN fleets f ON f.id = r.fleet_id";

fn request(row: &[Db]) -> Req {
    Req {
        id: int(row, 0),
        fleet_id: int(row, 1),
        fleet_name: text(row, 2),
        character_name: text(row, 3),
        killmail_id: int(row, 4),
        link: text(row, 5),
        ship_name: text(row, 6),
        killmail_time: when(row, 7),
        kb_total_loss: float(row, 8),
        payout: float(row, 9),
        status: text(row, 10),
        paid: flag(row, 11),
        info: text(row, 12),
        reviewer: text(row, 13),
        created_at: when(row, 14),
        decided_at: when(row, 15),
        paid_at: when(row, 16),
        character_id: int(row, 19),
        ship_type_id: int(row, 20),
    }
}

impl Req {
    fn status(&self) -> Badge {
        match (self.status.as_str(), self.paid) {
            ("approved", true) => badge("Paid", Tone::Success),
            ("approved", false) => badge("Approved", Tone::Accent),
            ("rejected", _) => badge("Rejected", Tone::Danger),
            _ => badge("Pending", Tone::Warning),
        }
    }
}

fn request_page(code: &str, note: Option<&str>) -> Result<Page, PageError> {
    let f = fleet_by_code(code)?;
    let about = Card::new(f.name.clone())
        .field("Doctrine", f.doctrine.clone())
        .field("Fleet Commander", f.fleet_commander.clone())
        .field("Fleet Time", time_or(f.time, ""))
        .field("Status", f.status());
    let mut page = Page::new("Request SRP").description(format!("For your loss on {}", f.name));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.card(about);
    if f.completed {
        return Ok(page.text("This fleet's SRP is completed: it takes no more requests."));
    }
    Ok(page.form(
        Form::new("request", "Request SRP")
            .description(
                "Tether reads the loss from EVE and its value from zKillboard. It must be a loss \
                 of one of your characters, and each loss can be requested once.",
            )
            .field(
                Field::text("killboard_link", "Killboard Link", MAX_LINK)
                    .required()
                    .help("zKillboard's link to the kill: https://zkillboard.com/kill/…"),
            )
            .field(
                Field::textarea("additional_info", "Additional Info", MAX_INFO)
                    .help("Anything SRP staff should know."),
            ),
    ))
}

/// zKillboard's hash and value for a kill: kept once fetched, else asked.
/// `None` if zKillboard doesn't know it or couldn't be asked.
fn zkb_value(kill: i64) -> Result<Option<killmail::Zkb>, PageError> {
    if let Some(row) = query(
        "SELECT killmail_hash, total_value FROM zkb_values WHERE killmail_id = $1",
        &[kill.into()],
    )?
    .first()
    {
        return Ok(Some(killmail::Zkb {
            hash: text(row, 0),
            total_value: float(row, 1).unwrap_or_default(),
        }));
    }
    // zKillboard asks for gentle use: a kill it had nothing on is left
    // alone for a while.
    if !query(
        "SELECT 1 FROM zkb_misses WHERE killmail_id = $1 \
         AND fetched_at > now() - interval '10 minutes'",
        &[kill.into()],
    )?
    .is_empty()
    {
        return Ok(None);
    }
    let missed = || {
        storage::execute(
            "INSERT INTO zkb_misses (killmail_id) VALUES ($1) \
             ON CONFLICT (killmail_id) DO UPDATE SET fetched_at = now()",
            &[kill.into()],
        )
        .map(|_| None)
        .map_err(|e| failed("noting zKillboard's miss", e))
    };
    let answer = match http::get_json(&format!("https://zkillboard.com/api/killID/{kill}/")) {
        Ok(answer) if answer.is_success() => answer,
        Ok(answer) => {
            log::warn(format!(
                "zKillboard answered {} for kill {kill}",
                answer.status
            ));
            return missed();
        }
        Err(err) => {
            log::warn(format!("zKillboard for kill {kill}: {err:?}"));
            return missed();
        }
    };
    let Some(zkb) = answer
        .text()
        .and_then(|body| killmail::parse_zkb(body, kill))
    else {
        return missed();
    };
    storage::execute(
        "INSERT INTO zkb_values (killmail_id, killmail_hash, total_value) VALUES ($1, $2, $3) \
         ON CONFLICT (killmail_id) DO NOTHING",
        &[kill.into(), zkb.hash.clone().into(), zkb.total_value.into()],
    )
    .map_err(|e| failed("keeping zKillboard's value", e))?;
    Ok(Some(zkb))
}

fn request_srp(
    viewer: &Viewer,
    code: &str,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let f = fleet_by_code(code)?;
    let again = |text: &str| Ok(SubmitResult::Page(request_page(code, Some(text))?));
    if f.completed {
        return again("This fleet's SRP is completed: it takes no more requests.");
    }
    let link_text = submission.value("killboard_link").trim().to_owned();
    let Some(link) = killmail::parse_link(&link_text) else {
        return again(
            "That isn't a zKillboard link: paste the kill's link, https://zkillboard.com/kill/….",
        );
    };
    let recent_failures = query(
        "SELECT count(*) FROM failed_lookups WHERE account_id = $1 \
         AND at > now() - interval '10 minutes'",
        &[viewer.account_id.into()],
    )?
    .first()
    .map_or(0, |r| int(r, 0));
    if recent_failures >= MAX_FAILED_LOOKUPS {
        return again(
            "Several of your links didn't check out lately: wait a few minutes, then try again.",
        );
    }
    // A link that doesn't check out counts against the pilot.
    let refused = |text: &str| {
        storage::transaction(&[
            Statement::new(
                "DELETE FROM failed_lookups WHERE at < now() - interval '1 day'",
                vec![],
            ),
            Statement::new(
                "INSERT INTO failed_lookups (account_id) VALUES ($1)",
                vec![viewer.account_id.into()],
            ),
        ])
        .map_err(|e| failed("noting a failed lookup", e))?;
        again(text)
    };
    // AA's duplicate check: a loss with a request (on any fleet).
    if !query(
        "SELECT 1 FROM requests WHERE killmail_id = $1 \
         UNION ALL SELECT 1 FROM legacy_claims WHERE killmail_id = $1",
        &[link.id.into()],
    )?
    .is_empty()
    {
        return again("SRP has already been requested for this loss.");
    }
    let Some(zkb) = zkb_value(link.id)? else {
        return refused(
            "zKillboard doesn't know that kill yet, or couldn't be reached. Try again in a few \
             minutes.",
        );
    };
    let hash = zkb.hash.clone();
    let params = vec![
        ("killmail_id".to_owned(), link.id.to_string()),
        ("killmail_hash".to_owned(), hash.clone()),
    ];
    // A public endpoint: the subject isn't used.
    let body = match esi::get(
        "killmail",
        Subject::Character(viewer.main.id),
        &params,
        None,
    ) {
        Ok(response) => response.body,
        Err(esi::Error::Status(status)) if (400..500).contains(&status) => {
            return refused("EVE doesn't know that killmail: check the link.");
        }
        Err(err) => {
            log::warn(format!("ESI killmail {}: {err:?}", link.id));
            return again("EVE couldn't be asked about that killmail just now: try again.");
        }
    };
    let Some(km) = killmail::parse_killmail(&body) else {
        return Err(PageError::Failed(
            "ESI's killmail couldn't be read".to_owned(),
        ));
    };
    // The victim must be one of the pilot's own characters (as AA).
    let character = match km.victim_character_id {
        Some(victim) => match viewer.characters.iter().find(|c| c.id == victim) {
            Some(character) => character,
            None => return refused("That loss isn't one of your characters'."),
        },
        None => {
            return refused("That loss has no pilot: SRP is for ships your characters lost.");
        }
    };
    let ship_name = esi::names(&[km.ship_type_id])
        .ok()
        .and_then(|names| names.into_iter().find(|n| n.id == km.ship_type_id))
        .map_or_else(|| format!("Type {}", km.ship_type_id), |n| n.name);
    // Once per loss while its request exists (killmail_id is unique), and
    // only on a fleet still open. Its card for the SRP team's channel, if
    // Settings picked one, is queued with it.
    let added = storage::query(
        "WITH added AS (INSERT INTO requests (fleet_id, account_id, character_id, \
             character_name, killmail_id, killmail_hash, killboard_link, ship_type_id, ship_name, \
             solar_system_id, killmail_time, kb_total_loss, additional_info) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13 \
         WHERE EXISTS (SELECT 1 FROM fleets WHERE id = $1 AND NOT completed) \
           AND NOT EXISTS (SELECT 1 FROM legacy_claims WHERE killmail_id = $5) \
         ON CONFLICT (killmail_id) DO NOTHING RETURNING id), \
         queued AS (INSERT INTO outbox (request_id, channel) \
             SELECT a.id, s.channel FROM added a, settings s \
             WHERE s.id = 1 AND s.channel IS NOT NULL RETURNING 1) \
         SELECT id, (SELECT count(*) FROM queued)::bigint FROM added",
        &[
            f.id.into(),
            viewer.account_id.into(),
            character.id.into(),
            character.name.clone().into(),
            link.id.into(),
            hash.into(),
            link_text.into(),
            km.ship_type_id.into(),
            ship_name.clone().into(),
            km.solar_system_id.into(),
            Db::timestamp(rfc3339(km.time)),
            zkb.total_value.into(),
            submission.value("additional_info").trim().to_owned().into(),
        ],
    )
    .map_err(|e| failed("saving the request", e))?;
    let Some(row) = added.rows.first() else {
        return again("SRP has already been requested for this loss.");
    };
    log::info(format!(
        "SRP requested on fleet {} for {} lost by {} ({}), kill {}",
        f.id, ship_name, character.name, character.id, link.id
    ));
    // The relay posts it; the request stands whatever becomes of that.
    if int(row, 1) > 0
        && let Err(err) = jobs::enqueue(NewJob::new(RELAY).key(RELAY))
    {
        log::warn(format!(
            "the SRP team's card for request {} waits for the next relay: {err:?}",
            int(row, 0)
        ));
    }
    Ok(SubmitResult::Redirect(String::new()))
}

// ---- the SRP team's channel ------------------------------------------------------

/// The channel Settings picked (aa-srp's `srp_team_discord_channel_id`),
/// if any.
fn team_channel() -> Result<Option<String>, PageError> {
    Ok(query("SELECT channel FROM settings WHERE id = 1", &[])?
        .first()
        .and_then(|r| r.first())
        .and_then(Db::as_text)
        .map(str::to_owned))
}

/// Discord markdown out of what players write, and no bare links.
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

/// A new request, as its card tells it.
struct TeamRequest {
    fleet_name: String,
    srp_code: String,
    character_id: i64,
    character_name: String,
    ship_type_id: i64,
    ship_name: String,
    killmail_id: i64,
    info: String,
    requested: Option<DateTime<Utc>>,
}

/// aa-srp's "New SRP Request" message to the SRP team, as a card: the
/// pilot, the ship, the fleet and its SRP code, the loss on zKillboard
/// (built from the killmail's id, never the pasted link) and the
/// additional info only when there is some. Names and info are escaped,
/// so nothing pings or links.
fn team_card(r: &TeamRequest) -> Embed {
    let mut card = Embed::new(format!("New SRP request: {}", r.ship_name))
        .author(
            r.character_name.clone(),
            Some(Image::Character(r.character_id)),
        )
        .thumbnail(Image::TypeRender(r.ship_type_id))
        // aa-srp's info colour.
        .color(0x5b_c0de)
        .field("SRP fleet", escape(&r.fleet_name))
        .field("SRP code", r.srp_code.clone())
        .field(
            "Killmail",
            format!(
                "[zKillboard](https://zkillboard.com/kill/{}/)",
                r.killmail_id
            ),
        )
        .footer("Ship Replacement");
    let info = r.info.trim();
    if !info.is_empty() {
        let mut cut: String = info.chars().take(CARD_INFO).collect();
        if info.chars().count() > CARD_INFO {
            cut.push('…');
        }
        card = card.description(escape(&cut));
    }
    if let Some(at) = r.requested {
        card = card.timestamp(rfc3339(at));
    }
    card
}

/// Posts the queued cards, up to the host's limit a run, then comes back
/// for the rest (as Contracts' and Freight's relays).
fn relay() -> Result<(), JobError> {
    let retry = |what: &str, err: storage::Error| JobError::Retry(format!("{what}: {err:?}"));
    storage::execute(
        "UPDATE outbox SET failed = 'too old to send' WHERE sent_at IS NULL AND failed IS NULL \
         AND queued_at < now() - make_interval(hours => $1::int)",
        &[STALE_HOURS.into()],
    )
    .map_err(|e| retry("expiring cards", e))?;
    let mut gap = RELAY_GAP_SECONDS;
    // A request removed with its fleet takes its card along.
    let waiting = storage::query(
        "SELECT o.id, o.channel, f.name, f.srp_code, r.character_id, r.character_name, \
             r.ship_type_id, r.ship_name, r.killmail_id, r.additional_info, r.created_at \
         FROM outbox o JOIN requests r ON r.id = o.request_id JOIN fleets f ON f.id = r.fleet_id \
         WHERE o.sent_at IS NULL AND o.failed IS NULL ORDER BY o.id LIMIT $1",
        &[SENDS_PER_RUN.into()],
    )
    .map_err(|e| retry("reading the outbox", e))?;
    for row in &waiting.rows {
        let id = int(row, 0);
        let claimed = storage::execute(
            "UPDATE outbox SET sent_at = now() WHERE id = $1 AND sent_at IS NULL AND failed IS NULL",
            &[id.into()],
        )
        .map_err(|e| retry("claiming a card", e))?;
        if claimed == 0 {
            continue;
        }
        let card = team_card(&TeamRequest {
            fleet_name: text(row, 2),
            srp_code: text(row, 3),
            character_id: int(row, 4),
            character_name: text(row, 5),
            ship_type_id: int(row, 6),
            ship_name: text(row, 7),
            killmail_id: int(row, 8),
            info: text(row, 9),
            requested: when(row, 10),
        });
        match discord::send_embed(&text(row, 1), &card, Mention::None) {
            Ok(()) => {}
            // Not a channel of the app's any more, Discord not set up, or
            // a card the host refuses: it won't go later either.
            Err(discord::Error::NotAllowed(why) | discord::Error::Invalid(why)) => {
                log::warn(format!(
                    "a new request wasn't posted to the SRP team's channel: {why}"
                ));
                storage::execute(
                    "UPDATE outbox SET sent_at = NULL, failed = $2 WHERE id = $1",
                    &[id.into(), why.into()],
                )
                .map_err(|e| retry("marking a card", e))?;
            }
            // Rate limited or Discord down: released for later.
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
    // Sent cards are kept a week, for the record.
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
                .key(RELAY)
                .at(rfc3339(Utc::now() + Duration::seconds(gap))),
        )
        .map_err(|e| JobError::Retry(format!("queuing the relay: {e:?}")))?;
    }
    Ok(())
}

/// Ship Replacement's settings, for `manage`: aa-srp's Setting, changed in
/// Django's admin there.
fn settings_page() -> Result<Page, PageError> {
    let assigned = discord::channels();
    let stored = team_channel()?;
    let mut channels: Vec<(String, String)> = vec![(String::new(), "Not posted".to_owned())];
    channels.extend(
        assigned
            .iter()
            .map(|c| (c.id.clone(), format!("#{}", c.name))),
    );
    // The stored channel, while it's still the app's.
    let current = stored
        .clone()
        .filter(|c| assigned.iter().any(|a| a.id == *c))
        .unwrap_or_default();
    let how = "an admin adds a channel on the Discord page, then assigns it to Ship Replacement \
               under Administration, Apps.";
    let mut group = SettingsGroup::new("Discord");
    if assigned.is_empty() {
        group = group.description(format!(
            "No Discord channel is assigned to this app yet, so new requests aren't posted: {how}"
        ));
    } else if stored.is_some() && current.is_empty() {
        group = group.description(
            "The channel picked before is no longer assigned to this app, so new requests aren't \
             posted: pick another, or ask an admin to assign it again.",
        );
    } else if let Some(why) = last_failure()? {
        group = group.description(format!("The last new request wasn't posted: {why}."));
    }
    group = group.field(
        Field::select("channel", "Post new SRP requests to", channels)
            .value(current)
            .help(format!(
                "Each new request is posted there as a card, pinging nobody. To offer a channel \
                 here, {how}"
            )),
    );
    Ok(Page::new("Ship Replacement settings")
        .description("aa-srp's settings: where the SRP team hears of new requests.")
        .settings(SettingsForm::new("settings").group(group)))
}

/// Why the newest card that wasn't posted wasn't, if none was posted
/// since.
fn last_failure() -> Result<Option<String>, PageError> {
    Ok(query(
        "SELECT failed FROM outbox WHERE failed IS NOT NULL AND id > coalesce( \
             (SELECT max(id) FROM outbox WHERE sent_at IS NOT NULL AND failed IS NULL), 0) \
         ORDER BY id DESC LIMIT 1",
        &[],
    )?
    .first()
    .map(|r| text(r, 0)))
}

fn save_settings(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    if !viewer.can("manage") {
        return Err(PageError::Forbidden);
    }
    // Only one of the app's own channels.
    let value = submission.value("channel");
    let channel = discord::channels()
        .into_iter()
        .map(|c| c.id)
        .find(|id| id.as_str() == value);
    storage::execute(
        "UPDATE settings SET channel = $1 WHERE id = 1",
        &[channel.clone().into()],
    )
    .map_err(|e| failed("saving settings", e))?;
    log::info(format!(
        "settings changed by {} ({}): SRP team channel {channel:?}",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".to_owned()))
}

// ---- fleet view ----------------------------------------------------------------

/// A fleet's page of requests: the first is `fleet/<id>`, the rest
/// `fleet/<id>/page/<n>`.
fn fleet_path(fleet_id: i64, page_number: i64) -> String {
    if page_number > 1 {
        format!("fleet/{fleet_id}/page/{page_number}")
    } else {
        format!("fleet/{fleet_id}")
    }
}

/// A fleet's requests, for everyone with `access_srp` (AA's fleet view),
/// oldest first, `REQUEST_ROWS` a page; managers' buttons and the
/// requests' own pages for `srp_management`.
fn fleet_page(
    viewer: &Viewer,
    fleet_id: i64,
    page_number: i64,
    note: Option<&str>,
) -> Result<Page, PageError> {
    let f = fleet_by_id(fleet_id)?;
    let sums = query(&format!("{TOTALS} WHERE fleet_id = $1"), &[f.id.into()])?;
    let count = sums.first().map_or(0, |r| int(r, 0));
    let pages = ((count + REQUEST_ROWS - 1) / REQUEST_ROWS).max(1);
    if page_number > pages {
        return Err(PageError::NotFound);
    }
    let requests: Vec<Req> = query(
        &format!(
            "{REQUEST_SELECT} WHERE r.fleet_id = $1 ORDER BY r.created_at, r.id LIMIT $2 OFFSET $3"
        ),
        &[
            f.id.into(),
            REQUEST_ROWS.into(),
            ((page_number - 1) * REQUEST_ROWS).into(),
        ],
    )?
    .iter()
    .map(|r| request(r))
    .collect();
    let mut about = Card::new("SRP fleet")
        .field("Fleet Name", f.name.clone())
        .field("Doctrine", f.doctrine.clone())
        .field("Fleet Commander", f.fleet_commander.clone())
        .field("Fleet Time", time_or(f.time, ""))
        .field("Status", f.status())
        .field("SRP Code", f.code.clone())
        .field("Added by", f.created_by.clone());
    if !f.completed {
        // aa-srp's "Copy SRP link to clipboard": for fleet chat or Discord.
        about = about
            .field("Link to share", share(format!("request/{}", f.code)))
            .field(
                "Request link",
                link("Request SRP", format!("request/{}", f.code)),
            );
    }
    if !f.aar.is_empty() {
        about = about.field("After Action Report", cut(&f.aar, 1500));
    }
    let manage = manager(viewer);
    if manage {
        about = about.field("Actions", actions(fleet_buttons(&f)));
    }
    let mut columns = vec![
        Column::numeric("Requested"),
        Column::text("Character"),
        Column::text("Ship"),
        Column::numeric("Killmail"),
        Column::text("Additional Info"),
        Column::numeric("Loss value"),
        Column::numeric("Payout"),
        Column::text("Status"),
    ];
    // aa-srp's Approve and Reject (and Mark Paid) in the request's row.
    if manage {
        columns.push(Column::text(""));
    }
    // Which page, only when there's more than one.
    let mut table = Table::new(columns)
        .title(if pages > 1 {
            let first = (page_number - 1) * REQUEST_ROWS + 1;
            let last = (page_number * REQUEST_ROWS).min(count);
            format!("SRP Requests {first} to {last} of {count}")
        } else {
            "SRP Requests".to_owned()
        })
        .empty("No requests yet.");
    for r in &requests {
        let who: Value = if manage {
            link(r.character_name.clone(), format!("review/{}", r.id)).into()
        } else {
            pilot(r.character_id, &r.character_name)
        };
        let mut row = vec![
            time_or(r.created_at, ""),
            who,
            ship(r.ship_type_id, &r.ship_name),
            r.killmail_id.into(),
            cut(&r.info, 200).into(),
            isk_or(r.kb_total_loss, "unknown"),
            isk_or(r.payout, ""),
            r.status().into(),
        ];
        if manage {
            // A value holds 1 to 4 buttons: an empty cell for none.
            let buttons = request_buttons(r);
            row.push(if buttons.is_empty() {
                "".into()
            } else {
                actions(buttons)
            });
        }
        table = table.row(row);
    }
    let mut page = Page::new(f.name.clone()).description("SRP fleet data");
    if let Some(note) = note {
        page = page.text(note);
    }
    if let Some(row) = sums.first() {
        page = page.stats(total_stats(&totals(row), "this fleet"));
    }
    page = page.card(about).table(table);
    // Paging, oldest first: every request can be opened, however many.
    let mut more = Card::new("Pages");
    if page_number > 1 {
        more = more.field(
            "Earlier requests",
            link(
                format!("Page {} of {pages}", page_number - 1),
                fleet_path(f.id, page_number - 1),
            ),
        );
    }
    if page_number < pages {
        more = more.field(
            "Later requests",
            link(
                format!("Page {} of {pages}", page_number + 1),
                fleet_path(f.id, page_number + 1),
            ),
        );
    }
    if !more.fields.is_empty() {
        page = page.card(more);
    }
    Ok(page)
}

/// A fleet's buttons for SRP managers: Mark Completed (or Incomplete),
/// Mark Approved Paid and Remove Fleet. Each posts to `fleet_action`.
fn fleet_buttons(f: &Fleet) -> Vec<Action> {
    vec![
        if f.completed {
            action("Mark Incomplete", "reopen").confirm("Pilots can request SRP again.")
        } else {
            action("Mark Completed", "complete")
                .confirm("No more requests: finish reviewing the ones in.")
        },
        action("Mark Approved Paid", "pay_all")
            .confirm("Every approved request of this fleet is marked paid now."),
        action("Remove Fleet", "remove").tone(Tone::Danger).confirm(
            "The fleet, its requests and their comments are removed. Their losses can then be \
             requested again, as in AA.",
        ),
    ]
}

/// A request's buttons in its fleet's table, for SRP managers: Approve,
/// Reject and, once approved, Mark Paid. Each posts to `fleet_action` with
/// the request's id.
fn request_buttons(r: &Req) -> Vec<Action> {
    let on = |a: Action| a.field("request", r.id.to_string());
    let mut buttons = Vec::new();
    if r.status != "approved" {
        buttons.push(on(action("Approve", "decide")).field("decision", "approve"));
    }
    if r.status != "rejected" {
        buttons.push(
            on(action("Reject", "decide"))
                .field("decision", "reject")
                .tone(Tone::Danger)
                .confirm(format!(
                    "{}'s request for their {} is rejected (and no longer paid, if it was); they \
                     see it on their SRP page.",
                    r.character_name, r.ship_name
                )),
        );
    }
    if r.status == "approved" && !r.paid {
        buttons.push(on(action("Mark Paid", "paid")).confirm(format!(
            "{}'s {} is marked paid.",
            r.character_name, r.ship_name
        )));
    }
    buttons
}

/// A fleet's buttons, and its requests' (posted from `page_number`, where
/// the manager goes back to).
fn fleet_action(
    viewer: &Viewer,
    fleet_id: i64,
    page_number: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    need(manager(viewer))?;
    let f = fleet_by_id(fleet_id)?;
    let back = || Ok(SubmitResult::Redirect(fleet_path(fleet_id, page_number)));
    let who = format!("{} ({})", viewer.main.name, viewer.main.id);
    match submission.form.as_str() {
        form @ ("complete" | "reopen") => {
            // Set, not toggled: a second click (or a second manager)
            // changes nothing.
            let completed = form == "complete";
            storage::execute(
                "UPDATE fleets SET completed = $2 WHERE id = $1",
                &[f.id.into(), completed.into()],
            )
            .map_err(|e| failed("changing the fleet", e))?;
            log::info(format!(
                "SRP fleet {} marked {} by {who}",
                f.id,
                if completed { "completed" } else { "incomplete" }
            ));
            back()
        }
        "pay_all" => {
            let paid = storage::execute(
                "UPDATE requests SET paid = true, paid_at = now() \
                 WHERE fleet_id = $1 AND status = 'approved' AND NOT paid",
                &[f.id.into()],
            )
            .map_err(|e| failed("marking paid", e))?;
            log::info(format!(
                "{paid} SRP requests of fleet {} marked paid by {who}",
                f.id
            ));
            back()
        }
        "remove" => {
            storage::execute("DELETE FROM fleets WHERE id = $1", &[f.id.into()])
                .map_err(|e| failed("removing the fleet", e))?;
            log::info(format!("SRP fleet {} ({}) removed by {who}", f.id, f.name));
            Ok(SubmitResult::Redirect(String::new()))
        }
        // A request's row buttons: one of this fleet's.
        form @ ("decide" | "paid") => {
            let r = request_by_id(id(submission.value("request"))?)?;
            if r.fleet_id != f.id {
                return Err(PageError::NotFound);
            }
            let problem = if form == "paid" {
                mark_paid(viewer, &r)?
            } else {
                let approve = match submission.value("decision") {
                    "approve" => true,
                    "reject" => false,
                    _ => return Err(PageError::NotFound),
                };
                decide(viewer, &r, approve, "")?
            };
            match problem {
                Some(note) => Ok(SubmitResult::Page(fleet_page(
                    viewer,
                    fleet_id,
                    page_number,
                    Some(note),
                )?)),
                None => back(),
            }
        }
        _ => Err(PageError::NotFound),
    }
}

// ---- reviewing ---------------------------------------------------------------

fn request_by_id(request_id: i64) -> Result<Req, PageError> {
    query(
        &format!("{REQUEST_SELECT} WHERE r.id = $1"),
        &[request_id.into()],
    )?
    .first()
    .map(|r| request(r))
    .ok_or(PageError::NotFound)
}

fn review_page(viewer: &Viewer, request_id: i64, note: Option<&str>) -> Result<Page, PageError> {
    need(manager(viewer))?;
    let r = request_by_id(request_id)?;
    let mut about = Card::new("SRP request")
        .field(
            "Fleet",
            link(r.fleet_name.clone(), format!("fleet/{}", r.fleet_id)),
        )
        .field("Character", pilot(r.character_id, &r.character_name))
        .field("Ship", ship(r.ship_type_id, &r.ship_name))
        .field("Lost", time_or(r.killmail_time, ""))
        .field("Killboard Link", cut(&r.link, 300))
        .field("Killmail", r.killmail_id)
        .field(
            "Loss value (zKillboard)",
            isk_or(r.kb_total_loss, "unknown"),
        )
        .field("Payout", isk_or(r.payout, "not set"))
        .field("Status", r.status())
        .field("Requested", time_or(r.created_at, ""));
    if !r.reviewer.is_empty() {
        about = about.field("Reviewer", r.reviewer.clone());
    }
    if let Some(decided) = r.decided_at {
        about = about.field("Decided", time(rfc3339(decided)));
    }
    if let Some(paid) = r.paid_at {
        about = about.field("Paid", time(rfc3339(paid)));
    }
    if !r.info.is_empty() {
        about = about.field("Additional Info", cut(&r.info, 1500));
    }
    if r.status == "approved" && !r.paid {
        about = about.field(
            "Payment",
            action("Mark Paid", "paid").confirm(format!(
                "{}'s {} is marked paid.",
                r.character_name, r.ship_name
            )),
        );
    }
    let comments = query(
        "SELECT created_at, author_name, body FROM comments WHERE request_id = $1 \
         ORDER BY created_at, id LIMIT $2",
        &[r.id.into(), MAX_COMMENTS.into()],
    )?;
    let mut table = Table::new(vec![
        Column::numeric("When"),
        Column::text("By"),
        Column::text("Comment"),
    ])
    .title("Comments")
    .empty("No comments yet.");
    for c in &comments {
        table = table.row(vec![
            time_or(when(c, 0), ""),
            text(c, 1).into(),
            cut(&text(c, 2), 600).into(),
        ]);
    }
    let mut page = Page::new("SRP request").description(format!(
        "{}'s {} on {}",
        r.character_name, r.ship_name, r.fleet_name
    ));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.card(about).table(table);
    page = page.form(
        Form::new("decide", "Save Decision")
            .description(
                "Approving without a payout set pays zKillboard's value; rejecting a paid request \
                 unmarks it paid. The pilot sees the decision on their SRP page.",
            )
            .field(
                Field::select(
                    "decision",
                    "Decision",
                    vec![
                        ("approve".to_owned(), "Approve".to_owned()),
                        ("reject".to_owned(), "Reject".to_owned()),
                    ],
                )
                .required(),
            )
            .field(Field::textarea("comment", "Comment", MAX_COMMENT)),
    );
    // AA's update amount: at any time, whatever the status.
    let mut amount = Field::number("payout", "Payout (ISK)")
        .range(Some(0.0), Some(MAX_PAYOUT), true)
        .required();
    if let Some(p) = r.payout.or(r.kb_total_loss) {
        amount = amount.value(format!("{}", p.round() as i64));
    }
    page = page.form(
        Form::new("payout", "Update Payout")
            .description("What this loss pays out, whole ISK. The status stays as it is.")
            .field(amount)
            .field(Field::textarea("comment", "Comment", MAX_COMMENT)),
    );
    Ok(page.form(
        Form::new("comment", "Add Comment")
            .description("Only SRP staff see comments.")
            .field(Field::textarea("comment", "Comment", MAX_COMMENT).required()),
    ))
}

/// Adds a comment (at most `MAX_COMMENTS` per request); false if full.
fn add_comment(viewer: &Viewer, request_id: i64, body: &str) -> Result<bool, PageError> {
    let added = storage::execute(
        "INSERT INTO comments (request_id, author_account_id, author_name, body) \
         SELECT $1, $2, $3, $4 \
         WHERE (SELECT count(*) FROM comments WHERE request_id = $1) < $5",
        &[
            request_id.into(),
            viewer.account_id.into(),
            viewer.main.name.clone().into(),
            body.into(),
            MAX_COMMENTS.into(),
        ],
    )
    .map_err(|e| failed("adding the comment", e))?;
    Ok(added > 0)
}

/// Approves or rejects a request, with the comment (if any) on the
/// record. Callers check `srp_management`. What went wrong, for the page,
/// if it couldn't be decided.
fn decide(
    viewer: &Viewer,
    r: &Req,
    approve: bool,
    comment: &str,
) -> Result<Option<&'static str>, PageError> {
    // At any time, as AA. Approving keeps a payout set earlier, else pays
    // zKillboard's value; rejecting unmarks a paid request (only approved
    // ones are paid).
    let changed = storage::execute(
        "UPDATE requests SET status = $2, \
           payout = CASE WHEN $3 THEN coalesce(payout, kb_total_loss) ELSE payout END, \
           paid = paid AND $3, paid_at = CASE WHEN $3 THEN paid_at END, \
           reviewer_name = $4, decided_at = now() \
         WHERE id = $1",
        &[
            r.id.into(),
            if approve { "approved" } else { "rejected" }.into(),
            approve.into(),
            viewer.main.name.clone().into(),
        ],
    )
    .map_err(|e| failed("saving the decision", e))?;
    if changed == 0 {
        return Ok(Some("That request is gone."));
    }
    // Every decision is on the record, with the comment if any (and that
    // it had been paid, if a paid request is rejected).
    let word = match (approve, r.paid) {
        (true, _) => "Approved",
        (false, true) => "Rejected (it had been marked paid)",
        (false, false) => "Rejected",
    };
    let line = if comment.is_empty() {
        format!("{word}.")
    } else {
        format!("{word}: {comment}")
    };
    if !add_comment(viewer, r.id, &line)? {
        return Ok(Some(
            "Decision saved, but this request holds no more comments.",
        ));
    }
    log::info(format!(
        "SRP request {} {} by {} ({})",
        r.id,
        if approve { "approved" } else { "rejected" },
        viewer.main.name,
        viewer.main.id
    ));
    Ok(None)
}

/// Marks an approved request paid, once. Callers check `srp_management`.
fn mark_paid(viewer: &Viewer, r: &Req) -> Result<Option<&'static str>, PageError> {
    let changed = storage::execute(
        "UPDATE requests SET paid = true, paid_at = now() \
         WHERE id = $1 AND status = 'approved' AND NOT paid",
        &[r.id.into()],
    )
    .map_err(|e| failed("marking paid", e))?;
    if changed == 0 {
        return Ok(Some("Only approved requests can be paid, once."));
    }
    add_comment(viewer, r.id, "Marked paid.")?;
    log::info(format!(
        "SRP request {} marked paid by {} ({})",
        r.id, viewer.main.name, viewer.main.id
    ));
    Ok(None)
}

fn review_action(
    viewer: &Viewer,
    request_id: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    need(manager(viewer))?;
    let r = request_by_id(request_id)?;
    let back = || Ok(SubmitResult::Redirect(format!("review/{request_id}")));
    let note = |text: &str| {
        Ok(SubmitResult::Page(review_page(
            viewer,
            request_id,
            Some(text),
        )?))
    };
    let who = format!("{} ({})", viewer.main.name, viewer.main.id);
    let comment = submission.value("comment").trim().to_owned();
    match submission.form.as_str() {
        "decide" => {
            let approve = match submission.value("decision") {
                "approve" => true,
                "reject" => false,
                _ => return Err(PageError::NotFound),
            };
            match decide(viewer, &r, approve, &comment)? {
                Some(problem) => note(problem),
                None => back(),
            }
        }
        "payout" => {
            let amount: f64 = submission
                .value("payout")
                .parse()
                .map_err(|_| PageError::Failed("payout wasn't a number".to_owned()))?;
            if !(0.0..=MAX_PAYOUT).contains(&amount) {
                return note("That payout is out of range.");
            }
            // AA's update amount: the status stays.
            let changed = storage::execute(
                "UPDATE requests SET payout = $2 WHERE id = $1",
                &[r.id.into(), amount.into()],
            )
            .map_err(|e| failed("saving the payout", e))?;
            if changed == 0 {
                return note("That request is gone.");
            }
            let line = if comment.is_empty() {
                format!("Payout set to {amount:.0} ISK.")
            } else {
                format!("Payout set to {amount:.0} ISK: {comment}")
            };
            // A note for the record; a full comment list doesn't block it.
            add_comment(viewer, r.id, &line)?;
            log::info(format!(
                "SRP request {} payout set to {amount:.0} ISK by {who}",
                r.id
            ));
            back()
        }
        "paid" => match mark_paid(viewer, &r)? {
            Some(problem) => note(problem),
            None => back(),
        },
        "comment" => {
            if comment.is_empty() {
                return note("Write a comment first.");
            }
            if !add_comment(viewer, r.id, &comment)? {
                return note(&format!("A request holds at most {MAX_COMMENTS} comments."));
            }
            back()
        }
        _ => Err(PageError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn rifter(info: &str) -> TeamRequest {
        TeamRequest {
            fleet_name: "Op Rock".to_owned(),
            srp_code: "ABCDEF0123456789".to_owned(),
            character_id: 443630591,
            character_name: "Pilot A".to_owned(),
            ship_type_id: 587,
            ship_name: "Rifter".to_owned(),
            killmail_id: 1001,
            info: info.to_owned(),
            requested: DateTime::parse_from_rfc3339("2026-09-20T19:30:00Z")
                .ok()
                .map(|t| t.with_timezone(&Utc)),
        }
    }

    #[test]
    fn a_new_request_is_posted_as_aa_srp_tells_it() {
        let card = team_card(&rifter(""));
        assert_eq!(card.title, "New SRP request: Rifter");
        let author = card.author.as_ref().unwrap();
        assert_eq!(author.name, "Pilot A");
        assert!(matches!(author.icon, Some(Image::Character(443630591))));
        assert!(matches!(card.thumbnail, Some(Image::TypeRender(587))));
        assert_eq!(card.color, Some(0x5b_c0de));
        let fields: Vec<(&str, &str)> = card
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.value.as_str()))
            .collect();
        assert_eq!(
            fields,
            vec![
                ("SRP fleet", "Op Rock"),
                ("SRP code", "ABCDEF0123456789"),
                (
                    "Killmail",
                    "[zKillboard](https://zkillboard.com/kill/1001/)"
                ),
            ]
        );
        assert_eq!(card.footer.as_deref(), Some("Ship Replacement"));
        assert_eq!(card.timestamp.as_deref(), Some("2026-09-20T19:30:00Z"));
        // No additional info, no description (as aa-srp's).
        assert!(card.description.is_none());
        assert!(team_card(&rifter("  \n ")).description.is_none());
    }

    #[test]
    fn additional_info_cannot_ping_link_or_overflow() {
        let card = team_card(&rifter("**x** @everyone https://evil"));
        let text = card.description.unwrap();
        assert!(
            text.starts_with("\\*\\*x\\*\\* \\@everyone https:"),
            "{text}"
        );
        assert!(!text.contains("://"), "{text}");
        // Escaping doubles markdown: still within the host's 2,000.
        let long = team_card(&rifter(&"*".repeat(1000))).description.unwrap();
        assert!(long.chars().count() <= 2000, "{}", long.chars().count());
        assert!(long.ends_with('…'));
    }

    #[test]
    fn names_cannot_ping_or_link() {
        assert_eq!(escape("@here [x](y)"), "\\@here \\[x\\]\\(y\\)");
        assert!(!escape("https://evil.example").contains("://"));
    }
}
