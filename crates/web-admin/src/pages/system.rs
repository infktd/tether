//! The admin dashboard (F14): ESI health and error budget, the job queue,
//! platform updates (`/admin/system`), and the audit log (`/admin/audit`).

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
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
    upgrade: crate::upgrader::View,
    accent: String,
    presets: Vec<Preset>,
    /// The accent isn't one of the presets.
    custom: bool,
    /// AA's `NOTIFICATIONS_MAX_PER_USER`.
    notifications_max: i64,
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
    let problem = error.as_ref().map(|e| e.message().to_owned());
    let accent = crate::theme::accent(&state.db).await?;
    Ok(super::with_problem(
        problem,
        render(
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
                upgrade: crate::upgrader::view(state).await?,
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
                error: error.map(|e| e.message().to_owned()),
            },
        ),
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
    Ok(super::stay::back("/admin/system", "Job queued again."))
}

/// Runs a schedule now for an admin (the System page's or an app page's
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
        Ok(()) => Ok(super::stay::back("/admin/system", "Queued.")),
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
    Ok(super::stay::back(
        "/admin/system",
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
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
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
        Ok(()) => Ok(super::stay::back(
            "/admin/system",
            "Notification limit saved.",
        )),
        Err(err) => system_page(&state, shell, Some(err)).await,
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
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
    let (back, done) = match form.from.as_str() {
        "setup" => ("/setup", "Site name saved."),
        _ => ("/admin/system", "Site name saved."),
    };
    match crate::site_name::set(&state, session.account, &form.site_name).await {
        // The name is in the tab title: reload the page for it.
        Ok(_) => Ok(super::stay::back(back, done)),
        Err(err) if back == "/setup" => Err(err.into()),
        Err(err) => system_page(&state, shell, Some(err)).await,
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
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "system").await?;
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
        Ok(()) => Ok(Redirect::to("/admin/system").into_response()),
        Err(err) => system_page(&state, shell, Some(err)).await,
    }
}

#[derive(Template)]
#[template(path = "admin_system_upgrade.html")]
struct UpgradeCard {
    upgrade: crate::upgrader::View,
}

/// `GET /admin/system/upgrade`: the Upgrade card, which polls itself while
/// the updater works.
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
            "/admin/system",
            "Upgrade started: Tether restarts in a minute or two.",
        )),
        Err(err) => system_page(&state, shell, Some(err)).await,
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
            "/admin/system",
            "Rollback started: Tether restarts in a minute or two.",
        )),
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
        Ok(()) => Ok(super::stay::back("/admin/system", "Checked for updates.")),
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

#[derive(Template)]
#[template(path = "status_strip.html")]
struct StatusStrip {
    /// Pilots online, with thousands separators; `None` when ESI didn't
    /// answer (why is on System, for admins).
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
