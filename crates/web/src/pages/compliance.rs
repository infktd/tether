//! Scope compliance pages (F11): the registration checklist every pilot
//! sees after login, and each app's (F16: registering characters for an
//! app, as AA's Member Audit), and the officers' page listing accounts
//! that aren't compliant and Corp Stats (corporation members who never
//! registered).

use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::permissions::COMPLIANCE_VIEW;
use tether_core::scopes::Problem;
use tether_db::compliance as db;

use super::admin::guard;
use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::compliance;
use crate::error::AppError;

fn problem_text(problem: &Problem) -> String {
    match problem {
        Problem::NotRegistered => "Not registered yet".to_owned(),
        Problem::Revoked => "EVE access was revoked".to_owned(),
        Problem::Missing(scopes) => format!(
            "Missing {}",
            scopes
                .iter()
                .map(|s| tether_core::scopes::describe(s))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

// ---- the checklist -----------------------------------------------------------

pub struct RequiredScope {
    pub scope: String,
    pub description: String,
}

pub struct CheckRow {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    /// What's wrong, or `None` when it's done.
    pub problem: Option<String>,
}

/// An app the pilot may register characters for, on the checklist.
pub struct AppLine {
    pub id: String,
    pub name: String,
    pub registered: usize,
    pub characters: usize,
}

/// Registering for one app.
pub struct AppView {
    pub id: String,
    pub name: String,
}

#[derive(Template)]
#[template(path = "register.html")]
struct RegisterPage {
    shell: Shell,
    /// The state being worked towards; `None` for Guest.
    target: Option<String>,
    target_style: &'static str,
    flagged: bool,
    required: Vec<RequiredScope>,
    characters: Vec<CheckRow>,
    done: bool,
    /// Registering for this app, rather than for the state.
    app: Option<AppView>,
    /// Where the Register buttons post.
    start_action: String,
    /// The apps the pilot may register characters for (the state's
    /// checklist only).
    apps: Vec<AppLine>,
}

fn rows(characters: &[compliance::CharacterStatus]) -> Vec<CheckRow> {
    characters
        .iter()
        .map(|c| CheckRow {
            id: c.id,
            name: c.name.clone(),
            is_main: c.is_main,
            problem: c.problem.as_ref().map(problem_text),
        })
        .collect()
}

fn required(scopes: &std::collections::BTreeSet<String>) -> Vec<RequiredScope> {
    scopes
        .iter()
        .map(|scope| RequiredScope {
            description: tether_core::scopes::describe(scope).to_owned(),
            scope: scope.clone(),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct RegisterQuery {
    /// An app's id: register characters for it.
    app: Option<String>,
}

/// `GET /register`: what each character still needs for the account's
/// state; with `?app=<id>`, for that app (only for holders of one of its
/// permissions).
pub async fn register(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<RegisterQuery>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    let loaded = load(&state, &session, "profile").await?;
    let status = compliance::registration(&state.db, session.account).await?;
    let target_style = status.target.as_ref().map_or("guest", |t| t.style());
    let target = status.target.as_ref().map(|t| t.name.clone());
    let page = match query.app.as_deref() {
        Some(id) => {
            let app = compliance::app_registration(&state, session.account, id)
                .await?
                .ok_or_else(|| AppError::not_found("No app you may register characters for."))?;
            RegisterPage {
                shell: loaded.shell,
                target_style,
                target,
                flagged: status.flagged,
                done: app.registered() == app.characters.len(),
                required: required(&app.scopes),
                characters: rows(&app.characters),
                start_action: format!("/register/start?app={}", app.id),
                app: Some(AppView {
                    id: app.id,
                    name: app.name,
                }),
                apps: Vec::new(),
            }
        }
        None => RegisterPage {
            shell: loaded.shell,
            target_style,
            target,
            flagged: status.flagged,
            done: status.done(),
            required: required(&status.required),
            characters: rows(&status.characters),
            app: None,
            start_action: "/register/start".to_owned(),
            apps: compliance::app_registrations(&state, session.account)
                .await?
                .into_iter()
                .map(|a| AppLine {
                    registered: a.registered(),
                    characters: a.characters.len(),
                    id: a.id,
                    name: a.name,
                })
                .collect(),
        },
    };
    Ok(render(StatusCode::OK, &page))
}

#[derive(Debug, Deserialize)]
pub struct StartForm {
    /// Register for this app too.
    app: Option<String>,
}

/// `POST /register/start`: off to EVE SSO with the required scopes (and an
/// app's, with `?app=<id>`), to register a character or add a new one.
pub async fn start(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Query(form): Query<StartForm>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let app = form.app.as_deref().filter(|a| !a.is_empty());
    Ok(compliance::start_register(&state, jar, session.account, app).await?)
}

// ---- the officers' page ------------------------------------------------------

pub struct Shortfall {
    pub name: String,
    pub is_main: bool,
    pub problem: String,
}

pub struct NotCompliantRow {
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: &'static str,
    pub since: String,
    pub shortfalls: Vec<Shortfall>,
}

pub struct Unregistered {
    pub id: i64,
    pub name: String,
}

pub struct CorpRow {
    pub id: i64,
    pub name: String,
    pub members: i32,
    pub registered: i64,
    /// Registered members as a whole percentage.
    pub coverage: i64,
    pub fetched: String,
    pub unregistered: Vec<Unregistered>,
}

pub struct UncoveredCorp {
    pub id: i64,
    pub name: String,
}

#[derive(Template)]
#[template(path = "compliance.html")]
struct CompliancePage {
    shell: Shell,
    not_compliant: Vec<NotCompliantRow>,
    corporations: Vec<CorpRow>,
    uncovered: Vec<UncoveredCorp>,
    unregistered_total: i64,
}

/// Unregistered members shown per corporation; the count covers the rest.
const MAX_UNREGISTERED_SHOWN: usize = 500;

fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M EVE").to_string()
}

/// `GET /compliance`
pub async fn page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, COMPLIANCE_VIEW, "compliance").await?;
    let states = tether_db::states::list(&state.db).await?;
    let mut not_compliant = Vec::new();
    for row in db::not_compliant(&state.db).await? {
        let current = states.iter().find(|s| s.id == row.state);
        let status = compliance::registration(&state.db, row.account).await?;
        not_compliant.push(NotCompliantRow {
            main_id: row.main_id,
            main_name: row.main_name,
            state: current.map_or_else(|| "?".to_owned(), |s| s.name.clone()),
            state_style: current.map_or("custom", |s| s.style()),
            since: row.since.map(when).unwrap_or_default(),
            shortfalls: status
                .characters
                .iter()
                .filter_map(|c| {
                    Some(Shortfall {
                        problem: problem_text(c.problem.as_ref()?),
                        name: c.name.clone(),
                        is_main: c.is_main,
                    })
                })
                .collect(),
        });
    }
    let mut corporations = Vec::new();
    let mut unregistered_total = 0;
    for list in db::member_lists(&state.db).await? {
        let missing = db::unregistered(&state.db, list.corporation_id).await?;
        unregistered_total += i64::try_from(missing.len()).unwrap_or(i64::MAX);
        corporations.push(CorpRow {
            id: list.corporation_id,
            name: list
                .corporation_name
                .unwrap_or_else(|| "Unknown corporation".to_owned()),
            members: list.members,
            registered: list.registered,
            coverage: if list.members > 0 {
                list.registered * 100 / i64::from(list.members)
            } else {
                100
            },
            fetched: when(list.fetched_at),
            unregistered: missing
                .into_iter()
                .take(MAX_UNREGISTERED_SHOWN)
                .map(|(id, name)| Unregistered {
                    id,
                    name: name.unwrap_or_else(|| "Unknown character".to_owned()),
                })
                .collect(),
        });
    }
    let uncovered = db::corporations_without_lists(&state.db)
        .await?
        .into_iter()
        .map(|(id, name)| UncoveredCorp {
            id,
            name: name.unwrap_or_else(|| "Unknown corporation".to_owned()),
        })
        .collect();
    Ok(render(
        StatusCode::OK,
        &CompliancePage {
            shell,
            not_compliant,
            corporations,
            uncovered,
            unregistered_total,
        },
    ))
}
