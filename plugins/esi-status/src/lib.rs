//! ESI Status (aa-esi-status), as a status page.
//!
//! ESI's own status, route by route, as ESI reports it (OK, degraded,
//! down, recovering or unknown), read every five minutes. The page leads
//! with the verdict and how much of the last day ESI was fully OK, then
//! what needs attention and since when, the last day's incidents, every
//! area of routes, each change newest first, and every route (as
//! aa-esi-status lists them). Anyone signed in may look, as there.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{
    Column, Page, PageError, Plugin, Request, Stat, Table, Tone, Value, badge, log, time,
};

/// Public endpoints take any subject.
const PUBLIC: Subject = Subject::Character(0);
/// The job's (and schedule's) name.
const CHECK: &str = "check";
/// How far back incidents and the share of checks fully OK reach.
const DAY: i64 = 24;
/// Three checks missed: the page says it's out of date.
const STALE_MINUTES: i64 = 15;
/// The most rows a table may have (the host's limit).
const MAX_ROWS: usize = 500;

/// ESI's statuses, as aa-esi-status shows them: the worst first, with
/// what each means.
const STATUSES: [(&str, &str, Tone); 5] = [
    (
        "Down",
        "These routes have a good chance of not responding at all or being completely \
         unavailable.",
        Tone::Danger,
    ),
    (
        "Degraded",
        "These routes have a good chance of being slow or returning errors.",
        Tone::Warning,
    ),
    (
        "Recovering",
        "These routes are recovering from earlier problems and might not work as expected yet, \
         or might be slow.",
        Tone::Warning,
    ),
    (
        "Unknown",
        "These routes have an unknown status.",
        Tone::Neutral,
    ),
    ("OK", "These routes are working as expected.", Tone::Success),
];

struct EsiStatus;

impl Plugin for EsiStatus {
    fn render(request: Request) -> Result<Page, PageError> {
        match request.path.as_str() {
            "" => status_page(),
            _ => Err(PageError::NotFound),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            CHECK => check(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(EsiStatus);

fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
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

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A status as one of ESI's five (anything else is unknown).
fn known(status: &str) -> &'static str {
    STATUSES
        .iter()
        .map(|(s, _, _)| *s)
        .find(|s| *s == status)
        .unwrap_or("Unknown")
}

/// How bad a status is: 0 for OK, up to 4 for Down.
fn severity(status: &str) -> usize {
    STATUSES
        .iter()
        .position(|(s, _, _)| *s == status)
        .map_or(0, |i| STATUSES.len() - 1 - i)
}

fn tone(status: &str) -> Tone {
    STATUSES
        .iter()
        .find(|(s, _, _)| *s == status)
        .map_or(Tone::Neutral, |(_, _, t)| *t)
}

/// A route's area: its first path segment in words (`/characters/{id}/x`
/// is Characters).
fn area(path: &str) -> String {
    match path.trim_start_matches('/').split('/').next().unwrap_or("") {
        "" => "Other".to_owned(),
        "fw" => "Faction warfare".to_owned(),
        "ui" => "UI".to_owned(),
        first => {
            let words = first.replace('_', " ");
            let mut chars = words.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

/// Routes by status, from `esi-status`' body.
fn parse(body: &str) -> (String, Vec<(String, String, &'static str)>) {
    let value: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let routes = value["routes"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|r| {
                    Some((
                        r["method"].as_str()?.to_owned(),
                        r["path"].as_str()?.to_owned(),
                        known(r["status"].as_str().unwrap_or("")),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    (
        value["compatibility_date"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        routes,
    )
}

/// The routes as `json_to_recordset` reads them.
const RECORDS: &str = "json_to_recordset($1::json) AS x(method text, path text, status text)";

/// Reads ESI's status: each route's change recorded, the routes replaced
/// (keeping since when each has had its status), the counts kept for 24
/// hours and the changes for a week.
fn check() -> Result<(), JobError> {
    let body = esi::get("esi-status", PUBLIC, &[], None)
        .map_err(|e| JobError::Retry(format!("reading ESI's status: {e:?}")))?
        .body;
    let (compatibility, routes) = parse(&body);
    if routes.is_empty() {
        log::warn("ESI's status listed no routes");
        return Ok(());
    }
    let count = |status: &str| {
        i64::try_from(routes.iter().filter(|(_, _, s)| *s == status).count()).unwrap_or(0)
    };
    let rows: Vec<serde_json::Value> = routes
        .iter()
        .map(|(method, path, status)| {
            serde_json::json!({ "method": method, "path": path, "status": status })
        })
        .collect();
    let records = || Db::json(serde_json::Value::Array(rows.clone()).to_string());
    storage::transaction(&[
        // A known route whose status isn't what it was.
        Statement::new(
            format!(
                "INSERT INTO changes (at, method, path, status, was) \
                 SELECT now(), x.method, x.path, x.status, r.status \
                 FROM (SELECT DISTINCT ON (method, path) method, path, status FROM {RECORDS}) x \
                 JOIN routes r ON r.method = x.method AND r.path = x.path \
                 WHERE r.status <> x.status"
            ),
            vec![records()],
        ),
        Statement::new(
            format!(
                "DELETE FROM routes r WHERE NOT EXISTS \
                 (SELECT 1 FROM {RECORDS} WHERE x.method = r.method AND x.path = r.path)"
            ),
            vec![records()],
        ),
        Statement::new(
            format!(
                "INSERT INTO routes (method, path, status, since) \
                 SELECT DISTINCT ON (method, path) method, path, status, now() FROM {RECORDS} \
                 ON CONFLICT (method, path) DO UPDATE SET status = EXCLUDED.status, \
                 since = CASE WHEN routes.status = EXCLUDED.status THEN routes.since \
                 ELSE EXCLUDED.since END"
            ),
            vec![records()],
        ),
        Statement::new(
            "INSERT INTO checks (checked_at, compatibility_date, ok, degraded, down, recovering, \
                 unknown) VALUES (now(), $1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            vec![
                compatibility.into(),
                count("OK").into(),
                count("Degraded").into(),
                count("Down").into(),
                count("Recovering").into(),
                count("Unknown").into(),
            ],
        ),
        Statement::new(
            "DELETE FROM checks WHERE checked_at < now() - interval '24 hours'",
            vec![],
        ),
        Statement::new(
            "DELETE FROM changes WHERE at < now() - interval '7 days'",
            vec![],
        ),
    ])
    .map_err(|e| JobError::Retry(format!("storing ESI's status: {e:?}")))?;
    Ok(())
}

struct Route {
    method: String,
    path: String,
    status: &'static str,
    since: Option<DateTime<Utc>>,
}

struct Change {
    at: DateTime<Utc>,
    method: String,
    path: String,
    status: &'static str,
    was: &'static str,
}

/// A stretch of time a route (or an area) wasn't OK, and the worst it was.
#[derive(Debug, Clone, PartialEq)]
struct Outage {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    status: &'static str,
}

/// A route's stretches not OK from `start` to `now`: from its changes in
/// that time (oldest first) and its status now.
fn outages(
    start: DateTime<Utc>,
    now: DateTime<Utc>,
    changes: &[&Change],
    current: &'static str,
) -> Vec<Outage> {
    let mut out = Vec::new();
    let mut status = changes.first().map_or(current, |c| c.was);
    let mut from = start;
    for change in changes {
        if status != "OK" && change.at > from {
            out.push(Outage {
                from,
                to: change.at,
                status,
            });
        }
        status = change.status;
        from = change.at;
    }
    if status != "OK" && now > from {
        out.push(Outage {
            from,
            to: now,
            status,
        });
    }
    out
}

/// Stretches closer than this are one incident: ESI often flaps between
/// statuses from one check to the next.
const GAP_MINUTES: i64 = 15;

/// Overlapping (or nearly touching) stretches as one, at the worst status
/// among them.
fn merged(mut all: Vec<Outage>) -> Vec<Outage> {
    all.sort_by_key(|o| o.from);
    let mut out: Vec<Outage> = Vec::new();
    for o in all {
        match out.last_mut() {
            Some(last) if o.from <= last.to + Duration::minutes(GAP_MINUTES) => {
                last.to = last.to.max(o.to);
                if severity(o.status) > severity(last.status) {
                    last.status = o.status;
                }
            }
            _ => out.push(o),
        }
    }
    out
}

/// The worst status any route had at a check (a `checks` row: checked_at,
/// compatibility_date, degraded, down, recovering, unknown).
fn check_worst(row: &[Db]) -> &'static str {
    if int(row, 3) > 0 {
        "Down"
    } else if int(row, 2) > 0 {
        "Degraded"
    } else if int(row, 4) > 0 {
        "Recovering"
    } else if int(row, 5) > 0 {
        "Unknown"
    } else {
        "OK"
    }
}

/// Stretches merged, leaving out any shorter than a minute: that can't be
/// told from a check's own timing.
fn lasting(all: Vec<Outage>) -> Vec<Outage> {
    merged(all)
        .into_iter()
        .filter(|o| (o.to - o.from).num_seconds() >= 60)
        .collect()
}

/// `45m`, `2h 05m`: how long a stretch lasted.
fn lasted(span: Duration) -> String {
    let minutes = span.num_minutes().max(1);
    if minutes >= 60 {
        format!("{}h {:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// `1 down · 2 degraded`: an area's routes that aren't OK, worst first.
fn summary(counts: &BTreeMap<&'static str, usize>) -> String {
    STATUSES
        .iter()
        .filter(|(s, _, _)| *s != "OK")
        .filter_map(|(s, _, _)| {
            counts
                .get(s)
                .filter(|n| **n > 0)
                .map(|n| format!("{n} {}", s.to_lowercase()))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn status_page() -> Result<Page, PageError> {
    let routes = storage::query(
        "SELECT method, path, status, since FROM routes ORDER BY path, method LIMIT 2000",
        &[],
    )
    .map_err(|e| failed("reading routes", e))?;
    let checks = storage::query(
        "SELECT checked_at, compatibility_date, degraded, down, recovering, unknown \
         FROM checks ORDER BY checked_at DESC LIMIT 300",
        &[],
    )
    .map_err(|e| failed("reading checks", e))?;
    let tracking = storage::query("SELECT min(started) FROM tracking", &[])
        .map_err(|e| failed("reading tracking", e))?;
    let changes = storage::query(
        "SELECT at, method, path, status, was FROM changes ORDER BY at DESC LIMIT 1000",
        &[],
    )
    .map_err(|e| failed("reading changes", e))?;
    let page = Page::new("ESI Status");
    let Some(latest) = checks.rows.first() else {
        return Ok(page
            .description("ESI's own status, route by route, read every five minutes.")
            .text("No ESI status data yet: the first check runs within five minutes."));
    };
    let now = Utc::now();
    let start = now - Duration::hours(DAY);
    let mut page = page.description(match when(latest, 0) {
        Some(at) => format!(
            "ESI's own status, route by route, read every five minutes; last at {} EVE \
             (compatibility date {}).",
            at.format("%H:%M"),
            text(latest, 1)
        ),
        None => "ESI's own status, route by route, read every five minutes.".to_owned(),
    });
    // Checks that stopped: say so before anything that may be out of date.
    if let Some(at) = when(latest, 0)
        && now - at > Duration::minutes(STALE_MINUTES)
    {
        page = page.text(format!(
            "Out of date: the last check was {} ago, and they run every five minutes. \
             What's below is as ESI was then.",
            lasted(now - at)
        ));
    }

    let routes: Vec<Route> = routes
        .rows
        .iter()
        .map(|r| Route {
            method: text(r, 0),
            path: text(r, 1),
            status: known(&text(r, 2)),
            since: when(r, 3),
        })
        .collect();
    let changes: Vec<Change> = changes
        .rows
        .iter()
        .filter_map(|r| {
            Some(Change {
                at: when(r, 0)?,
                method: text(r, 1),
                path: text(r, 2),
                status: known(&text(r, 3)),
                was: known(&text(r, 4)),
            })
        })
        .collect();

    // The verdict, how much of the day was fully OK, and how often it
    // changed.
    let total = routes.len();
    let mut not_ok: Vec<&Route> = routes.iter().filter(|r| r.status != "OK").collect();
    not_ok.sort_by(|a, b| {
        severity(b.status)
            .cmp(&severity(a.status))
            .then(a.since.cmp(&b.since))
            .then(a.path.cmp(&b.path))
    });
    let worst = not_ok.first().map_or("OK", |r| r.status);
    // Each check kept from the last day, oldest first: when, and the worst
    // status any route had then.
    let mut day: Vec<(DateTime<Utc>, &'static str)> = checks
        .rows
        .iter()
        .filter_map(|r| Some((when(r, 0)?, check_worst(r))))
        .filter(|(at, _)| *at > start)
        .collect();
    day.sort_by_key(|(at, _)| *at);
    let fully = day.iter().filter(|(_, worst)| *worst == "OK").count();
    let share = match day.len() {
        0 => "—".to_owned(),
        n if fully == n => "100%".to_owned(),
        n => format!("{:.1}%", fully as f64 * 100.0 / n as f64),
    };
    let today: Vec<&Change> = changes.iter().filter(|c| c.at > start).collect();
    page = page.stats(vec![
        Stat::new("ESI", badge(worst, tone(worst))).caption(if not_ok.is_empty() {
            format!("All {total} routes OK")
        } else {
            format!("{} of {total} routes not OK", not_ok.len())
        }),
        Stat::new("Fully OK · 24 h", share).caption(format!("of {} checks", day.len())),
        Stat::new(
            "Changes · 24 h",
            i64::try_from(today.len()).unwrap_or(i64::MAX),
        )
        .caption(match today.first() {
            Some(c) => format!("The last at {} EVE", c.at.format("%H:%M")),
            None => "None".to_owned(),
        }),
    ]);

    // What needs attention, worst and longest first, after what each
    // status there means.
    if !not_ok.is_empty() {
        let meanings: Vec<String> = STATUSES
            .iter()
            .filter(|(s, _, _)| not_ok.iter().any(|r| r.status == *s))
            .map(|(s, meaning, _)| {
                let meaning = meaning.replacen("These routes", "these routes", 1);
                format!("{s}: {meaning}")
            })
            .collect();
        page = page.text(meanings.join("\n"));
        let mut table = Table::new(vec![
            Column::text("Route"),
            Column::text("Method"),
            Column::text("Status"),
            Column::numeric("Since"),
        ])
        .title("Needs attention");
        for r in not_ok.iter().take(MAX_ROWS) {
            table = table.row(vec![
                r.path.clone().into(),
                r.method.clone().into(),
                badge(r.status, tone(r.status)).into(),
                r.since.map_or_else(|| "".into(), |at| time(rfc3339(at))),
            ]);
        }
        page = page.table(table);
    }

    // Incidents in the last 24 hours: each stretch any route wasn't OK,
    // from the checks kept, so the whole day. Which areas: from the
    // changes, since tracking them began (and the first check kept),
    // never guessed back further.
    let mut any: Vec<Outage> = Vec::new();
    for (i, (at, worst)) in day.iter().enumerate() {
        if *worst != "OK" {
            let to = day.get(i + 1).map_or(now, |(next, _)| *next);
            any.push(Outage {
                from: *at,
                to,
                status: worst,
            });
        }
    }
    let first = day
        .first()
        .map(|(at, _)| *at)
        .into_iter()
        .chain(tracking.rows.first().and_then(|r| when(r, 0)))
        .chain([start])
        .max()
        .unwrap_or(start);
    let mut by_area: BTreeMap<String, Vec<Outage>> = BTreeMap::new();
    for route in &routes {
        let mut mine: Vec<&Change> = today
            .iter()
            .filter(|c| c.method == route.method && c.path == route.path)
            .copied()
            .collect();
        mine.sort_by_key(|c| c.at);
        // Since when it's had its status matters only when it hasn't
        // changed in the window (since is then its last change).
        let from = if mine.is_empty() {
            route.since.map_or(first, |since| since.max(first))
        } else {
            first
        };
        let found = outages(from, now, &mine, route.status);
        if !found.is_empty() {
            by_area.entry(area(&route.path)).or_default().extend(found);
        }
    }
    let by_area: Vec<(String, Vec<Outage>)> = by_area
        .into_iter()
        .map(|(name, all)| (name, lasting(all)))
        .collect();
    let mut incidents = Table::new(vec![
        Column::numeric("Started"),
        Column::numeric("Ended"),
        Column::numeric("Lasted"),
        Column::text("Worst"),
        Column::text("Areas"),
    ])
    .title("Incidents, last 24 hours")
    .empty("Every route was OK at every check in the last 24 hours.");
    for o in lasting(any).iter().rev() {
        let areas: Vec<&str> = by_area
            .iter()
            .filter(|(_, all)| all.iter().any(|a| a.from < o.to && a.to > o.from))
            .map(|(name, _)| name.as_str())
            .collect();
        let ongoing = o.to >= now;
        incidents = incidents.row(vec![
            time(rfc3339(o.from)),
            if ongoing {
                badge("Ongoing", tone(o.status)).into()
            } else {
                time(rfc3339(o.to))
            },
            lasted(o.to - o.from).into(),
            badge(o.status, tone(o.status)).into(),
            if areas.is_empty() {
                "".into()
            } else {
                areas.join(", ").into()
            },
        ]);
    }
    page = page.table(incidents);

    // Every area, the worst first.
    let mut areas: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for route in &routes {
        *areas
            .entry(area(&route.path))
            .or_default()
            .entry(route.status)
            .or_default() += 1;
    }
    let mut areas: Vec<(String, BTreeMap<&'static str, usize>)> = areas.into_iter().collect();
    let area_worst = |counts: &BTreeMap<&'static str, usize>| {
        counts
            .keys()
            .copied()
            .max_by_key(|s| severity(s))
            .unwrap_or("OK")
    };
    areas.sort_by(|a, b| {
        severity(area_worst(&b.1))
            .cmp(&severity(area_worst(&a.1)))
            .then(a.0.cmp(&b.0))
    });
    let mut table = Table::new(vec![
        Column::text("Area"),
        Column::numeric("Routes"),
        Column::text("Status"),
    ])
    .title("By area");
    for (name, counts) in &areas {
        let routes: usize = counts.values().sum();
        let worst = area_worst(counts);
        let status: Value = if worst == "OK" {
            badge("OK", Tone::Success).into()
        } else {
            badge(summary(counts), tone(worst)).into()
        };
        table = table.row(vec![
            name.clone().into(),
            i64::try_from(routes).unwrap_or(i64::MAX).into(),
            status,
        ]);
    }
    page = page.table(table);

    // Each change, newest first.
    let mut history = Table::new(vec![
        Column::numeric("When"),
        Column::text("Route"),
        Column::text("Method"),
        Column::text("Was"),
        Column::text("Now"),
    ])
    .title("Changes, newest first")
    .empty("No route has changed status in the last week.");
    for c in changes.iter().take(50) {
        history = history.row(vec![
            time(rfc3339(c.at)),
            c.path.clone().into(),
            c.method.clone().into(),
            badge(c.was, tone(c.was)).into(),
            badge(c.status, tone(c.status)).into(),
        ]);
    }
    page = page.table(history);

    // Every route, as aa-esi-status lists them.
    let mut every = Table::new(vec![
        Column::text("Route"),
        Column::text("Method"),
        Column::text("Status"),
        Column::numeric("Since"),
    ])
    .title("Every route");
    for r in routes.iter().take(MAX_ROWS) {
        every = every.row(vec![
            r.path.clone().into(),
            r.method.clone().into(),
            badge(r.status, tone(r.status)).into(),
            r.since.map_or_else(|| "".into(), |at| time(rfc3339(at))),
        ]);
    }
    Ok(page.table(every))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_read_as_esi_gives_them() {
        let (date, routes) = parse(
            r#"{"compatibility_date": "2026-08-18", "routes": [
                {"method": "GET", "path": "/alliances", "status": "OK"},
                {"method": "GET", "path": "/markets", "status": "Degraded"},
                {"method": "GET", "path": "/odd", "status": "Sideways"},
                {"path": "/no-method", "status": "OK"}
            ]}"#,
        );
        assert_eq!(date, "2026-08-18");
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[1].2, "Degraded");
        assert_eq!(routes[2].2, "Unknown");
        assert!(parse("nonsense").1.is_empty());
    }

    #[test]
    fn routes_group_into_areas() {
        assert_eq!(area("/characters/{character_id}/assets/"), "Characters");
        assert_eq!(area("/fw/stats/"), "Faction warfare");
        assert_eq!(area("/ui/autopilot/waypoint/"), "UI");
        assert_eq!(area("/status/"), "Status");
        assert_eq!(area("/"), "Other");
        assert!(severity("Down") > severity("Degraded"));
        assert!(severity("Degraded") > severity("OK"));
    }

    fn at(hour: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_default()
            + Duration::hours(hour)
    }

    fn change(hour: i64, was: &'static str, status: &'static str) -> Change {
        Change {
            at: at(hour),
            method: "GET".to_owned(),
            path: "/markets/".to_owned(),
            status,
            was,
        }
    }

    #[test]
    fn outages_follow_the_changes() {
        // Degraded from 2 to 5, then down from 20 on.
        let changes = [
            change(2, "OK", "Degraded"),
            change(5, "Degraded", "OK"),
            change(20, "OK", "Down"),
        ];
        let refs: Vec<&Change> = changes.iter().collect();
        let found = outages(at(0), at(24), &refs, "Down");
        assert_eq!(
            found,
            vec![
                Outage {
                    from: at(2),
                    to: at(5),
                    status: "Degraded"
                },
                Outage {
                    from: at(20),
                    to: at(24),
                    status: "Down"
                },
            ]
        );
        // No change today, but not OK: the whole day.
        assert_eq!(
            outages(at(0), at(24), &[], "Degraded"),
            vec![Outage {
                from: at(0),
                to: at(24),
                status: "Degraded"
            }]
        );
        assert!(outages(at(0), at(24), &[], "OK").is_empty());
    }

    #[test]
    fn overlapping_outages_merge_at_their_worst() {
        let o = |from, to, status| Outage {
            from: at(from),
            to: at(to),
            status,
        };
        assert_eq!(
            merged(vec![
                o(4, 6, "Down"),
                o(1, 5, "Degraded"),
                o(8, 9, "Recovering")
            ]),
            vec![o(1, 6, "Down"), o(8, 9, "Recovering")]
        );
        // Ten minutes apart: one incident.
        let near = Outage {
            from: at(9) + Duration::minutes(10),
            to: at(10),
            status: "Degraded",
        };
        assert_eq!(
            merged(vec![o(8, 9, "Recovering"), near]),
            vec![o(8, 10, "Degraded")]
        );
        let mut counts = BTreeMap::new();
        counts.insert("Degraded", 2);
        counts.insert("Down", 1);
        counts.insert("OK", 9);
        assert_eq!(summary(&counts), "1 down · 2 degraded");
    }
}
