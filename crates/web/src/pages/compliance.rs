//! Scope compliance pages (F11): the registration checklist every pilot
//! sees after login, and the officers' page listing accounts that aren't
//! compliant and Corp Stats (corporation members who never registered).

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
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
}

/// `GET /register`: what each character still needs.
pub async fn register(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    let loaded = load(&state, &session, "profile").await?;
    let status = compliance::registration(&state.db, session.account).await?;
    let page = RegisterPage {
        shell: loaded.shell,
        target_style: status.target.as_ref().map_or("guest", |t| t.style()),
        target: status.target.as_ref().map(|t| t.name.clone()),
        flagged: status.flagged,
        done: status.done(),
        required: status
            .required
            .iter()
            .map(|scope| RequiredScope {
                description: tether_core::scopes::describe(scope).to_owned(),
                scope: scope.clone(),
            })
            .collect(),
        characters: status
            .characters
            .iter()
            .map(|c| CheckRow {
                id: c.id,
                name: c.name.clone(),
                is_main: c.is_main,
                problem: c.problem.as_ref().map(problem_text),
            })
            .collect(),
    };
    Ok(render(StatusCode::OK, &page))
}

/// `POST /register/start`: off to EVE SSO with the required scopes, to
/// register a character or add a new one.
pub async fn start(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    Ok(compliance::start_register(&state, jar, session.account).await?)
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
