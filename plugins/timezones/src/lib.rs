//! Time Zones (aa-timezones).
//!
//! - **Time Zones**: EVE time, the pilot's own time zone and the panels
//!   (aa-timezones' ten defaults until an admin sets their own), each with
//!   its time, day and UTC offset; kept current while it's open.
//! - **Adjust time**: every panel at another time, for a timer (in up to
//!   7 days, 59 minutes and 59 seconds, as aa-timezones) or a planned
//!   fleet (a date and time in one of the zones), on a page with its own
//!   address to share.
//! - **Panels** (`manage`, aa-timezones' admin site): add and delete them.
//!
//! Anyone signed in may look, as aa-timezones. Its browser-local time
//! becomes the pilot's own zone, picked once: Tether can't see the
//! browser's.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{
    Card, Column, Field, Form, Page, PageError, Plugin, Request, Stat, Submission, SubmitResult,
    Table, Tone, Value, action, badge, log, share,
};

/// aa-timezones' default panels, shown until an admin adds their own.
const DEFAULT_PANELS: [(&str, &str); 10] = [
    ("US / Pacific", "US/Pacific"),
    ("US / Mountain", "US/Mountain"),
    ("US / Central", "US/Central"),
    ("US / Eastern", "US/Eastern"),
    ("EU / Western", "Europe/London"),
    ("EU / Central", "Europe/Berlin"),
    ("EU / Eastern", "Europe/Istanbul"),
    ("Russia / Moscow", "Europe/Moscow"),
    ("China / Shanghai", "Asia/Shanghai"),
    ("Australia / Sydney", "Australia/ACT"),
];

/// Panels at most (aa-timezones has no limit; a page shows 500 rows).
const MAX_PANELS: i64 = 100;
const MAX_NAME: u32 = 60;
/// aa-timezones' timer limit: 7 days, 59 minutes and 59 seconds.
const MAX_DAYS: i64 = 7;
/// Adjusted times from 2000 to 2100.
const EARLIEST: i64 = 946_684_800;
const LATEST: i64 = 4_102_444_800;
/// Seconds between refreshes of the page showing now.
const REFRESH: u32 = 30;

struct Timezones;

impl Plugin for Timezones {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let path = request.path.as_str();
        if let Some(at) = path.strip_prefix("at/") {
            return zones_page(&viewer, Some(instant(at)?));
        }
        match path {
            "" => zones_page(&viewer, None),
            "adjust" => adjust_page(&viewer, None),
            "mine" => mine_page(&viewer, None),
            "panels" if viewer.can("manage") => panels_page(None),
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        match (submission.request.path.as_str(), submission.form.as_str()) {
            ("adjust", "timer") => set_timer(&viewer, &submission),
            ("adjust", "fixed") => set_fixed(&viewer, &submission),
            ("mine", "mine") => save_mine(&viewer, &submission),
            ("panels", form) if viewer.can("manage") => match form {
                "add" => add_panel(&viewer, &submission),
                "delete" => delete_panel(&viewer, &submission),
                "defaults" => copy_defaults(&viewer),
                _ => Err(PageError::NotFound),
            },
            _ => Err(PageError::NotFound),
        }
    }
}

tether_plugin_sdk::export!(Timezones);

// ---- helpers ---------------------------------------------------------------

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

/// An adjusted time from its page's address (seconds since 1970).
fn instant(at: &str) -> Result<DateTime<Utc>, PageError> {
    at.parse::<i64>()
        .ok()
        .filter(|s| (EARLIEST..=LATEST).contains(s))
        .and_then(|s| DateTime::from_timestamp(s, 0))
        .ok_or(PageError::NotFound)
}

/// A zone by its IANA name ("Europe/Berlin").
fn zone(name: &str) -> Option<Tz> {
    name.trim().parse().ok()
}

/// A panel: its name and zone.
struct Panel {
    id: Option<i64>,
    name: String,
    zone: String,
}

/// The admins' panels, by name as aa-timezones, or its defaults.
fn panels() -> Result<(Vec<Panel>, bool), PageError> {
    let rows = storage::query(
        &format!("SELECT id, name, zone FROM panels ORDER BY name LIMIT {MAX_PANELS}"),
        &[],
    )
    .map_err(|e| failed("reading panels", e))?;
    if rows.rows.is_empty() {
        return Ok((
            DEFAULT_PANELS
                .iter()
                .map(|(name, zone)| Panel {
                    id: None,
                    name: (*name).to_owned(),
                    zone: (*zone).to_owned(),
                })
                .collect(),
            true,
        ));
    }
    Ok((
        rows.rows
            .iter()
            .map(|r| Panel {
                id: Some(int(r, 0)),
                name: text(r, 1),
                zone: text(r, 2),
            })
            .collect(),
        false,
    ))
}

/// The viewer's own zone, if they picked one.
fn mine(viewer: &Viewer) -> Result<Option<String>, PageError> {
    let rows = storage::query(
        "SELECT zone FROM viewer_zones WHERE account_id = $1",
        &[viewer.account_id.into()],
    )
    .map_err(|e| failed("reading your time zone", e))?;
    Ok(rows.rows.first().map(|r| text(r, 0)))
}

/// A zone's clock at `at`: time, day and UTC offset.
fn clock(tz: Tz, at: DateTime<Utc>) -> (String, String, String) {
    let local = at.with_timezone(&tz);
    (
        local.format("%H:%M").to_string(),
        local.format("%a %d %b %Y").to_string(),
        local.format("UTC%:z").to_string(),
    )
}

// ---- pages -------------------------------------------------------------------

/// Every zone now (kept current), or at an adjusted time.
fn zones_page(viewer: &Viewer, at: Option<DateTime<Utc>>) -> Result<Page, PageError> {
    let when = at.unwrap_or_else(Utc::now);
    let (panels, _) = panels()?;
    let eve = clock(Tz::UTC, when);
    let mut stats = vec![Stat::new("EVE time", eve.0).caption(eve.1)];
    let own = mine(viewer)?;
    match own.as_deref().and_then(zone) {
        Some(tz) => {
            let (time, day, offset) = clock(tz, when);
            stats.push(Stat::new("Your time", time).caption(format!("{day}, {offset}")));
        }
        None => stats.push(Stat::new(
            "Your time",
            badge("Pick your time zone", Tone::Neutral),
        )),
    }
    let mut table = Table::new(vec![
        Column::text("Panel"),
        Column::numeric("Time"),
        Column::text("Day"),
        Column::text("UTC offset"),
    ])
    .title("Time zones")
    .empty("No panels.");
    for panel in &panels {
        // A zone the database no longer knows shows as such.
        let row: Vec<Value> = match zone(&panel.zone) {
            Some(tz) => {
                let (time, day, offset) = clock(tz, when);
                vec![
                    panel.name.clone().into(),
                    time.into(),
                    day.into(),
                    offset.into(),
                ]
            }
            None => vec![
                panel.name.clone().into(),
                "".into(),
                badge("Unknown time zone", Tone::Warning).into(),
                "".into(),
            ],
        };
        table = table.row(row);
    }
    let mut page = match at {
        None => Page::new("Time Zones")
            .description("EVE time beside other time zones, now")
            .refresh(REFRESH),
        Some(at) => Page::new("Time Zones")
            .description(format!(
                "Every time zone at {} EVE",
                at.format("%Y-%m-%d %H:%M:%S")
            ))
            .card(
                Card::new("This time")
                    .field("Link to share", share(format!("at/{}", at.timestamp()))),
            ),
    };
    page = page.stats(stats).table(table);
    Ok(page)
}

fn adjust_page(viewer: &Viewer, problem: Option<&str>) -> Result<Page, PageError> {
    let number = |name: &str, label: &str, max: i64| {
        Field::number(name, label)
            .range(Some(0.0), Some(max as f64), true)
            .value("0")
            .required()
    };
    let timer = Form::new("timer", "Set time")
        .title("In a while")
        .description(
            "For a timer (reinforced, anchoring): the time it runs out, up to 7 days, 59 minutes \
             and 59 seconds from now.",
        )
        .field(number("days", "Days", MAX_DAYS))
        .field(number("hours", "Hours", 23))
        .field(number("minutes", "Minutes", 59))
        .field(number("seconds", "Seconds", 59));
    let (panels, _) = panels()?;
    let mut zones: Vec<(String, String)> = vec![("UTC".to_owned(), "EVE time".to_owned())];
    if let Some(own) = mine(viewer)? {
        zones.push((own.clone(), format!("Your time ({own})")));
    }
    zones.extend(
        panels
            .iter()
            .filter(|p| !zones.iter().any(|(z, _)| *z == p.zone))
            .map(|p| (p.zone.clone(), p.name.clone()))
            .collect::<Vec<_>>(),
    );
    let fixed = Form::new("fixed", "Set time")
        .title("At a date and time")
        .description(
            "For a planned fleet: a date and time in one of the zones. Keep EVE time to plan in \
             EVE time.",
        )
        .field(
            Field::text("date", "Date", 10)
                .help("YYYY-MM-DD, e.g. 2026-10-03")
                .required(),
        )
        .field(
            Field::text("time", "Time", 5)
                .help("HH:MM, e.g. 19:30")
                .required(),
        )
        .field(Field::select("zone", "In", zones).value("UTC").required());
    let mut page = Page::new("Adjust time")
        .description("Every time zone at another time, with a link to share");
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    Ok(page.form(timer).form(fixed))
}

fn mine_page(viewer: &Viewer, problem: Option<&str>) -> Result<Page, PageError> {
    let current = mine(viewer)?.unwrap_or_default();
    let form = Form::new("mine", "Save")
        .title("Your time zone")
        .description(
            "Shown beside EVE time. aa-timezones shows the browser's own time; Tether can't see \
             it, so pick yours once.",
        )
        .field(
            Field::text("zone", "Time zone", MAX_NAME)
                .value(current)
                .help("An IANA name, e.g. Europe/Berlin or America/New_York. Empty: none."),
        );
    let mut page = Page::new("Your time zone").description("The time zone shown as yours");
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    Ok(page.form(form))
}

fn panels_page(problem: Option<&str>) -> Result<Page, PageError> {
    let (panels, defaults) = panels()?;
    let mut table = Table::new(vec![
        Column::text("Panel"),
        Column::text("Time zone"),
        Column::numeric("Time now"),
        Column::text(""),
    ])
    .title(if defaults {
        "Panels: aa-timezones' defaults"
    } else {
        "Panels"
    })
    .empty("No panels.");
    for panel in &panels {
        let now = zone(&panel.zone).map_or_else(String::new, |tz| clock(tz, Utc::now()).0);
        let delete: Value = match panel.id {
            Some(id) => action("Delete", "delete")
                .field("panel", id.to_string())
                .tone(Tone::Danger)
                .confirm(format!("The {} panel is deleted.", panel.name))
                .into(),
            None => "".into(),
        };
        table = table.row(vec![
            panel.name.clone().into(),
            panel.zone.clone().into(),
            now.into(),
            delete,
        ]);
    }
    let add = Form::new("add", "Add panel")
        .title("Add a panel")
        .description(if defaults {
            "Adding one replaces the defaults, as in aa-timezones: add every panel you want \
             shown, or start from the defaults below."
        } else {
            "Panels are shown by name."
        })
        .field(
            Field::text("name", "Name", MAX_NAME)
                .help("e.g. EU / Central")
                .required(),
        )
        .field(
            Field::text("zone", "Time zone", MAX_NAME)
                .help("An IANA name, e.g. Europe/Berlin")
                .required(),
        );
    let mut page = Page::new("Panels").description("The time zones everyone sees beside EVE time");
    if let Some(problem) = problem {
        page = page.text(problem);
    }
    page = page.table(table).form(add);
    if defaults {
        page = page.card(
            Card::new("Start from the defaults")
                .description("Adds aa-timezones' ten default panels, to change from there.")
                .field("Defaults", action("Add the defaults", "defaults")),
        );
    }
    Ok(page)
}

// ---- forms -------------------------------------------------------------------

fn number(submission: &Submission, name: &str, max: i64) -> Option<i64> {
    submission
        .value(name)
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|n| (0..=max).contains(n))
}

fn set_timer(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let parts = (
        number(submission, "days", MAX_DAYS),
        number(submission, "hours", 23),
        number(submission, "minutes", 59),
        number(submission, "seconds", 59),
    );
    let (Some(days), Some(hours), Some(minutes), Some(seconds)) = parts else {
        return Ok(SubmitResult::Page(adjust_page(
            viewer,
            Some("Days are 0 to 7, hours 0 to 23, minutes and seconds 0 to 59."),
        )?));
    };
    let at = Utc::now()
        + Duration::days(days)
        + Duration::hours(hours)
        + Duration::minutes(minutes)
        + Duration::seconds(seconds);
    Ok(SubmitResult::Redirect(format!("at/{}", at.timestamp())))
}

fn set_fixed(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let date = NaiveDate::parse_from_str(submission.value("date").trim(), "%Y-%m-%d");
    let time = NaiveTime::parse_from_str(submission.value("time").trim(), "%H:%M");
    let tz = zone(submission.value("zone"));
    let (Ok(date), Ok(time), Some(tz)) = (date, time, tz) else {
        return Ok(SubmitResult::Page(adjust_page(
            viewer,
            Some("The date is YYYY-MM-DD and the time HH:MM, e.g. 2026-10-03 and 19:30."),
        )?));
    };
    // A time the clocks skip (spring forward) takes the next valid one;
    // one they repeat, the first.
    let local = date.and_time(time);
    let at = tz
        .from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(local + Duration::hours(1)))
                .earliest()
        })
        .map(|t| t.with_timezone(&Utc))
        .filter(|t| (EARLIEST..=LATEST).contains(&t.timestamp()));
    match at {
        Some(at) => Ok(SubmitResult::Redirect(format!("at/{}", at.timestamp()))),
        None => Ok(SubmitResult::Page(adjust_page(
            viewer,
            Some("Pick a time between the years 2000 and 2100."),
        )?)),
    }
}

fn save_mine(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let name = submission.value("zone").trim();
    if name.is_empty() {
        storage::execute(
            "DELETE FROM viewer_zones WHERE account_id = $1",
            &[viewer.account_id.into()],
        )
        .map_err(|e| failed("clearing your time zone", e))?;
        return Ok(SubmitResult::Redirect(String::new()));
    }
    if zone(name).is_none() || name.chars().count() > MAX_NAME as usize {
        return Ok(SubmitResult::Page(mine_page(
            viewer,
            Some("That isn't a time zone Tether knows: use an IANA name, e.g. Europe/Berlin."),
        )?));
    }
    storage::execute(
        "INSERT INTO viewer_zones (account_id, zone) VALUES ($1, $2) \
         ON CONFLICT (account_id) DO UPDATE SET zone = EXCLUDED.zone",
        &[viewer.account_id.into(), name.into()],
    )
    .map_err(|e| failed("saving your time zone", e))?;
    Ok(SubmitResult::Redirect(String::new()))
}

fn add_panel(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let name = submission.value("name").trim();
    let zone_name = submission.value("zone").trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME as usize
        || name.chars().any(char::is_control)
    {
        return Ok(SubmitResult::Page(panels_page(Some(
            "A panel's name is 1 to 60 characters.",
        ))?));
    }
    if zone(zone_name).is_none() || zone_name.chars().count() > MAX_NAME as usize {
        return Ok(SubmitResult::Page(panels_page(Some(
            "That isn't a time zone Tether knows: use an IANA name, e.g. Europe/Berlin.",
        ))?));
    }
    let added = storage::execute(
        &format!(
            "INSERT INTO panels (name, zone) SELECT $1, $2 \
             WHERE (SELECT count(*) FROM panels) < {MAX_PANELS} ON CONFLICT (name) DO NOTHING"
        ),
        &[name.into(), zone_name.into()],
    )
    .map_err(|e| failed("adding the panel", e))?;
    if added == 0 {
        return Ok(SubmitResult::Page(panels_page(Some(&format!(
            "There's a panel named {name} already, or {MAX_PANELS} panels."
        )))?));
    }
    log::info(format!(
        "panel {name} ({zone_name}) added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("panels".to_owned()))
}

fn delete_panel(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let id: i64 = submission
        .value("panel")
        .parse()
        .map_err(|_| PageError::NotFound)?;
    storage::execute("DELETE FROM panels WHERE id = $1", &[id.into()])
        .map_err(|e| failed("deleting the panel", e))?;
    log::info(format!(
        "panel {id} deleted by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("panels".to_owned()))
}

fn copy_defaults(viewer: &Viewer) -> Result<SubmitResult, PageError> {
    let statements: Vec<storage::Statement> = DEFAULT_PANELS
        .iter()
        .map(|(name, zone)| {
            storage::Statement::new(
                "INSERT INTO panels (name, zone) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
                vec![(*name).into(), (*zone).into()],
            )
        })
        .collect();
    storage::transaction(&statements).map_err(|e| failed("adding the defaults", e))?;
    log::info(format!(
        "default panels added by {} ({})",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("panels".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_known_zones() {
        for (name, tz) in DEFAULT_PANELS {
            assert!(zone(tz).is_some(), "{name}: {tz}");
        }
        assert!(zone("Not/AZone").is_none());
    }

    #[test]
    fn clocks_follow_daylight_saving_time() {
        let berlin = zone("Europe/Berlin").unwrap();
        // Summer: UTC+2; winter: UTC+1.
        let summer = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        let winter = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(
            clock(berlin, summer),
            (
                "14:00".to_owned(),
                "Wed 01 Jul 2026".to_owned(),
                "UTC+02:00".to_owned()
            )
        );
        assert_eq!(clock(berlin, winter).0, "13:00");
        assert_eq!(clock(Tz::UTC, summer).2, "UTC+00:00");
    }

    #[test]
    fn adjusted_times_are_real_instants() {
        assert_eq!(instant("1790000000").unwrap().timestamp(), 1_790_000_000);
        assert!(instant("12").is_err());
        assert!(instant("x").is_err());
        assert!(instant("99999999999").is_err());
    }
}
