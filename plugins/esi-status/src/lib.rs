//! ESI Status (aa-esi-status).
//!
//! ESI's own status, route by route, as ESI reports it: OK, degraded,
//! down, recovering or unknown, each kind with what it means, and how the
//! counts went over the last 24 hours. A check every five minutes; anyone
//! signed in may look, as aa-esi-status.

use chrono::{DateTime, Utc};
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

/// A status as one of ESI's five (anything else is unknown).
fn known(status: &str) -> &'static str {
    STATUSES
        .iter()
        .map(|(s, _, _)| *s)
        .find(|s| *s == status)
        .unwrap_or("Unknown")
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

/// Reads ESI's status: the routes replaced, the counts kept for 24 hours.
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
    storage::transaction(&[
        Statement::new("DELETE FROM routes", vec![]),
        Statement::new(
            "INSERT INTO routes (method, path, status) \
             SELECT DISTINCT ON (method, path) method, path, status \
             FROM json_to_recordset($1::json) AS x(method text, path text, status text)",
            vec![Db::json(serde_json::Value::Array(rows).to_string())],
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
    ])
    .map_err(|e| JobError::Retry(format!("storing ESI's status: {e:?}")))?;
    Ok(())
}

/// A status's count's column in `checks` as read by `status_page`.
fn column(status: &str) -> usize {
    match status {
        "OK" => 2,
        "Degraded" => 3,
        "Down" => 4,
        "Recovering" => 5,
        _ => 6,
    }
}

fn status_page() -> Result<Page, PageError> {
    let routes = storage::query(
        "SELECT method, path, status FROM routes ORDER BY path, method LIMIT 2000",
        &[],
    )
    .map_err(|e| failed("reading routes", e))?;
    let checks = storage::query(
        "SELECT checked_at, compatibility_date, ok, degraded, down, recovering, unknown \
         FROM checks ORDER BY checked_at DESC LIMIT 300",
        &[],
    )
    .map_err(|e| failed("reading checks", e))?;
    let mut page = Page::new("ESI Status");
    let Some(latest) = checks.rows.first() else {
        return Ok(page
            .description("ESI's status, route by route")
            .text("No ESI status data yet: the first check runs within five minutes."));
    };
    page = page.description(format!(
        "ESI's status, route by route (compatibility date {})",
        text(latest, 1)
    ));
    let total = routes.rows.len();
    let mut stats = vec![
        Stat::new("Endpoints", i64::try_from(total).unwrap_or(0)).caption(match when(latest, 0) {
            Some(at) => format!("Checked {} EVE", at.format("%H:%M")),
            None => String::new(),
        }),
    ];
    for (status, _, tone) in STATUSES {
        let n = int(latest, column(status));
        let value: Value = if n > 0 && status != "OK" {
            badge(n.to_string(), tone).into()
        } else {
            n.into()
        };
        stats.push(Stat::new(status, value));
    }
    page = page.stats(stats);
    // A table for each status that has routes, the worst first.
    for (status, meaning, tone) in STATUSES {
        let rows: Vec<&Vec<Db>> = routes
            .rows
            .iter()
            .filter(|r| known(&text(r, 2)) == status)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let mut table = Table::new(vec![
            Column::text("Route"),
            Column::text("Method"),
            Column::text("Status"),
        ])
        .title(format!("{status}: {meaning}"));
        for r in rows {
            table = table.row(vec![
                text(r, 1).into(),
                text(r, 0).into(),
                badge(status, tone).into(),
            ]);
        }
        page = page.table(table);
    }
    // The last 24 hours: each check whose counts changed.
    let mut history = Table::new(vec![
        Column::numeric("Checked"),
        Column::numeric("OK"),
        Column::numeric("Degraded"),
        Column::numeric("Down"),
        Column::numeric("Recovering"),
        Column::numeric("Unknown"),
    ])
    .title("Status history: the last 24 hours, when it changed");
    let mut last: Option<[i64; 5]> = None;
    let mut shown = 0;
    for r in checks.rows.iter().rev() {
        let counts = [int(r, 2), int(r, 3), int(r, 4), int(r, 5), int(r, 6)];
        if last == Some(counts) {
            continue;
        }
        last = Some(counts);
        let Some(at) = when(r, 0) else {
            continue;
        };
        history = history.row(vec![
            time(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            counts[0].into(),
            counts[1].into(),
            counts[2].into(),
            counts[3].into(),
            counts[4].into(),
        ]);
        shown += 1;
    }
    if shown > 0 {
        page = page.table(history);
    }
    Ok(page)
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
}
