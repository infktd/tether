//! The admin dashboard (F14): ESI health and error budget, the job queue,
//! platform updates (`/admin/system`), and the audit log (`/admin/audit`).

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;
use tether_core::permissions::{ADMIN_AUDIT, ADMIN_SYSTEM};
use tether_db::audit::{self, Actor};
use tether_esi::budget::BudgetSnapshot;
use tether_jobs::{JobId, JobState};

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::updates;

pub struct JobCount {
    pub state: &'static str,
    pub count: i64,
}

pub struct DeadJob {
    pub id: i64,
    pub kind: String,
    pub attempts: i32,
    pub when: String,
    pub error: String,
}

pub struct ScheduleView {
    pub name: String,
    pub every: String,
    pub enabled: bool,
    pub next_run: String,
    pub last_run: String,
}

#[derive(Template)]
#[template(path = "admin_system.html")]
struct SystemPage {
    shell: Shell,
    esi_online: Option<i64>,
    esi_error: Option<String>,
    budget: BudgetSnapshot,
    jobs: Vec<JobCount>,
    dead_count: i64,
    dead: Vec<DeadJob>,
    schedules: Vec<ScheduleView>,
    updates: updates::Status,
    accent: String,
    presets: Vec<Preset>,
    /// The accent isn't one of the presets.
    custom: bool,
    error: Option<String>,
}

pub struct Preset {
    pub name: &'static str,
    pub value: &'static str,
    pub checked: bool,
}

fn every(secs: i32) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("every {} day(s)", s / 86_400),
        s if s % 3_600 == 0 => format!("every {} hour(s)", s / 3_600),
        s if s % 60 == 0 => format!("every {} minute(s)", s / 60),
        s => format!("every {s} s"),
    }
}

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
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

async fn system_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let (esi_online, esi_error) = esi_status(state).await;
    let counts = tether_jobs::counts(&state.db).await?;
    let dead_count = counts
        .iter()
        .find(|(s, _)| *s == JobState::Dead)
        .map_or(0, |(_, n)| *n);
    let jobs = counts
        .into_iter()
        .map(|(state, count)| JobCount {
            state: state.as_str(),
            count,
        })
        .collect();
    let dead = tether_jobs::list(&state.db, Some(JobState::Dead), 20)
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
    let schedules = tether_jobs::schedule::list(&state.db)
        .await?
        .into_iter()
        .map(|s| ScheduleView {
            name: s.name,
            every: every(s.every_secs),
            enabled: s.enabled,
            next_run: time(s.next_run_at),
            last_run: s.last_enqueued_at.map_or_else(|| "never".to_owned(), time),
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let accent = crate::theme::accent(&state.db).await?;
    Ok(render(
        code,
        &SystemPage {
            shell,
            esi_online,
            esi_error,
            budget: state.esi.budget(),
            jobs,
            dead_count,
            dead,
            schedules,
            updates: updates::status(&state.db).await?,
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
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

#[derive(Template)]
#[template(path = "admin_system_summary.html")]
struct SystemPanel {
    esi_online: Option<i64>,
    esi_error: Option<String>,
    budget: BudgetSnapshot,
    queued: i64,
    running: i64,
    dead: i64,
    updates: updates::Status,
}

/// `GET /admin/system/summary`: the System panel at the top of
/// Administration's overview (AA's Dashboard admin panels: Software
/// Version, Task Queue and ESI status), a fragment loaded after the page so
/// a slow ESI never holds the overview up.
pub async fn summary(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(&state, ADMIN_SYSTEM).await?;
    let (esi_online, esi_error) = esi_status(&state).await;
    let counts = tether_jobs::counts(&state.db).await?;
    let count = |wanted: JobState| {
        counts
            .iter()
            .find(|(s, _)| *s == wanted)
            .map_or(0, |(_, n)| *n)
    };
    Ok(render(
        StatusCode::OK,
        &SystemPanel {
            esi_online,
            esi_error,
            budget: state.esi.budget(),
            queued: count(JobState::Queued),
            running: count(JobState::Running),
            dead: count(JobState::Dead),
            updates: updates::status(&state.db).await?,
        },
    ))
}

/// `GET /admin/system`
pub async fn system(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    system_page(&state, shell, None).await
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
        return system_page(&state, shell, Some(err)).await;
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
    Ok(Redirect::to("/admin/system").into_response())
}

/// Runs a schedule now for an admin (the System page's or an app page's
/// Run now), audited as `schedule.run_now`.
pub(crate) async fn run_schedule(
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
pub(crate) fn run_now_fragment(result: &Result<(), AppError>) -> Response {
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
        Ok(()) => Ok(Redirect::to("/admin/system").into_response()),
        Err(err) => system_page(&state, shell, Some(err)).await,
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
    let (session, _) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    updates::set_enabled(&state, session.account, form.enabled.is_some()).await?;
    Ok(Redirect::to("/admin/system").into_response())
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
    Form(form): Form<ThemeForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    let chosen = if form.accent == "custom" {
        &form.custom_accent
    } else {
        &form.accent
    };
    match crate::theme::set_accent(&state, session.account, chosen).await {
        Ok(()) => Ok(Redirect::to("/admin/system").into_response()),
        Err(err) => system_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/system/updates/check`
pub async fn check_updates(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    match updates::check_now(&state, session.account).await {
        Ok(()) => Ok(Redirect::to("/admin/system").into_response()),
        Err(err) => system_page(&state, shell, Some(err)).await,
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
