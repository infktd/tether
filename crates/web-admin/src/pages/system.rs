//! The admin dashboard (F14). Health (`/admin/system`): whether ESI,
//! Discord, the job queue, backups, apps' data sources and updates are
//! working, then ESI's requests, the queue, schedules and the version.
//! Settings (`/admin/settings`): the site's name, the accent colour, the
//! notification limit and update checks. And the audit log
//! (`/admin/audit`).

use std::collections::HashMap;

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use tether_core::permissions::{ADMIN_AUDIT, ADMIN_SYSTEM};
use tether_db::audit::{self, Actor};
use tether_esi::budget::{BULK_ERROR_RESERVE, BudgetSnapshot};
use tether_jobs::schedule::{LastRun, ScheduleRow};
use tether_jobs::{JobId, JobState};

use super::admin::guard;
use super::{PageError, Shell, ago, grouped, plural, render, until};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::updates;

/// A status line's tone (DESIGN.md, Status lines), mildest first: the
/// verdict takes the worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    /// Not in use: not set up, switched off, or not yet.
    Off,
    Ok,
    Warn,
    Danger,
}

impl Tone {
    pub fn as_str(self) -> &'static str {
        match self {
            Tone::Off => "off",
            Tone::Ok => "ok",
            Tone::Warn => "warn",
            Tone::Danger => "danger",
        }
    }
}

/// One line of Health's Systems panel.
pub struct SystemLine {
    pub name: &'static str,
    /// What it is, or what's wrong.
    pub detail: String,
    /// Its reading, if it has one: `23,456 pilots online`.
    pub value: String,
    pub status: String,
    pub tone: Tone,
    /// Where it's set up or looked after.
    pub href: Option<&'static str>,
}

/// The line above them all: the worst of them, in words.
pub struct Verdict {
    pub tone: Tone,
    pub headline: String,
    pub detail: String,
}

fn verdict(lines: &[SystemLine], now: DateTime<Utc>) -> Verdict {
    let count = |tone: Tone| lines.iter().filter(|l| l.tone == tone).count() as i64;
    let (problems, warnings) = (count(Tone::Danger), count(Tone::Warn));
    let checked = format!("checked {} EVE", now.format("%H:%M"));
    let with_warnings = || {
        if warnings > 0 {
            format!("{} · {checked}", plural(warnings, "warning", "warnings"))
        } else {
            checked.clone()
        }
    };
    if problems > 0 {
        Verdict {
            tone: Tone::Danger,
            headline: plural(problems, "problem", "problems"),
            detail: with_warnings(),
        }
    } else if warnings > 0 {
        Verdict {
            tone: Tone::Warn,
            headline: "Needs attention".to_owned(),
            detail: with_warnings(),
        }
    } else {
        Verdict {
            tone: Tone::Ok,
            headline: "All systems nominal".to_owned(),
            detail: checked,
        }
    }
}

/// A rate-limit group, as Health lists it.
pub struct RateGroup {
    pub name: String,
    pub limit: String,
    pub remaining: String,
    /// `for 42 s` after a 429.
    pub held: Option<String>,
}

/// A labelled number in a readout grid.
pub struct Readout {
    pub label: &'static str,
    pub value: String,
    /// `warn` or `danger` when it needs someone.
    pub tone: Option<&'static str>,
}

fn readout(label: &'static str, n: u64, tone: Option<&'static str>) -> Readout {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    Readout {
        label,
        value: grouped(n),
        tone: tone.filter(|_| n > 0),
    }
}

pub struct DeadJob {
    pub id: i64,
    pub kind: String,
    pub attempts: i32,
    pub when: String,
    pub error: String,
}

/// A schedule's latest run, as a status line and when.
pub struct LastView {
    pub tone: &'static str,
    pub status: &'static str,
    pub ago: String,
    pub at: String,
}

/// An app, by its name, linking to its Apps page.
pub struct AppRef {
    pub name: String,
    pub href: String,
}

pub struct ScheduleView {
    /// Tether's own by its full name; an app's by its name in the app.
    pub name: String,
    /// Run now posts here: Tether's own schedules only (an app's run from
    /// its Apps page).
    pub run: Option<String>,
    /// The app, for an app's schedule.
    pub app: Option<AppRef>,
    pub every: String,
    pub enabled: bool,
    pub next: String,
    pub next_at: String,
    pub last: Option<LastView>,
}

/// `every hour`, `every 5 minutes`, `every 2 days`.
fn every(secs: i32) -> String {
    let (n, one, many) = match secs {
        s if s % 86_400 == 0 => (s / 86_400, "day", "days"),
        s if s % 3_600 == 0 => (s / 3_600, "hour", "hours"),
        s if s % 60 == 0 => (s / 60, "minute", "minutes"),
        s => (s, "second", "seconds"),
    };
    if n == 1 {
        format!("every {one}")
    } else {
        format!("every {} {many}", grouped(i64::from(n)))
    }
}

fn time(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

fn seconds_since(at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - at).num_seconds()
}

/// Whether ESI answers: pilots online, or why not. Bounded: the pages for
/// diagnosing ESI must load when ESI doesn't.
async fn esi_status(state: &AppState) -> (Option<i64>, Option<String>) {
    let online = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.esi.players_online(),
    )
    .await;
    match online {
        Ok(Ok(n)) => (Some(n), None),
        Ok(Err(err)) => (None, Some(err.to_string())),
        Err(_) => (None, Some("no answer within 5 seconds".to_owned())),
    }
}

/// How a run went, in a status line's words.
fn run_status(run: &LastRun) -> (Tone, &'static str) {
    match run.state {
        JobState::Succeeded => (Tone::Ok, "Succeeded"),
        JobState::Dead => (Tone::Danger, "Failed"),
        // A failed try waits to be retried with its error kept.
        _ if run.last_error.is_some() => (Tone::Warn, "Retrying"),
        JobState::Running => (Tone::Ok, "Running"),
        JobState::Queued => (Tone::Ok, "Queued"),
    }
}

/// The first line of a job's error, for a status line's detail.
fn first_line(error: Option<&str>, otherwise: &str) -> String {
    error
        .and_then(|e| e.lines().find(|l| !l.trim().is_empty()))
        .map_or_else(|| otherwise.to_owned(), |l| l.trim().to_owned())
}

/// Everything Health reads, once per view.
struct Readings {
    esi_online: Option<i64>,
    esi_error: Option<String>,
    budget: BudgetSnapshot,
    counts: Vec<(JobState, i64)>,
    schedules: Vec<ScheduleRow>,
    last_runs: HashMap<String, LastRun>,
    discord: bool,
    sources: (i64, i64),
    updates: updates::Status,
    /// Installed apps' names, by id.
    apps: HashMap<String, String>,
    now: DateTime<Utc>,
}

impl Readings {
    async fn take(state: &AppState) -> Result<Self, AppError> {
        let (esi_online, esi_error) = esi_status(state).await;
        let last_runs = tether_jobs::schedule::last_runs(&state.db)
            .await?
            .into_iter()
            .map(|run| (run.schedule.clone(), run))
            .collect();
        Ok(Self {
            esi_online,
            esi_error,
            budget: state.esi.budget(),
            counts: tether_jobs::counts(&state.db).await?,
            schedules: tether_jobs::schedule::list(&state.db).await?,
            last_runs,
            discord: crate::discord::is_configured(state).await?,
            sources: tether_db::plugin_esi::data_source_health(&state.db).await?,
            updates: updates::status(&state.db).await?,
            apps: tether_db::plugins::list(&state.db)
                .await?
                .into_iter()
                .map(|p| (p.id, p.name))
                .collect(),
            now: Utc::now(),
        })
    }

    fn count(&self, wanted: JobState) -> i64 {
        self.counts
            .iter()
            .find(|(s, _)| *s == wanted)
            .map_or(0, |(_, n)| *n)
    }

    fn lines(&self) -> Vec<SystemLine> {
        let mut lines = vec![self.esi(), self.limits(), self.discord(), self.queue()];
        lines.extend(self.backups());
        lines.extend(self.sources());
        lines.push(self.updates());
        lines
    }

    fn esi(&self) -> SystemLine {
        match self.esi_online {
            Some(n) => SystemLine {
                name: "ESI",
                detail: "Tranquility's status, asked as this page loaded".to_owned(),
                value: plural(n, "pilot online", "pilots online"),
                status: "Up".to_owned(),
                tone: Tone::Ok,
                href: None,
            },
            None => SystemLine {
                name: "ESI",
                detail: self
                    .esi_error
                    .clone()
                    .unwrap_or_else(|| "No answer".to_owned()),
                value: String::new(),
                status: "Down".to_owned(),
                tone: Tone::Danger,
                href: None,
            },
        }
    }

    fn limits(&self) -> SystemLine {
        let b = &self.budget;
        let held = b
            .groups
            .iter()
            .filter(|g| g.held_for_secs.is_some())
            .count() as i64;
        let refused = i64::try_from(b.counts.error_limited).unwrap_or(i64::MAX);
        let (status, tone, detail) = if refused > 0 {
            (
                "Budget exceeded",
                Tone::Danger,
                format!(
                    "ESI refused {} for too many errors (HTTP 420) since Tether started; it must stay 0",
                    plural(refused, "request", "requests")
                ),
            )
        } else if held > 0 {
            (
                "Held back",
                Tone::Warn,
                format!(
                    "{} waiting out a 429",
                    plural(held, "rate-limit group", "rate-limit groups")
                ),
            )
        } else if b.error_remain.is_some_and(|r| r < BULK_ERROR_RESERVE) {
            (
                "Syncs paused",
                Tone::Warn,
                format!(
                    "Fewer than {BULK_ERROR_RESERVE} errors left this minute: syncs wait for it to reset, people's own requests don't"
                ),
            )
        } else {
            (
                "Within limits",
                Tone::Ok,
                "Tether's own requests: nothing held back".to_owned(),
            )
        };
        SystemLine {
            name: "ESI limits",
            detail,
            value: b.error_remain.map_or_else(
                || "no errors this minute".to_owned(),
                |r| plural(i64::from(r), "error left", "errors left"),
            ),
            status: status.to_owned(),
            tone,
            href: None,
        }
    }

    fn discord(&self) -> SystemLine {
        let line = |detail: String, value: String, status: &str, tone| SystemLine {
            name: "Discord",
            detail,
            value,
            status: status.to_owned(),
            tone,
            href: Some("/admin/discord"),
        };
        if !self.discord {
            return line(
                "Members can't link Discord or get roles".to_owned(),
                String::new(),
                "Not set up",
                Tone::Off,
            );
        }
        let every = format!(
            "Roles and nicknames, {}",
            every(crate::discord_sync::SYNC_ALL_EVERY.as_secs() as i32)
        );
        match self.last_runs.get("discord.sync_all") {
            Some(run) if run.state == JobState::Dead => line(
                first_line(run.last_error.as_deref(), "The last sync gave up"),
                String::new(),
                "Sync failing",
                Tone::Danger,
            ),
            Some(run) if run.state == JobState::Succeeded => line(
                every,
                format!(
                    "synced {}",
                    ago(seconds_since(
                        run.finished_at.unwrap_or(run.run_at),
                        self.now
                    ))
                ),
                "Syncing",
                Tone::Ok,
            ),
            Some(run) => {
                let (tone, status) = run_status(run);
                line(every, String::new(), status, tone)
            }
            None => line(every, String::new(), "Set up", Tone::Ok),
        }
    }

    fn queue(&self) -> SystemLine {
        let dead = self.count(JobState::Dead);
        let value = format!(
            "{} queued · {} running",
            grouped(self.count(JobState::Queued)),
            grouped(self.count(JobState::Running))
        );
        if dead > 0 {
            SystemLine {
                name: "Job queue",
                detail: "Gave up after retrying: why, and Retry, below".to_owned(),
                value,
                status: plural(dead, "dead job", "dead jobs"),
                tone: Tone::Warn,
                href: None,
            }
        } else {
            SystemLine {
                name: "Job queue",
                detail: "Nothing has given up".to_owned(),
                value,
                status: "Working".to_owned(),
                tone: Tone::Ok,
                href: None,
            }
        }
    }

    /// The nightly backup, if this instance has the schedule.
    fn backups(&self) -> Option<SystemLine> {
        let schedule = self
            .schedules
            .iter()
            .find(|s| s.name == crate::backups::NIGHTLY_JOB)?;
        let line = |detail: String, value: String, status: &str, tone| SystemLine {
            name: "Backups",
            detail,
            value,
            status: status.to_owned(),
            tone,
            href: None,
        };
        let kept = "Nightly and encrypted; a week of them kept".to_owned();
        if !schedule.enabled {
            return Some(line(kept, String::new(), "Off", Tone::Off));
        }
        Some(match self.last_runs.get(&schedule.name) {
            Some(run) if run.state == JobState::Succeeded => {
                let age = seconds_since(run.finished_at.unwrap_or(run.run_at), self.now);
                let value = format!("last {}", ago(age));
                if age > 36 * 3_600 {
                    line(
                        "The last one is over a day and a half old".to_owned(),
                        value,
                        "Overdue",
                        Tone::Warn,
                    )
                } else {
                    line(kept, value, "Backed up", Tone::Ok)
                }
            }
            Some(run) if run.state == JobState::Dead => line(
                first_line(run.last_error.as_deref(), "The last one gave up"),
                String::new(),
                "Failed",
                Tone::Danger,
            ),
            Some(run) => {
                let (tone, status) = run_status(run);
                line(kept, String::new(), status, tone)
            }
            None => line(
                format!(
                    "The first one runs {}",
                    until(seconds_since(self.now, schedule.next_run_at))
                ),
                String::new(),
                "None yet",
                Tone::Off,
            ),
        })
    }

    /// Apps' data sources, once any app has one.
    fn sources(&self) -> Option<SystemLine> {
        let (working, broken) = self.sources;
        let total = working + broken;
        (total > 0).then(|| SystemLine {
            name: "Data sources",
            detail: if broken > 0 {
                "Apps can't read through them: a character lost a role or its token".to_owned()
            } else {
                "The characters apps read ESI through".to_owned()
            },
            value: format!("{} of {} working", grouped(working), grouped(total)),
            status: if broken > 0 {
                format!("{} not working", grouped(broken))
            } else {
                "All working".to_owned()
            },
            tone: if broken > 0 { Tone::Warn } else { Tone::Ok },
            href: Some("/admin/plugins"),
        })
    }

    fn updates(&self) -> SystemLine {
        let u = &self.updates;
        let line = |detail: String, status: String, tone, href| SystemLine {
            name: "Updates",
            detail,
            value: u.current.to_owned(),
            status,
            tone,
            href,
        };
        if !u.enabled {
            return line(
                "Tether doesn't ask GitHub for new releases".to_owned(),
                "Checks off".to_owned(),
                Tone::Off,
                Some(SETTINGS),
            );
        }
        if u.newer
            && let Some(latest) = &u.latest
        {
            return line(
                "Upgrade under Version, below: a snapshot is taken first".to_owned(),
                format!("{latest} available"),
                Tone::Warn,
                None,
            );
        }
        if let Some(e) = &u.error {
            return line(e.clone(), "Check failed".to_owned(), Tone::Warn, None);
        }
        match &u.latest {
            Some(latest) => line(
                format!("The latest release is {latest}"),
                "Up to date".to_owned(),
                Tone::Ok,
                None,
            ),
            None if u.no_releases => line(
                "Nothing has been released yet".to_owned(),
                "Up to date".to_owned(),
                Tone::Ok,
                None,
            ),
            None => line(
                "Tether asks GitHub once a day".to_owned(),
                "Not checked yet".to_owned(),
                Tone::Off,
                None,
            ),
        }
    }

    fn requests(&self) -> Vec<Readout> {
        let c = &self.budget.counts;
        vec![
            readout("Fresh", c.ok, None),
            readout("From cache", c.cached, None),
            readout("Not modified", c.not_modified, None),
            readout("No response", c.transport_errors, Some("warn")),
            readout("Client errors · 4xx", c.client_errors, None),
            readout("Server errors · 5xx", c.server_errors, Some("warn")),
            readout("Rate limited · 429", c.rate_limited, Some("warn")),
            readout("Budget exceeded · 420", c.error_limited, Some("danger")),
        ]
    }

    fn rate_groups(&self) -> Vec<RateGroup> {
        self.budget
            .groups
            .iter()
            .map(|g| RateGroup {
                name: g.name.clone(),
                limit: g.limit.clone(),
                remaining: grouped(i64::from(g.remaining)),
                held: g.held_for_secs.map(|h| format!("for {h} s")),
            })
            .collect()
    }

    fn jobs(&self) -> Vec<Readout> {
        let n = |s| u64::try_from(self.count(s)).unwrap_or(0);
        vec![
            readout("Queued", n(JobState::Queued), None),
            readout("Running", n(JobState::Running), None),
            readout("Succeeded · 7 days", n(JobState::Succeeded), None),
            readout("Dead", n(JobState::Dead), Some("warn")),
        ]
    }

    /// Tether's own schedules first, then each app's, by the app's name.
    fn schedule_views(&self) -> Vec<ScheduleView> {
        let mut views: Vec<ScheduleView> = self
            .schedules
            .iter()
            .map(|s| {
                let app = s
                    .name
                    .strip_prefix("plugin:")
                    .and_then(|rest| rest.split_once(':'));
                ScheduleView {
                    name: app.map_or_else(|| s.name.clone(), |(_, name)| name.to_owned()),
                    run: app
                        .is_none()
                        .then(|| format!("/admin/system/schedules/{}/run", s.name)),
                    app: app.map(|(id, _)| AppRef {
                        name: self.apps.get(id).cloned().unwrap_or_else(|| id.to_owned()),
                        href: format!("/admin/plugins/{id}"),
                    }),
                    every: every(s.every_secs),
                    enabled: s.enabled,
                    next: until(seconds_since(self.now, s.next_run_at)),
                    next_at: time(s.next_run_at),
                    last: self.last_runs.get(&s.name).map(|run| {
                        let (tone, status) = run_status(run);
                        let at = run.finished_at.unwrap_or(run.run_at);
                        LastView {
                            tone: tone.as_str(),
                            status,
                            ago: ago(seconds_since(at, self.now)),
                            at: time(at),
                        }
                    }),
                }
            })
            .collect();
        views.sort_by(|a, b| {
            let app = |v: &ScheduleView| v.app.as_ref().map(|a| a.name.to_lowercase());
            app(a).cmp(&app(b)).then_with(|| a.name.cmp(&b.name))
        });
        views
    }
}

/// The Settings page.
const SETTINGS: &str = "/admin/settings";
/// The Health page.
const HEALTH: &str = "/admin/system";

/// How many dead jobs Health lists.
const DEAD_SHOWN: i64 = 20;

#[derive(Template)]
#[template(path = "admin_system.html")]
struct HealthPage {
    shell: Shell,
    verdict: Verdict,
    lines: Vec<SystemLine>,
    requests: Vec<Readout>,
    rate_groups: Vec<RateGroup>,
    jobs: Vec<Readout>,
    dead_count: i64,
    /// How many of them are listed.
    dead_shown: i64,
    dead: Vec<DeadJob>,
    schedules: Vec<ScheduleView>,
    upgrade: crate::upgrader::View,
    updates: updates::Status,
    error: Option<String>,
}

async fn health_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let readings = Readings::take(state).await?;
    let lines = readings.lines();
    let dead: Vec<DeadJob> = tether_jobs::list(&state.db, Some(JobState::Dead), DEAD_SHOWN)
        .await?
        .into_iter()
        .map(|j| DeadJob {
            id: j.id.0,
            kind: j.kind,
            attempts: j.attempts,
            when: time(j.run_at),
            error: j.last_error.unwrap_or_default(),
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            code,
            &HealthPage {
                shell,
                verdict: verdict(&lines, readings.now),
                lines,
                requests: readings.requests(),
                rate_groups: readings.rate_groups(),
                jobs: readings.jobs(),
                dead_count: readings.count(JobState::Dead),
                dead_shown: dead.len() as i64,
                dead,
                schedules: readings.schedule_views(),
                upgrade: crate::upgrader::view(state).await?,
                updates: readings.updates,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

pub struct Preset {
    pub name: &'static str,
    pub value: &'static str,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct SettingsPage {
    shell: Shell,
    accent: String,
    presets: Vec<Preset>,
    /// The accent isn't one of the presets.
    custom: bool,
    /// AA's `NOTIFICATIONS_MAX_PER_USER`.
    notifications_max: i64,
    updates: updates::Status,
    error: Option<String>,
}

async fn settings_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    let accent = crate::theme::accent(&state.db).await?;
    Ok(super::with_problem(
        problem,
        render(
            code,
            &SettingsPage {
                shell,
                custom: !crate::theme::PRESETS.iter().any(|(_, v)| *v == accent),
                presets: crate::theme::PRESETS
                    .iter()
                    .map(|(name, value)| Preset {
                        name,
                        value,
                        checked: *value == accent,
                    })
                    .collect(),
                accent,
                notifications_max: tether_db::settings::notifications_max(&state.db).await?,
                updates: updates::status(&state.db).await?,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

#[derive(Template)]
#[template(path = "admin_system_summary.html")]
struct HealthSummary {
    verdict: Verdict,
    /// The lines that aren't simply working.
    lines: Vec<SystemLine>,
}

/// `GET /admin/system/summary`: Health's verdict at the top of
/// Administration's overview, with whatever isn't simply working (AA's
/// Dashboard admin panels: Software Version, Task Queue and ESI status),
/// a fragment loaded after the page so a slow ESI never holds the
/// overview up.
pub async fn summary(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(&state, ADMIN_SYSTEM).await?;
    let readings = Readings::take(&state).await?;
    let lines = readings.lines();
    Ok(render(
        StatusCode::OK,
        &HealthSummary {
            verdict: verdict(&lines, readings.now),
            lines: lines.into_iter().filter(|l| l.tone != Tone::Ok).collect(),
        },
    ))
}

/// `GET /admin/system`: Health.
pub async fn system(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    health_page(&state, shell, None).await
}

/// `GET /admin/settings`
pub async fn settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_SYSTEM, "settings").await?;
    settings_page(&state, shell, None).await
}

/// `POST /admin/jobs/{id}/retry`: a dead job, back in the queue.
pub async fn retry_job(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    let mut tx = state.db.begin().await?;
    let retried = tether_jobs::retry(&mut *tx, JobId(id)).await?;
    if !retried {
        drop(tx);
        let err = AppError::not_found("No dead job with that id.");
        return health_page(&state, shell, Some(err)).await;
    }
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "job.retry",
        Some(&format!("job:{id}")),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(super::stay::back(HEALTH, "Job queued again."))
}

/// Runs a schedule now for an admin (Health's or an app's Apps page's
/// Run now), audited as `schedule.run_now`.
pub async fn run_schedule(
    state: &AppState,
    actor: tether_db::accounts::AccountId,
    name: &str,
) -> Result<(), AppError> {
    use tether_jobs::schedule::RunNow;
    match tether_jobs::schedule::run_now(&state.db, name).await? {
        RunNow::Queued => {}
        RunNow::Busy => {
            return Err(AppError::bad_request(
                "Its last run is still queued or running; it'll pick up the latest data.",
            ));
        }
        RunNow::TooSoon => {
            return Err(AppError::bad_request(
                "It ran less than a minute ago. Give it a moment.",
            ));
        }
        RunNow::Off => {
            return Err(AppError::not_found("No schedule by that name is running."));
        }
    }
    audit::record(
        &state.db,
        Actor::Account(actor),
        "schedule.run_now",
        Some(&format!("schedule:{name}")),
        json!({}),
    )
    .await?;
    Ok(())
}

#[derive(askama::Template)]
#[template(path = "run_now_result.html")]
struct RunNowResult<'a> {
    queued: bool,
    message: &'a str,
}

/// Run now's answer to htmx: swapped in place of the button, so the page
/// isn't reloaded.
pub fn run_now_fragment(result: &Result<(), AppError>) -> Response {
    let (queued, message) = match result {
        Ok(()) => (true, ""),
        Err(err) => (false, err.message()),
    };
    super::render(StatusCode::OK, &RunNowResult { queued, message })
}

/// `POST /admin/system/schedules/{name}/run`: one of Tether's own
/// schedules (apps' are run from their pages).
pub async fn run_now(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    let result = if name.starts_with("plugin:") {
        Err(AppError::bad_request(
            "Run an app's schedules from its page.",
        ))
    } else {
        run_schedule(&state, session.account, &name).await
    };
    if super::is_htmx(&headers) {
        return Ok(run_now_fragment(&result));
    }
    match result {
        Ok(()) => Ok(super::stay::back(HEALTH, "Queued.")),
        Err(err) => health_page(&state, shell, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatesForm {
    /// A checkbox: present when ticked.
    enabled: Option<String>,
}

/// `POST /admin/system/updates`: switch update checks on or off.
pub async fn set_updates(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<UpdatesForm>,
) -> Result<Response, PageError> {
    let (session, _) = guard(&state, session, ADMIN_SYSTEM, "settings").await?;
    updates::set_enabled(&state, session.account, form.enabled.is_some()).await?;
    Ok(super::stay::back(
        SETTINGS,
        if form.enabled.is_some() {
            "Update checks on."
        } else {
            "Update checks off."
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct NotificationsForm {
    max_per_user: String,
}

/// `POST /admin/system/notifications`: AA's `NOTIFICATIONS_MAX_PER_USER`.
pub async fn set_notifications(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NotificationsForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "settings").await?;
    let range = tether_db::settings::NOTIFICATIONS_MAX_RANGE;
    let result = match form.max_per_user.trim().parse::<i64>() {
        Ok(n) if range.contains(&n) => {
            crate::notifications::set_max(&state.db, session.account, n).await
        }
        _ => Err(AppError::bad_request(format!(
            "Keep {} to {} notifications per user.",
            range.start(),
            range.end()
        ))),
    };
    match result {
        Ok(()) => Ok(super::stay::back(SETTINGS, "Notification limit saved.")),
        Err(err) => settings_page(&state, shell, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct SiteNameForm {
    #[serde(default)]
    site_name: String,
    /// `setup` when saved on the setup wizard's last step, which it goes
    /// back to.
    #[serde(default)]
    from: String,
}

/// `POST /admin/system/site-name`: the site's own name (empty: Tether's
/// alone), from System or the setup wizard's last step.
pub async fn set_site_name(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<SiteNameForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "settings").await?;
    let (back, done) = match form.from.as_str() {
        "setup" => ("/setup", "Site name saved."),
        _ => (SETTINGS, "Site name saved."),
    };
    match crate::site_name::set(&state, session.account, &form.site_name).await {
        // The name is in the tab title: reload the page for it.
        Ok(_) => Ok(super::stay::back(back, done)),
        Err(err) if back == "/setup" => Err(err.into()),
        Err(err) => settings_page(&state, shell, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ThemeForm {
    /// A preset's value, or `custom` for `custom_accent`.
    accent: String,
    #[serde(default)]
    custom_accent: String,
}

/// `POST /admin/system/theme`: the accent colour.
pub async fn set_theme(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
    Form(form): Form<ThemeForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "settings").await?;
    let chosen = if form.accent == "custom" {
        &form.custom_accent
    } else {
        &form.accent
    };
    match crate::theme::set_accent(&state, session.account, chosen).await {
        // The accent is in a stylesheet: a whole new load shows it.
        Ok(()) if super::is_htmx(&headers) => Ok(super::stay::with_toast(
            (StatusCode::NO_CONTENT, [("hx-refresh", "true")]).into_response(),
            super::stay::Toast::done("Accent saved."),
        )),
        Ok(()) => Ok(Redirect::to(SETTINGS).into_response()),
        Err(err) => settings_page(&state, shell, Some(err)).await,
    }
}

#[derive(Template)]
#[template(path = "admin_system_upgrade.html")]
struct UpgradeCard {
    upgrade: crate::upgrader::View,
    updates: updates::Status,
}

/// `GET /admin/system/upgrade`: Health's Version card, which polls itself
/// while the updater works.
pub async fn upgrade_card(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(&state, ADMIN_SYSTEM).await?;
    Ok(render(
        StatusCode::OK,
        &UpgradeCard {
            upgrade: crate::upgrader::view(&state).await?,
            updates: updates::status(&state.db).await?,
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpgradeForm {
    tag: String,
}

/// `POST /admin/system/upgrade`: asks the updater for the version offered.
pub async fn upgrade(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<UpgradeForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    match crate::upgrader::upgrade(&state, session.account, &form.tag).await {
        Ok(()) => Ok(super::stay::back(
            HEALTH,
            "Upgrade started: Tether restarts in a minute or two.",
        )),
        Err(err) => health_page(&state, shell, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct RollbackForm {
    #[serde(default)]
    confirmation: String,
}

/// `POST /admin/system/rollback`: back to the version before the last
/// upgrade, typed to confirm.
pub async fn rollback(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<RollbackForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    match crate::upgrader::rollback(&state, session.account, &form.confirmation).await {
        Ok(()) => Ok(super::stay::back(
            HEALTH,
            "Rollback started: Tether restarts in a minute or two.",
        )),
        Err(err) => health_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/system/updates/check`
pub async fn check_updates(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    match updates::check_now(&state, session.account).await {
        Ok(()) => Ok(super::stay::back(HEALTH, "Checked for updates.")),
        Err(err) => health_page(&state, shell, Some(err)).await,
    }
}

pub struct AuditRow {
    pub id: i64,
    pub at: String,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub details: String,
}

#[derive(Template)]
#[template(path = "admin_audit.html")]
struct AuditPage {
    shell: Shell,
    rows: Vec<AuditRow>,
    /// Link to the next (older) page.
    older: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
}

const AUDIT_PAGE: i64 = 50;

/// `GET /admin/audit`
pub async fn audit_log(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<AuditQuery>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_AUDIT, "audit").await?;
    let entries = audit::list(&state.db, AUDIT_PAGE + 1, query.before).await?;
    let more = entries.len() as i64 > AUDIT_PAGE;
    let rows: Vec<AuditRow> = entries
        .into_iter()
        .take(AUDIT_PAGE as usize)
        .map(|e| AuditRow {
            id: e.id,
            at: e.at.format("%Y-%m-%d %H:%M:%S").to_string(),
            actor: e.actor_name.unwrap_or_else(|| "System".to_owned()),
            action: e.action,
            target: e.target.unwrap_or_default(),
            details: if e.details.as_object().is_some_and(|o| o.is_empty()) {
                String::new()
            } else {
                e.details.to_string()
            },
        })
        .collect();
    let older = more.then(|| rows.last().map(|r| r.id)).flatten();
    Ok(render(StatusCode::OK, &AuditPage { shell, rows, older }))
}

#[derive(Template)]
#[template(path = "status_strip.html")]
struct StatusStrip {
    /// Pilots online, with thousands separators; `None` when ESI didn't
    /// answer (why is on Health, for admins).
    players: Option<String>,
}

/// How long the strip's answer stands, success or not.
const STRIP_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// `GET /status/strip`: Tranquility and ESI for every page's status strip
/// (DESIGN.md). Signed-in only, and at most one ESI call a minute for the
/// whole instance ([`crate::state::StripStatus`]).
pub async fn strip(
    State(state): State<AppState>,
    headers: HeaderMap,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    if session.is_none() {
        // The strip's poll outlived the session: the whole tab goes to log
        // in, rather than the login page landing inside the strip.
        if super::is_htmx(&headers) {
            return Ok(([("HX-Redirect", "/login")], StatusCode::OK).into_response());
        }
        return Err(AppError::unauthorized().into());
    }
    let players = {
        let mut last = state.strip.last.lock().await;
        match *last {
            Some((at, players)) if at.elapsed() < STRIP_FOR => players,
            _ => {
                let (players, _) = esi_status(&state).await;
                *last = Some((std::time::Instant::now(), players));
                players
            }
        }
    };
    Ok(render(
        StatusCode::OK,
        &StatusStrip {
            players: players.map(super::grouped),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_read_in_plain_words() {
        assert_eq!(every(3_600), "every hour");
        assert_eq!(every(6 * 3_600), "every 6 hours");
        assert_eq!(every(300), "every 5 minutes");
        assert_eq!(every(60), "every minute");
        assert_eq!(every(86_400), "every day");
        assert_eq!(every(2 * 86_400), "every 2 days");
        assert_eq!(every(45), "every 45 seconds");
    }

    fn line(tone: Tone) -> SystemLine {
        SystemLine {
            name: "X",
            detail: String::new(),
            value: String::new(),
            status: String::new(),
            tone,
            href: None,
        }
    }

    #[test]
    fn the_verdict_is_the_worst_line() {
        let now = DateTime::parse_from_rfc3339("2026-10-06T21:04:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = verdict(&[line(Tone::Ok), line(Tone::Off)], now);
        assert_eq!(v.tone, Tone::Ok);
        assert_eq!(v.headline, "All systems nominal");
        assert_eq!(v.detail, "checked 21:04 EVE");
        let v = verdict(&[line(Tone::Ok), line(Tone::Warn)], now);
        assert_eq!(v.tone, Tone::Warn);
        assert_eq!(v.headline, "Needs attention");
        assert_eq!(v.detail, "1 warning · checked 21:04 EVE");
        let v = verdict(
            &[line(Tone::Danger), line(Tone::Danger), line(Tone::Warn)],
            now,
        );
        assert_eq!(v.tone, Tone::Danger);
        assert_eq!(v.headline, "2 problems");
        assert_eq!(v.detail, "1 warning · checked 21:04 EVE");
    }
}
