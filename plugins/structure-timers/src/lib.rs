//! Structure Timers (Alliance Auth's timerboard; PRD F23).
//!
//! Timers for structures: the structure type, the timer type, objective
//! (Friendly, Hostile, Neutral), system, planet or moon, EVE time and
//! details, flagged important or corporation-only.
//!
//! - `timer_view` sees upcoming and past timers, with countdowns.
//! - `timer_management` creates, edits and deletes them.
//! - A corporation-only timer is seen, and edited, only by pilots whose
//!   main is in the corporation of the creator's main (as AA; stricter
//!   than AA, which lets any manager edit it by its address).
//! - Automatic timers: those other apps publish (Structures' own
//!   structures, as aa-structures feeds the timerboard), listed with the
//!   rest, marked with the app's name, and changed only there. The host
//!   passes on a corporation-only one only to that corporation.

mod when;

use chrono::{DateTime, Utc};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Lane, LaneItem, Page, PageError, Plugin, Request, Section, Stat,
    Submission, SubmitResult, Table, Timeline, Tone, Value, action, badge, character, countdown,
    link, log, time,
};

/// AA's structure choices.
const STRUCTURES: &[&str] = &[
    "Astrahus",
    "Fortizar",
    "Keepstar",
    "Raitaru",
    "Azbel",
    "Sotiyo",
    "Athanor",
    "Tatara",
    "Metenox Moon Drill",
    "Ansiblex Jump Gate",
    "Pharolux Cyno Beacon",
    "Tenebrex Cyno Jammer",
    "Sovereignty Hub",
    "Orbital Skyhook",
    "POCO",
    "Mercenary Den",
    "POS (Small)",
    "POS (Medium)",
    "POS (Large)",
    "Other",
];

/// AA's timer types.
const TIMER_TYPES: &[&str] = &[
    "Not Specified",
    "Shield",
    "Armor",
    "Hull",
    "Final",
    "Anchoring",
    "Unanchoring",
    "Abandoned",
    "Theft",
];

const OBJECTIVES: &[&str] = &["Friendly", "Hostile", "Neutral"];

/// AA's field lengths.
const MAX_TEXT: u32 = 254;
/// Rows per list at most; long texts cut them shorter (`LIST_BYTES`).
const UPCOMING_ROWS: i64 = 500;
const PAST_ROWS: i64 = 200;
/// Both tabs' rows together, well under the host's 1 MiB a page.
const LIST_BYTES: usize = 600 * 1024;

struct StructureTimers;

impl Plugin for StructureTimers {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if let Some(id) = path.strip_prefix("timer/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return edit_page(&viewer, id, None, None);
        }
        match path {
            "" => timers_page(&viewer),
            "add" => add_page(None, None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        if !viewer.can("timer_management") {
            return Err(PageError::Forbidden);
        }
        let path = submission.request.path.as_str();
        if let Some(id) = path.strip_prefix("timer/") {
            let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
            return match submission.form.as_str() {
                "timer" => save_timer(&viewer, Some(id), &submission),
                "delete" => delete_timer(&viewer, id),
                _ => Err(PageError::NotFound),
            };
        }
        match (path, submission.form.as_str()) {
            ("add", "timer") => save_timer(&viewer, None, &submission),
            // A row's Delete on the list.
            ("", "delete") => {
                let id: i64 = submission
                    .value("timer")
                    .parse()
                    .map_err(|_| PageError::NotFound)?;
                delete_timer(&viewer, id)
            }
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(StructureTimers);

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

fn flag(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or_default()
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

/// The viewer's main's corporation, if the host knows it (0 when not).
fn corporation(viewer: &Viewer) -> i64 {
    viewer.main.corporation_id
}

/// Only timers the viewer may see: every timer but other corporations'
/// corporation-only ones. `$1` is the viewer's corporation.
const VISIBLE: &str = "(NOT corp_timer OR (corporation_id = $1 AND $1 <> 0))";

// ---- the list --------------------------------------------------------------

struct Timer {
    id: i64,
    details: String,
    system: String,
    planet_moon: String,
    structure: String,
    timer_type: String,
    objective: String,
    eve_time: DateTime<Utc>,
    important: bool,
    corp_timer: bool,
    creator: String,
    /// The creator's main (0 for automatic timers).
    creator_id: i64,
    created_at: Option<DateTime<Utc>>,
    corporation_id: i64,
    /// Who last edited it, and when ("" if nobody has).
    editor: String,
    updated_at: Option<DateTime<Utc>>,
    /// An automatic timer: the app that published it (read-only here).
    source: Option<String>,
}

const TIMER_COLUMNS: &str = "id, details, system, planet_moon, structure, timer_type, objective, \
                             eve_time, important, corp_timer, creator_name, created_at, \
                             corporation_id, coalesce(updated_by_name, ''), updated_at, \
                             creator_character_id";

fn timer(row: &[Db]) -> Option<Timer> {
    Some(Timer {
        id: int(row, 0),
        details: text(row, 1),
        system: text(row, 2),
        planet_moon: text(row, 3),
        structure: text(row, 4),
        timer_type: text(row, 5),
        objective: text(row, 6),
        eve_time: when(row, 7)?,
        important: flag(row, 8),
        corp_timer: flag(row, 9),
        creator: text(row, 10),
        creator_id: int(row, 15),
        created_at: when(row, 11),
        corporation_id: int(row, 12),
        editor: text(row, 13),
        updated_at: when(row, 14),
        source: None,
    })
}

/// "friendly" as AA writes it: "Friendly".
fn objective_name(objective: &str) -> String {
    OBJECTIVES
        .iter()
        .find(|o| o.eq_ignore_ascii_case(objective))
        .map_or_else(|| "Neutral".to_owned(), |o| (*o).to_owned())
}

/// Timers other apps publish, as the host gives them to this viewer
/// (corporation-only ones only to that corporation). If they can't be
/// read, a note says so and the page shows the rest.
fn automatic() -> (Vec<Timer>, Option<String>) {
    match tether_plugin_sdk::timers::published() {
        Ok(shared) => (
            shared
                .into_iter()
                .filter_map(|s| {
                    let eve_time = DateTime::parse_from_rfc3339(&s.timer.at)
                        .ok()?
                        .with_timezone(&Utc);
                    Some(Timer {
                        id: 0,
                        details: s.timer.details,
                        system: s.timer.system,
                        planet_moon: String::new(),
                        structure: s.timer.title,
                        timer_type: String::new(),
                        objective: objective_name(&s.timer.objective),
                        eve_time,
                        important: false,
                        corp_timer: s.timer.corporation_id.is_some(),
                        creator: s.source.clone(),
                        creator_id: 0,
                        created_at: None,
                        corporation_id: s.timer.corporation_id.unwrap_or_default(),
                        editor: String::new(),
                        updated_at: None,
                        source: Some(s.source),
                    })
                })
                .collect(),
            None,
        ),
        Err(err) => {
            log::warn(format!("automatic timers couldn't be read: {err:?}"));
            (
                Vec::new(),
                Some("Automatic timers from other apps couldn't be read just now.".to_owned()),
            )
        }
    }
}

fn timers(filter: &str, order: &str, limit: i64, viewer: &Viewer) -> Result<Vec<Timer>, PageError> {
    Ok(query(
        &format!(
            "SELECT {TIMER_COLUMNS} FROM timers WHERE {VISIBLE} AND {filter} ORDER BY {order} LIMIT $2"
        ),
        &[corporation(viewer).into(), limit.into()],
    )?
    .iter()
    .filter_map(|r| timer(r))
    .collect())
}

fn objective_badge(objective: &str) -> Value {
    let tone = match objective {
        "Friendly" => Tone::Success,
        "Hostile" => Tone::Danger,
        _ => Tone::Neutral,
    };
    badge(objective, tone).into()
}

/// Automatic, important and corporation-only, in one cell.
fn flags(t: &Timer) -> Value {
    let mut labels = Vec::new();
    if t.source.is_some() {
        labels.push("Automatic");
    }
    if t.important {
        labels.push("Important");
    }
    if t.corp_timer {
        labels.push("Corporation");
    }
    if labels.is_empty() {
        return "".into();
    }
    let tone = if t.important {
        Tone::Warning
    } else {
        Tone::Neutral
    };
    badge(labels.join(" · "), tone).into()
}

fn timer_table(list: &[Timer], now: DateTime<Utc>, manage: bool, empty: &str) -> Table {
    let mut columns = vec![
        Column::numeric("EVE time"),
        Column::numeric("Remaining"),
        Column::text("Structure"),
        Column::text("Timer type"),
        Column::text("System"),
        Column::text("Planet/Moon"),
        Column::text("Objective"),
        Column::text("Details"),
        Column::text("Flags"),
        Column::text("Creator"),
    ];
    if manage {
        columns.push(Column::text("Action"));
        columns.push(Column::text(""));
    }
    let mut table = Table::new(columns).empty(empty);
    for t in list {
        let creator: Value = if t.creator_id > 0 {
            character(t.creator_id, t.creator.clone()).into()
        } else {
            t.creator.clone().into()
        };
        let mut row = vec![
            time(rfc3339(t.eve_time)),
            remaining(now, t.eve_time),
            t.structure.clone().into(),
            t.timer_type.clone().into(),
            t.system.clone().into(),
            t.planet_moon.clone().into(),
            objective_badge(&t.objective),
            t.details.clone().into(),
            flags(t),
            creator,
        ];
        if manage {
            // Automatic timers are changed in the app that made them.
            match t.source {
                Some(_) => row.extend(["".into(), "".into()]),
                None => row.extend([
                    link("Edit", format!("timer/{}", t.id)).into(),
                    delete_button(t),
                ]),
            }
        }
        table = table.row(row);
    }
    table
}

/// The time left: upcoming timers tick in the browser; past ones say how
/// long ago.
fn remaining(now: DateTime<Utc>, at: DateTime<Utc>) -> Value {
    if at > now {
        countdown(rfc3339(at))
    } else {
        when::countdown(now, at).into()
    }
}

/// A timer's Delete, asking first. It posts `delete` with the timer's id,
/// from the list or the timer's own page.
fn delete_button(t: &Timer) -> Value {
    action("Delete", "delete")
        .field("timer", t.id.to_string())
        .tone(Tone::Danger)
        .confirm(format!(
            "The {} timer in {} is deleted for everyone.",
            t.structure, t.system
        ))
        .into()
}

/// What one row of the list costs the page: every text it draws (the
/// structure and system twice, in Delete's question), 16 bytes for each of
/// its 12 values, and room for its times, badges and links. Fields are
/// capped in characters, and the host counts bytes.
fn row_bytes(t: &Timer) -> usize {
    t.details.len()
        + t.system.len() * 2
        + t.planet_moon.len()
        + t.structure.len() * 2
        + t.timer_type.len()
        + t.objective.len()
        + t.creator.len()
        + 12 * 16
        + 256
}

/// Keeps as much of `list` as fits in `budget`, taking it from the budget.
/// Whether anything was left out.
fn fit(list: &mut Vec<Timer>, budget: &mut usize) -> bool {
    let mut kept = 0;
    for t in list.iter() {
        let bytes = row_bytes(t);
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

/// The next week at a glance: a lane per timer type (the rest under
/// "Other"); hostile timers red, friendly blue, any within three hours in
/// the signal colour.
fn week_timeline(list: &[Timer], now: DateTime<Utc>, manage: bool) -> Option<Timeline> {
    let to = now + chrono::Duration::days(7);
    let soon: Vec<&Timer> = list.iter().filter(|t| t.eve_time <= to).collect();
    if soon.is_empty() {
        return None;
    }
    let mut lanes: Vec<(String, Lane)> = Vec::new();
    for t in soon.into_iter().take(200) {
        let kind = if t.timer_type.is_empty() {
            "Timers".to_owned()
        } else {
            t.timer_type.clone()
        };
        let label = if t.system.is_empty() {
            t.structure.clone()
        } else {
            format!("{} · {}", t.structure, t.system)
        };
        let tone = if t.eve_time - now <= chrono::Duration::hours(3) {
            Tone::Warning
        } else {
            match t.objective.as_str() {
                "Hostile" => Tone::Danger,
                "Friendly" => Tone::Success,
                _ => Tone::Neutral,
            }
        };
        let mut item = LaneItem::new(label, rfc3339(t.eve_time)).tone(tone);
        if manage && t.source.is_none() {
            item = item.link(format!("timer/{}", t.id));
        }
        // Six lanes by type at most; the rest share "Other".
        let known = lanes.iter().any(|(k, _)| *k == kind);
        let kind = if known || lanes.len() < 6 {
            kind
        } else {
            "Other".to_owned()
        };
        match lanes.iter().position(|(k, _)| *k == kind) {
            Some(i) => {
                if lanes[i].1.items.len() < 50 {
                    lanes[i].1.items.push(item);
                }
            }
            None => lanes.push((kind.clone(), Lane::new(kind).item(item))),
        }
    }
    let timeline = Timeline::new(rfc3339(now - chrono::Duration::hours(6)), rfc3339(to))
        .title("The next seven days");
    Some(
        lanes
            .into_iter()
            .fold(timeline, |t, (_, lane)| t.lane(lane)),
    )
}

fn timers_page(viewer: &Viewer) -> Result<Page, PageError> {
    let now = Utc::now();
    let manage = viewer.can("timer_management");
    let mut upcoming = timers("eve_time >= now()", "eve_time, id", UPCOMING_ROWS, viewer)?;
    let mut past = timers("eve_time < now()", "eve_time DESC, id", PAST_ROWS, viewer)?;
    // With the automatic ones, in the same order and within the same caps.
    let (shared, problem) = automatic();
    let (shared_upcoming, shared_past): (Vec<Timer>, Vec<Timer>) =
        shared.into_iter().partition(|t| t.eve_time >= now);
    upcoming.extend(shared_upcoming);
    upcoming.sort_by_key(|t| t.eve_time);
    upcoming.truncate(usize::try_from(UPCOMING_ROWS).unwrap_or(usize::MAX));
    past.extend(shared_past);
    past.sort_by_key(|t| std::cmp::Reverse(t.eve_time));
    past.truncate(usize::try_from(PAST_ROWS).unwrap_or(usize::MAX));
    // The stats count the whole list; the tables are cut to what fits.
    let upcoming_count = count(upcoming.len());
    let important_count = count(upcoming.iter().filter(|t| t.important).count());
    let corporation_count = count(upcoming.iter().filter(|t| t.corp_timer).count());
    // However long the timers' texts, the page stays within the host's
    // limits: the lists are cut short rather than the page refused.
    // Upcoming first, leaving the past at least a third.
    let mut budget = LIST_BYTES * 2 / 3;
    let upcoming_title = fit(&mut upcoming, &mut budget)
        .then(|| format!("The first {}, all that fit", upcoming.len()));
    budget += LIST_BYTES / 3;
    let past_title = if fit(&mut past, &mut budget) {
        format!("The latest {}, all that fit", past.len())
    } else {
        format!("The latest {PAST_ROWS}, newest first")
    };
    let next = upcoming
        .first()
        .map_or_else(|| Value::from("None"), |t| countdown(rfc3339(t.eve_time)));
    let mut next_stat = Stat::new("Next timer", next);
    if let Some(t) = upcoming.first() {
        next_stat = next_stat.caption(format!("{}, {}", t.structure, t.system));
    }
    let mut page = Page::new("Structure Timers")
        .description(
            "Structure timers in EVE time. Corporation timers are seen only by the creator's \
             corporation. Automatic timers come from other apps, such as Structures, and change there.",
        )
        .stats(vec![
            next_stat,
            Stat::new("Upcoming", upcoming_count),
            Stat::new("Important", important_count).caption("upcoming"),
            Stat::new("Corporation", corporation_count)
                .caption("upcoming, your corporation's only"),
        ]);
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    if let Some(timeline) = week_timeline(&upcoming, now, manage) {
        page = page.timeline(timeline);
    }
    let mut upcoming_table = timer_table(&upcoming, now, manage, "No upcoming timers.");
    if let Some(title) = upcoming_title {
        upcoming_table = upcoming_table.title(title);
    }
    Ok(page
        .tab("Upcoming", vec![Section::Table(upcoming_table)])
        .tab(
            "Past",
            vec![Section::Table(
                timer_table(&past, now, manage, "No past timers.").title(past_title),
            )],
        ))
}

// ---- adding and editing ------------------------------------------------------

fn options(list: &[&str]) -> Vec<(String, String)> {
    list.iter()
        .map(|o| ((*o).to_owned(), (*o).to_owned()))
        .collect()
}

/// What the timer form shows: a new timer's defaults, a stored timer, or
/// what was just posted (to fix a mistake).
struct Values {
    text: Vec<(&'static str, String)>,
    important: bool,
    corp_timer: bool,
}

const TEXT_FIELDS: [&str; 10] = [
    "details",
    "system",
    "planet_moon",
    "structure",
    "timer_type",
    "objective",
    "eve_time",
    "days",
    "hours",
    "minutes",
];

impl Values {
    fn new_timer() -> Self {
        Self {
            text: vec![
                ("structure", "Other".to_owned()),
                ("timer_type", "Not Specified".to_owned()),
                ("objective", "Neutral".to_owned()),
            ],
            important: false,
            corp_timer: false,
        }
    }

    fn stored(t: &Timer) -> Self {
        Self {
            text: vec![
                ("details", t.details.clone()),
                ("system", t.system.clone()),
                ("planet_moon", t.planet_moon.clone()),
                ("structure", t.structure.clone()),
                ("timer_type", t.timer_type.clone()),
                ("objective", t.objective.clone()),
                ("eve_time", when::eve_time_text(t.eve_time)),
            ],
            important: t.important,
            corp_timer: t.corp_timer,
        }
    }

    fn posted(submission: &Submission) -> Self {
        Self {
            text: TEXT_FIELDS
                .iter()
                .map(|name| (*name, submission.value(name).to_owned()))
                .collect(),
            important: submission.checked("important"),
            corp_timer: submission.checked("corp_timer"),
        }
    }

    fn get(&self, name: &str) -> String {
        self.text
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

fn timer_form(values: &Values, submit: &str) -> Form {
    let v = |name: &str| values.get(name);
    let left = |name: &str, label: &str, max: f64| {
        Field::number(name, label)
            .range(Some(0.0), Some(max), true)
            .value(v(name))
    };
    Form::new("timer", submit)
        .description("Give the EVE time, or the time left as the game shows it.")
        .field(
            Field::text("details", "Details", MAX_TEXT)
                .required()
                .value(v("details")),
        )
        .field(
            Field::text("system", "System", MAX_TEXT)
                .required()
                .value(v("system")),
        )
        .field(Field::text("planet_moon", "Planet/Moon", MAX_TEXT).value(v("planet_moon")))
        .field(
            Field::select("structure", "Structure type", options(STRUCTURES))
                .required()
                .value(v("structure")),
        )
        .field(
            Field::select("timer_type", "Timer type", options(TIMER_TYPES))
                .required()
                .value(v("timer_type")),
        )
        .field(
            Field::select("objective", "Objective", options(OBJECTIVES))
                .required()
                .value(v("objective")),
        )
        .field(
            Field::text("eve_time", "EVE time", 20)
                .help("As YYYY-MM-DD HH:MM, or YYYY.MM.DD HH:MM as the game shows it.")
                .value(v("eve_time")),
        )
        .field(left("days", "Days left", 365.0))
        .field(left("hours", "Hours left", 23.0))
        .field(left("minutes", "Minutes left", 59.0))
        .field(Field::checkbox("important", "Important", values.important))
        .field(
            Field::checkbox("corp_timer", "Corporation only", values.corp_timer)
                .help("Only pilots whose main is in the creator's corporation see it."),
        )
}

fn add_page(note: Option<&str>, values: Option<Values>) -> Result<Page, PageError> {
    let mut page = Page::new("New timer").description("A new structure timer");
    if let Some(note) = note {
        page = page.text(note);
    }
    let values = values.unwrap_or_else(Values::new_timer);
    Ok(page.form(timer_form(&values, "Create timer")))
}

/// A timer the viewer may see, or not found.
fn visible_timer(viewer: &Viewer, id: i64) -> Result<Timer, PageError> {
    query(
        &format!("SELECT {TIMER_COLUMNS} FROM timers WHERE id = $2 AND {VISIBLE}"),
        &[corporation(viewer).into(), id.into()],
    )?
    .first()
    .and_then(|r| timer(r))
    .ok_or(PageError::NotFound)
}

fn edit_page(
    viewer: &Viewer,
    id: i64,
    note: Option<&str>,
    values: Option<Values>,
) -> Result<Page, PageError> {
    let t = visible_timer(viewer, id)?;
    let now = Utc::now();
    let mut about = Card::new("Timer")
        .field("EVE time", time(rfc3339(t.eve_time)))
        .field("Remaining", remaining(now, t.eve_time))
        .field("Creator", character(t.creator_id, t.creator.clone()));
    if let Some(created) = t.created_at {
        about = about.field("Created", time(rfc3339(created)));
    }
    if let (false, Some(updated)) = (t.editor.is_empty(), t.updated_at) {
        about = about
            .field("Last edited by", t.editor.clone())
            .field("Last edited", time(rfc3339(updated)));
    }
    about = about.field("Delete", delete_button(&t));
    let mut page = Page::new("Edit timer")
        .description(format!("{} in {}", t.structure, t.system))
        .card(about);
    if let Some(note) = note {
        page = page.text(note);
    }
    let values = values.unwrap_or_else(|| Values::stored(&t));
    Ok(page.form(timer_form(&values, "Save timer")))
}

fn save_timer(
    viewer: &Viewer,
    id: Option<i64>,
    submission: &Submission,
) -> Result<SubmitResult, PageError> {
    let again = |note: &str| -> Result<SubmitResult, PageError> {
        let values = Some(Values::posted(submission));
        Ok(SubmitResult::Page(match id {
            Some(id) => edit_page(viewer, id, Some(note), values)?,
            None => add_page(Some(note), values)?,
        }))
    };
    let existing = id.map(|id| visible_timer(viewer, id)).transpose()?;
    let eve_time = match when::timer_time(
        Utc::now(),
        submission.value("eve_time"),
        [
            submission.value("days"),
            submission.value("hours"),
            submission.value("minutes"),
        ],
    ) {
        Ok(at) => at,
        Err(why) => return again(why),
    };
    let details = submission.value("details").trim().to_owned();
    let system = submission.value("system").trim().to_owned();
    if details.is_empty() || system.is_empty() {
        return again("Give the details and the system.");
    }
    // A corporation-only timer belongs to its creator's corporation, which
    // must be one the host knows; only that corporation makes a timer
    // corporation-only (or it could hide an alliance timer from everyone
    // else, the editor included).
    let corp_timer = submission.checked("corp_timer");
    let owner = existing
        .as_ref()
        .map_or(corporation(viewer), |t| t.corporation_id);
    if corp_timer && owner == 0 {
        return again(
            "The creator's corporation isn't known yet, so this can't be a corporation timer.",
        );
    }
    if corp_timer && owner != corporation(viewer) {
        return again("Only the creator's corporation can make this a corporation timer.");
    }
    let mut params: Vec<Db> = vec![
        details.into(),
        system.into(),
        submission.value("planet_moon").trim().to_owned().into(),
        submission.value("structure").to_owned().into(),
        submission.value("timer_type").to_owned().into(),
        submission.value("objective").to_owned().into(),
        Db::timestamp(rfc3339(eve_time)),
        submission.checked("important").into(),
        corp_timer.into(),
    ];
    match id {
        None => {
            params.extend([
                corporation(viewer).into(),
                viewer.account_id.into(),
                viewer.main.id.into(),
                viewer.main.name.clone().into(),
            ]);
            let added = storage::query(
                "INSERT INTO timers (details, system, planet_moon, structure, timer_type, objective, \
                 eve_time, important, corp_timer, corporation_id, creator_account_id, \
                 creator_character_id, creator_name) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
                &params,
            )
            .map_err(|e| failed("adding the timer", e))?;
            let new = added.rows.first().map_or(0, |r| int(r, 0));
            log::info(format!(
                "timer {new} created by {} ({})",
                viewer.main.name, viewer.main.id
            ));
        }
        Some(id) => {
            params.extend([
                id.into(),
                corporation(viewer).into(),
                viewer.main.id.into(),
                viewer.main.name.clone().into(),
            ]);
            // Visibility checked again in the statement itself.
            let changed = storage::execute(
                "UPDATE timers SET details = $1, system = $2, planet_moon = $3, structure = $4, \
                 timer_type = $5, objective = $6, eve_time = $7, important = $8, corp_timer = $9, \
                 updated_at = now(), updated_by_character_id = $12, updated_by_name = $13 \
                 WHERE id = $10 AND (NOT corp_timer OR (corporation_id = $11 AND $11 <> 0))",
                &params,
            )
            .map_err(|e| failed("saving the timer", e))?;
            if changed == 0 {
                return Err(PageError::NotFound);
            }
            log::info(format!(
                "timer {id} edited by {} ({})",
                viewer.main.name, viewer.main.id
            ));
        }
    }
    Ok(SubmitResult::Redirect(String::new()))
}

fn delete_timer(viewer: &Viewer, id: i64) -> Result<SubmitResult, PageError> {
    let deleted = storage::execute(
        &format!("DELETE FROM timers WHERE id = $2 AND {VISIBLE}"),
        &[corporation(viewer).into(), id.into()],
    )
    .map_err(|e| failed("deleting the timer", e))?;
    if deleted == 0 {
        return Err(PageError::NotFound);
    }
    log::info(format!(
        "timer {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect(String::new()))
}
