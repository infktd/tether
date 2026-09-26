//! Ship Replacement (Alliance Auth's srp; PRD F23).
//!
//! - **SRP fleets** (`add_srpfleetmain` adds them): a fleet name, doctrine,
//!   fleet commander and EVE time, an after action report, and an SRP
//!   code pilots request with. Open until SRP staff mark them Completed.
//! - **Request SRP** (`access_srp`): a pilot pastes a zKillboard or ESI
//!   killmail link for a loss on an open fleet and picks the character
//!   who lost the ship. The loss comes from ESI's public killmail endpoint
//!   (through Tether), its value from zKillboard (over the app's approved
//!   HTTPS host). The victim must be the chosen character, one of the
//!   pilot's own; each loss can be requested once.
//! - **Reviewing**: SRP staff (`manage_srp`) see a fleet's requests,
//!   approve (the payout defaults to zKillboard's value) or reject them
//!   with a comment, and mark approved ones paid; `change_srpuserrequest`
//!   adjusts payouts. Totals per fleet (requested, approved, paid) and
//!   overall.
//!
//! zKillboard asks for gentle use: a kill's value is fetched once and
//! kept, and only when a pilot requests SRP for it.

mod killmail;

use chrono::{DateTime, NaiveDateTime, Utc};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::http;
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Badge, Card, Column, Field, Form, Page, PageError, Plugin, Request, Stat, Submission,
    SubmitResult, Table, Tone, Value, badge, isk, link, log, time,
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
/// Rows on one page (the host allows 500).
const FLEET_ROWS: i64 = 200;
const REQUEST_ROWS: i64 = 400;
const MY_ROWS: i64 = 100;
/// The biggest payout a reviewer may set: more than any ship is worth.
const MAX_PAYOUT: f64 = killmail::MAX_VALUE;
/// Links that don't check out, per pilot, before they wait a while.
const MAX_FAILED_LOOKUPS: i64 = 5;

struct ShipReplacement;

impl Plugin for ShipReplacement {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let parts: Vec<&str> = request.path.split('/').collect();
        match parts.as_slice() {
            [""] => srp_fleets(&viewer),
            ["add"] => add_page(&viewer, None),
            ["fleet", fleet] => fleet_page(&viewer, id(fleet)?, None),
            ["request", code] => request_page(&viewer, code, None),
            ["review", request] => review_page(&viewer, id(request)?, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = submission.request.path.clone();
        let parts: Vec<&str> = path.split('/').collect();
        match (parts.as_slice(), submission.form.as_str()) {
            (["add"], "add_fleet") => add_fleet(&viewer, &submission),
            (["fleet", fleet], _) => fleet_action(&viewer, id(fleet)?, &submission),
            (["request", code], "request") => request_srp(&viewer, code, &submission),
            (["review", request], _) => review_action(&viewer, id(request)?, &submission),
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(ShipReplacement);

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

/// Who may see fleets' requests: SRP staff.
fn staff(viewer: &Viewer) -> bool {
    viewer.can("manage_srp") || viewer.can("change_srpuserrequest")
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

/// Sums over a fleet's requests (or all of them): requested (zKillboard's
/// value), approved and paid payouts, and counts.
struct Totals {
    requests: i64,
    pending: i64,
    requested: f64,
    approved: f64,
    paid: f64,
}

const TOTALS: &str = "SELECT count(*), count(*) FILTER (WHERE status = 'pending'), \
     coalesce(sum(kb_total_loss), 0)::float8, \
     coalesce(sum(payout) FILTER (WHERE status = 'approved'), 0)::float8, \
     coalesce(sum(payout) FILTER (WHERE status = 'approved' AND paid), 0)::float8 \
     FROM requests";

fn totals(row: &[Db]) -> Totals {
    Totals {
        requests: int(row, 0),
        pending: int(row, 1),
        requested: float(row, 2).unwrap_or_default(),
        approved: float(row, 3).unwrap_or_default(),
        paid: float(row, 4).unwrap_or_default(),
    }
}

fn total_stats(t: &Totals) -> Vec<Stat> {
    vec![
        Stat::new("Requests", t.requests),
        Stat::new("Pending", t.pending).caption("waiting for a decision"),
        Stat::new("Losses", isk(t.requested)).caption("zKillboard's value"),
        Stat::new("Total ISK Cost", isk(t.approved)).caption("approved payouts"),
        Stat::new("Paid", isk(t.paid)),
        Stat::new("Outstanding", isk(t.approved - t.paid)).caption("approved, not paid"),
    ]
}

fn srp_fleets(viewer: &Viewer) -> Result<Page, PageError> {
    let staff = staff(viewer);
    let fleets: Vec<(Fleet, i64, f64)> = query(
        &format!(
            "SELECT {FLEET_COLUMNS}, \
             (SELECT count(*) FROM requests r WHERE r.fleet_id = f.id AND r.status = 'pending'), \
             (SELECT coalesce(sum(r.payout), 0)::float8 FROM requests r \
                WHERE r.fleet_id = f.id AND r.status = 'approved') \
             FROM fleets f ORDER BY f.completed, f.fleet_time DESC, f.id DESC LIMIT $1"
        ),
        &[FLEET_ROWS.into()],
    )?
    .iter()
    .map(|r| (fleet(r), int(r, 9), float(r, 10).unwrap_or_default()))
    .collect();
    let mut columns = vec![
        Column::text("Fleet Name"),
        Column::numeric("Fleet Time"),
        Column::text("Doctrine"),
        Column::text("Fleet Commander"),
        Column::text("Status"),
    ];
    if staff {
        columns.push(Column::numeric("Pending"));
        columns.push(Column::numeric("Total ISK Cost"));
    }
    columns.push(Column::text("SRP"));
    let mut table = Table::new(columns)
        .title("SRP Fleets")
        .empty("No SRP fleets yet.");
    for (f, pending, cost) in &fleets {
        let name: Value = if staff {
            link(f.name.clone(), format!("fleet/{}", f.id)).into()
        } else {
            f.name.clone().into()
        };
        let mut row = vec![
            name,
            time_or(f.time, ""),
            f.doctrine.clone().into(),
            f.fleet_commander.clone().into(),
            f.status().into(),
        ];
        if staff {
            row.push((*pending).into());
            row.push(isk(*cost));
        }
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
    .title("My SRP Requests")
    .empty("You haven't requested SRP yet.");
    for r in &mine {
        my = my.row(vec![
            r.fleet_name.clone().into(),
            r.character_name.clone().into(),
            r.ship_name.clone().into(),
            time_or(r.killmail_time, ""),
            isk_or(r.kb_total_loss, "unknown"),
            isk_or(r.payout, ""),
            r.status().into(),
        ]);
    }

    let mut page = Page::new("Ship Replacement")
        .description("SRP fleets, and your requests for your losses on them");
    if staff {
        let all = query(TOTALS, &[])?;
        if let Some(row) = all.first() {
            page = page.stats(total_stats(&totals(row)));
        }
    }
    page = page.table(table).table(my);
    if viewer.can("add_srpfleetmain") {
        page = page.card(Card::new("SRP staff").field("New fleet", link("Add SRP Fleet", "add")));
    }
    Ok(page)
}

fn add_page(viewer: &Viewer, note: Option<&str>) -> Result<Page, PageError> {
    need(viewer.can("add_srpfleetmain"))?;
    let now = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let form = Form::new("add_fleet", "Create SRP Fleet")
        .description("Pilots request SRP with the fleet's SRP code, until you mark it Completed.")
        .field(Field::text("name", "Fleet Name", MAX_NAME).required())
        .field(Field::text("doctrine", "Fleet Doctrine", MAX_DOCTRINE).required())
        .field(
            Field::text("fleet_commander", "Fleet Commander", MAX_FC)
                .required()
                .value(viewer.main.name.clone()),
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
    let mut page = Page::new("Add SRP Fleet");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page.form(form))
}

fn add_fleet(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    need(viewer.can("add_srpfleetmain"))?;
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
    Ok(SubmitResult::Redirect(if staff(viewer) {
        format!("fleet/{fleet_id}")
    } else {
        String::new()
    }))
}

// ---- requests ----------------------------------------------------------------

struct Req {
    id: i64,
    account_id: i64,
    fleet_time: Option<DateTime<Utc>>,
    fleet_id: i64,
    fleet_name: String,
    character_name: String,
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
     r.account_id, f.fleet_time \
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
        account_id: int(row, 17),
        fleet_time: when(row, 18),
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

fn request_page(viewer: &Viewer, code: &str, note: Option<&str>) -> Result<Page, PageError> {
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
    let characters = viewer
        .characters
        .iter()
        .take(100)
        .map(|c| (c.id.to_string(), c.name.clone()))
        .collect();
    Ok(page.form(
        Form::new("request", "Request SRP")
            .description(
                "Tether reads the loss from EVE and its value from zKillboard. It must be a loss \
                 of the character you pick, and each loss can be requested once.",
            )
            .field(
                Field::text("killboard_link", "Killboard Link", MAX_LINK)
                    .required()
                    .help("zKillboard (https://zkillboard.com/kill/…) or an ESI killmail link"),
            )
            .field(Field::select("character", "Character", characters).required())
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
    let again = |text: &str| Ok(SubmitResult::Page(request_page(viewer, code, Some(text))?));
    if f.completed {
        return again("This fleet's SRP is completed: it takes no more requests.");
    }
    let link_text = submission.value("killboard_link").trim().to_owned();
    let Some(link) = killmail::parse_link(&link_text) else {
        return again(
            "That isn't a killmail link: paste a zKillboard kill (https://zkillboard.com/kill/…) \
             or an ESI killmail link.",
        );
    };
    let chosen: i64 = submission.value("character").parse().unwrap_or_default();
    let Some(character) = viewer.characters.iter().find(|c| c.id == chosen) else {
        return Err(PageError::Forbidden);
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
    if !query(
        "SELECT 1 FROM claimed_kills WHERE killmail_id = $1",
        &[link.id.into()],
    )?
    .is_empty()
    {
        return again("SRP has already been requested for this loss.");
    }
    let zkb = zkb_value(link.id)?;
    let hash = match (&link.hash, &zkb) {
        // A hash zKillboard contradicts isn't worth asking EVE about.
        (Some(hash), Some(zkb)) if *hash != zkb.hash => {
            return refused("That link's hash doesn't match the kill: check the link.");
        }
        (Some(hash), _) => hash.clone(),
        (None, Some(zkb)) => zkb.hash.clone(),
        (None, None) => {
            return refused(
                "zKillboard doesn't know that kill yet, or couldn't be reached. Try again in a few \
                 minutes, or paste the ESI killmail link from the game.",
            );
        }
    };
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
    // The victim must be one of the pilot's own characters, the one they
    // picked.
    match km.victim_character_id {
        Some(victim) if victim == character.id => {}
        Some(victim) => {
            return match viewer.characters.iter().find(|c| c.id == victim) {
                Some(other) => again(&format!(
                    "That loss is {}'s: pick {} as the character.",
                    other.name, other.name
                )),
                None => refused("That loss isn't one of your characters'."),
            };
        }
        None => {
            return refused("That loss has no pilot: SRP is for ships your characters lost.");
        }
    }
    let ship_name = esi::names(&[km.ship_type_id])
        .ok()
        .and_then(|names| names.into_iter().find(|n| n.id == km.ship_type_id))
        .map_or_else(|| format!("Type {}", km.ship_type_id), |n| n.name);
    // The claim and the request together: each loss once, ever.
    let added = storage::query(
        "WITH claim AS ( \
           INSERT INTO claimed_kills (killmail_id) \
           SELECT $5 WHERE EXISTS (SELECT 1 FROM fleets WHERE id = $1 AND NOT completed) \
           ON CONFLICT (killmail_id) DO NOTHING RETURNING killmail_id) \
         INSERT INTO requests (fleet_id, account_id, character_id, character_name, killmail_id, \
             killmail_hash, killboard_link, ship_type_id, ship_name, solar_system_id, \
             killmail_time, kb_total_loss, additional_info) \
         SELECT $1, $2, $3, $4, claim.killmail_id, $6, $7, $8, $9, $10, $11, $12, $13 \
         FROM claim RETURNING id",
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
            zkb.map(|z| z.total_value).into(),
            submission.value("additional_info").trim().to_owned().into(),
        ],
    )
    .map_err(|e| failed("saving the request", e))?;
    if added.rows.is_empty() {
        return again("SRP has already been requested for this loss.");
    }
    log::info(format!(
        "SRP requested on fleet {} for {} lost by {} ({}), kill {}",
        f.id, ship_name, character.name, character.id, link.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}

// ---- fleet view ----------------------------------------------------------------

fn fleet_page(viewer: &Viewer, fleet_id: i64, note: Option<&str>) -> Result<Page, PageError> {
    need(staff(viewer))?;
    let f = fleet_by_id(fleet_id)?;
    let requests: Vec<Req> = query(
        &format!("{REQUEST_SELECT} WHERE r.fleet_id = $1 ORDER BY r.created_at, r.id LIMIT $2"),
        &[f.id.into(), REQUEST_ROWS.into()],
    )?
    .iter()
    .map(|r| request(r))
    .collect();
    let sums = query(&format!("{TOTALS} WHERE fleet_id = $1"), &[f.id.into()])?;
    let mut about = Card::new("SRP Fleet")
        .field("Fleet Name", f.name.clone())
        .field("Doctrine", f.doctrine.clone())
        .field("Fleet Commander", f.fleet_commander.clone())
        .field("Fleet Time", time_or(f.time, ""))
        .field("Status", f.status())
        .field("SRP Code", f.code.clone())
        .field("Added by", f.created_by.clone());
    if !f.completed {
        about = about.field(
            "Request link",
            link("Request SRP", format!("request/{}", f.code)),
        );
    }
    if !f.aar.is_empty() {
        about = about.field("After Action Report", cut(&f.aar, 1500));
    }
    let mut table = Table::new(vec![
        Column::numeric("Requested"),
        Column::text("Character"),
        Column::text("Ship"),
        Column::numeric("Killmail"),
        Column::numeric("Loss value"),
        Column::numeric("Payout"),
        Column::text("Status"),
    ])
    .title("SRP Requests")
    .empty("No requests yet.");
    for r in &requests {
        table = table.row(vec![
            time_or(r.created_at, ""),
            link(r.character_name.clone(), format!("review/{}", r.id)).into(),
            r.ship_name.clone().into(),
            r.killmail_id.into(),
            isk_or(r.kb_total_loss, "unknown"),
            isk_or(r.payout, ""),
            r.status().into(),
        ]);
    }
    let mut page = Page::new(f.name.clone()).description("SRP Fleet Data");
    if let Some(note) = note {
        page = page.text(note);
    }
    if let Some(row) = sums.first() {
        page = page.stats(total_stats(&totals(row)));
    }
    page = page.card(about).table(table);
    if viewer.can("manage_srp") {
        page = page
            .form(if f.completed {
                Form::new("reopen", "Mark Incomplete")
                    .description("Pilots can request SRP again.")
                    .field(Field::checkbox("confirm", "Yes", true).required())
            } else {
                Form::new("complete", "Mark Completed")
                    .description("No more requests: finish reviewing the ones in.")
                    .field(Field::checkbox("confirm", "Yes", true).required())
            })
            .form(
                Form::new("pay_all", "Mark Approved Paid")
                    .description(
                        "Every approved request of this fleet, paid now (except your own).",
                    )
                    .field(Field::checkbox("confirm", "Yes, they're paid", false).required()),
            )
            .form(
                Form::new("remove", "Remove Fleet")
                    .description(
                        "Its requests and their comments go with it. Their losses stay claimed: \
                         they can't be requested again.",
                    )
                    .field(Field::checkbox("confirm", "Yes, remove this fleet", false).required()),
            );
    }
    Ok(page)
}

fn fleet_action(
    viewer: &Viewer,
    fleet_id: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    need(viewer.can("manage_srp"))?;
    let f = fleet_by_id(fleet_id)?;
    let back = || Ok(SubmitResult::Redirect(format!("fleet/{fleet_id}")));
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
            // Not the manager's own: someone else pays those.
            let paid = storage::execute(
                "UPDATE requests SET paid = true, paid_at = now() \
                 WHERE fleet_id = $1 AND status = 'approved' AND NOT paid AND account_id <> $2",
                &[f.id.into(), viewer.account_id.into()],
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
    need(staff(viewer))?;
    let r = request_by_id(request_id)?;
    let mut about = Card::new("SRP Request")
        .field(
            "Fleet",
            link(r.fleet_name.clone(), format!("fleet/{}", r.fleet_id)),
        )
        .field("Character", r.character_name.clone())
        .field("Ship", r.ship_name.clone())
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
    // Flagged, not refused (as AA): a loss far from the fleet's time.
    if let (Some(lost), Some(fleet)) = (r.killmail_time, r.fleet_time)
        && (lost - fleet).num_hours().abs() >= 24
    {
        about = about.field(
            "Check",
            badge("Lost more than a day from the fleet time", Tone::Warning),
        );
    }
    let own = r.account_id == viewer.account_id;
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
    let mut page = Page::new("SRP Request").description(format!(
        "{}'s {} on {}",
        r.character_name, r.ship_name, r.fleet_name
    ));
    if let Some(note) = note {
        page = page.text(note);
    }
    page = page.card(about).table(table);
    if own {
        page = page.text("This is your own request: someone else on SRP staff decides it.");
    }
    if viewer.can("manage_srp") && !r.paid && !own {
        page = page.form(
            Form::new("decide", "Save Decision")
                .description(
                    "Approving without a payout set pays zKillboard's value. The pilot sees the \
                     decision on their SRP page.",
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
    }
    if viewer.can("change_srpuserrequest") && !r.paid && r.status != "rejected" && !own {
        let mut amount = Field::number("payout", "Payout (ISK)")
            .range(Some(0.0), Some(MAX_PAYOUT), true)
            .required();
        if let Some(p) = r.payout.or(r.kb_total_loss) {
            amount = amount.value(format!("{}", p.round() as i64));
        }
        page = page.form(
            Form::new("payout", "Update Payout")
                .description(
                    "What this loss pays out, whole ISK. Changing an approved payout sends it \
                     back to pending, for a manager to approve again.",
                )
                .field(amount)
                .field(Field::textarea("comment", "Comment", MAX_COMMENT)),
        );
    }
    if viewer.can("manage_srp") && r.status == "approved" && !r.paid && !own {
        page = page.form(
            Form::new("paid", "Mark Paid")
                .field(Field::checkbox("confirm", "Yes, it's paid", false).required()),
        );
    }
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

fn review_action(
    viewer: &Viewer,
    request_id: i64,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    need(staff(viewer))?;
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
    // Nobody decides, prices or pays their own request.
    if r.account_id == viewer.account_id && submission.form != "comment" {
        return Err(PageError::Forbidden);
    }
    match submission.form.as_str() {
        "decide" => {
            need(viewer.can("manage_srp"))?;
            let approve = match submission.value("decision") {
                "approve" => true,
                "reject" => false,
                _ => return Err(PageError::NotFound),
            };
            // Not once paid, nor by its own pilot. Approving keeps a
            // payout set earlier, else pays zKillboard's value.
            let changed = storage::execute(
                "UPDATE requests SET status = $2, \
                   payout = CASE WHEN $3 THEN coalesce(payout, kb_total_loss) ELSE payout END, \
                   reviewer_name = $4, decided_at = now() \
                 WHERE id = $1 AND NOT paid AND account_id <> $5",
                &[
                    r.id.into(),
                    if approve { "approved" } else { "rejected" }.into(),
                    approve.into(),
                    viewer.main.name.clone().into(),
                    viewer.account_id.into(),
                ],
            )
            .map_err(|e| failed("saving the decision", e))?;
            if changed == 0 {
                return note("It's paid already: nothing to decide.");
            }
            // Every decision is on the record, with the comment if any.
            let word = if approve { "Approved" } else { "Rejected" };
            let line = if comment.is_empty() {
                format!("{word}.")
            } else {
                format!("{word}: {comment}")
            };
            if !add_comment(viewer, r.id, &line)? {
                return note("Decision saved, but this request holds no more comments.");
            }
            log::info(format!(
                "SRP request {} {} by {who}",
                r.id,
                if approve { "approved" } else { "rejected" }
            ));
            back()
        }
        "payout" => {
            need(viewer.can("change_srpuserrequest"))?;
            let amount: f64 = submission
                .value("payout")
                .parse()
                .map_err(|_| PageError::Failed("payout wasn't a number".to_owned()))?;
            if !(0.0..=MAX_PAYOUT).contains(&amount) {
                return note("That payout is out of range.");
            }
            // An approved request goes back to pending: a manager approves
            // the new amount before it can be paid.
            let changed = storage::execute(
                "UPDATE requests SET payout = $2, status = 'pending', decided_at = NULL \
                 WHERE id = $1 AND NOT paid AND status <> 'rejected' AND account_id <> $3",
                &[r.id.into(), amount.into(), viewer.account_id.into()],
            )
            .map_err(|e| failed("saving the payout", e))?;
            if changed == 0 {
                return note("Paid or rejected requests keep their payout.");
            }
            let again = if r.status == "approved" {
                " Back to pending."
            } else {
                ""
            };
            let line = if comment.is_empty() {
                format!("Payout set to {amount:.0} ISK.{again}")
            } else {
                format!("Payout set to {amount:.0} ISK: {comment}{again}")
            };
            // A note for the record; a full comment list doesn't block it.
            add_comment(viewer, r.id, &line)?;
            log::info(format!(
                "SRP request {} payout set to {amount:.0} ISK by {who}",
                r.id
            ));
            back()
        }
        "paid" => {
            need(viewer.can("manage_srp"))?;
            let changed = storage::execute(
                "UPDATE requests SET paid = true, paid_at = now() \
                 WHERE id = $1 AND status = 'approved' AND NOT paid AND account_id <> $2",
                &[r.id.into(), viewer.account_id.into()],
            )
            .map_err(|e| failed("marking paid", e))?;
            if changed == 0 {
                return note("Only approved requests can be paid, once.");
            }
            add_comment(viewer, r.id, "Marked paid.")?;
            log::info(format!("SRP request {} marked paid by {who}", r.id));
            back()
        }
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
