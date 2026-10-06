//! An app's Activity (DESIGN.md, App shell): Manage → Activity, for
//! those who run it (`admin.plugins`). Its ESI and HTTP calls, its
//! schedules and how each last ran, its jobs and its log, drawn by Tether
//! inside the app's frame. Plugins never see any of it.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use super::{ago, eve_time, every, until};
use crate::AppState;
use crate::error::AppError;

/// How many of each list the page shows, newest first.
const SHOWN: i64 = 50;

pub struct AccessView {
    /// `06 Oct 04:12`, and to the second for its tooltip.
    pub at: String,
    pub at_full: String,
    pub character: String,
    pub endpoint: String,
    /// The status line: `ok` when it went through, else what came of it.
    pub tone: &'static str,
    pub outcome: String,
}

pub struct HttpCallView {
    pub at: String,
    pub at_full: String,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: String,
    pub outcome: String,
    pub secret: String,
    pub bytes: i64,
    pub ms: i32,
}

pub struct ScheduleView {
    pub name: String,
    pub every: String,
    pub enabled: bool,
    /// `in 4m`, and the full EVE time.
    pub next: String,
    pub next_at: String,
    /// How its latest run went (a status line), and how long ago.
    pub last: Option<(&'static str, &'static str, String)>,
}

pub struct JobView {
    pub name: String,
    pub key: String,
    pub state: String,
    pub attempts: i32,
    pub when: String,
    pub error: String,
}

pub struct LogView {
    pub at: String,
    pub at_full: String,
    pub level: String,
    pub source: String,
    pub message: String,
}

/// To the second, for a time's tooltip.
fn full(at: DateTime<Utc>) -> String {
    format!("{} EVE", at.format("%Y-%m-%d %H:%M:%S"))
}

pub fn job_view(j: tether_db::plugin_jobs::JobRow, now: DateTime<Utc>) -> JobView {
    JobView {
        name: j.name,
        key: j.key.unwrap_or_default(),
        state: j.state,
        attempts: j.attempts,
        when: eve_time(j.run_at, now),
        error: j.last_error.unwrap_or_default(),
    }
}

pub fn log_view(l: tether_db::plugin_jobs::LogRow, now: DateTime<Utc>) -> LogView {
    LogView {
        at: eve_time(l.at, now),
        at_full: full(l.at),
        level: l.level,
        source: l.source,
        message: l.message,
    }
}

/// Everything the Activity page shows (and a stopped app's admin page).
pub struct Activity {
    pub plugin_id: String,
    /// The app runs: its schedules may be run now.
    pub running: bool,
    pub access: Vec<AccessView>,
    pub http: Vec<HttpCallView>,
    pub schedules: Vec<ScheduleView>,
    /// Queued or running now.
    pub active_jobs: i64,
    pub upcoming: Vec<JobView>,
    pub dead: Vec<JobView>,
    pub logs: Vec<LogView>,
}

fn seconds(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    (to - from).num_seconds()
}

pub async fn activity(
    state: &AppState,
    plugin_id: &str,
    running: bool,
) -> Result<Activity, AppError> {
    let now = Utc::now();
    let access = tether_db::plugin_esi::access_log(&state.db, plugin_id, SHOWN)
        .await?
        .into_iter()
        .map(|a| AccessView {
            at: eve_time(a.at, now),
            at_full: full(a.at),
            character: a.character.unwrap_or_default(),
            endpoint: a.endpoint,
            tone: if a.outcome == "ok" { "ok" } else { "danger" },
            outcome: if a.outcome == "ok" {
                "OK".to_owned()
            } else {
                a.outcome
            },
        })
        .collect();
    let http = tether_db::plugin_http::recent(&state.db, plugin_id, SHOWN)
        .await?
        .into_iter()
        .map(|c| HttpCallView {
            at: eve_time(c.at, now),
            at_full: full(c.at),
            status: c.status.map_or_else(String::new, |s| s.to_string()),
            secret: c.secret.unwrap_or_default(),
            method: c.method,
            host: c.host,
            path: c.path,
            outcome: c.outcome,
            bytes: c.bytes,
            ms: c.duration_ms,
        })
        .collect();
    // How each schedule's latest run went, from the queue's own record.
    let prefix = tether_db::plugin_jobs::schedule_name(plugin_id, "");
    let last_runs: HashMap<String, tether_jobs::schedule::LastRun> =
        tether_jobs::schedule::last_runs(&state.db)
            .await?
            .into_iter()
            .filter_map(|run| {
                let name = run.schedule.strip_prefix(&prefix)?.to_owned();
                Some((name, run))
            })
            .collect();
    let schedules = tether_db::plugin_jobs::schedules(&state.db, plugin_id)
        .await?
        .into_iter()
        .map(|s| ScheduleView {
            every: every(i64::from(s.every_secs)),
            enabled: s.enabled,
            next: until(seconds(now, s.next_run_at)),
            next_at: eve_time(s.next_run_at, now),
            last: last_runs.get(&s.name).map(|run| {
                let (tone, word) = match run.state {
                    tether_jobs::JobState::Succeeded => ("ok", "Succeeded"),
                    tether_jobs::JobState::Dead => ("danger", "Failed"),
                    _ if run.last_error.is_some() => ("warn", "Retrying"),
                    tether_jobs::JobState::Running => ("ok", "Running"),
                    tether_jobs::JobState::Queued => ("ok", "Queued"),
                };
                (
                    tone,
                    word,
                    ago(seconds(run.finished_at.unwrap_or(run.run_at), now)),
                )
            }),
            name: s.name,
        })
        .collect();
    let (active_jobs, upcoming, dead) =
        tether_db::plugin_jobs::jobs(&state.db, plugin_id, 20).await?;
    let logs = tether_db::plugin_jobs::logs(&state.db, plugin_id, SHOWN)
        .await?
        .into_iter()
        .map(|l| log_view(l, now))
        .collect();
    Ok(Activity {
        plugin_id: plugin_id.to_owned(),
        running,
        access,
        http,
        schedules,
        active_jobs,
        upcoming: upcoming.into_iter().map(|j| job_view(j, now)).collect(),
        dead: dead.into_iter().map(|j| job_view(j, now)).collect(),
        logs,
    })
}
