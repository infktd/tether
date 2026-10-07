//! `/sessions`: where the account is signed in, from the account menu,
//! with Sign out per session and Sign out everywhere else
//! ([`crate::sessions`]).

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use tether_db::auth::SessionRow;

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::sessions;

#[derive(Template)]
#[template(path = "sessions.html")]
struct SessionsPage {
    shell: Shell,
    rows: Vec<SessionRow>,
    /// This browser's session.
    current: i64,
    error: Option<String>,
}

impl SessionsPage {
    fn when(&self, at: &DateTime<Utc>) -> String {
        at.format("%Y-%m-%d %H:%M EVE").to_string()
    }

    fn others(&self) -> usize {
        self.rows.iter().filter(|r| r.id != self.current).count()
    }
}

async fn page(
    state: &AppState,
    session: &CurrentSession,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let (rows, current) = sessions::list(state, session).await?;
    let loaded = load(state, session, "sessions").await?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            code,
            &SessionsPage {
                shell: loaded.shell,
                rows,
                current,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /sessions`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    page(&state, &session, None).await
}

/// `POST /sessions/{id}/sign-out`: one of the account's other sessions.
pub async fn sign_out(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    match sessions::sign_out(&state, &session, id).await {
        Ok(()) => Ok(super::stay::back("/sessions", "Signed out.")),
        Err(err) if err.status() == StatusCode::FORBIDDEN => Err(err.into()),
        Err(err) => page(&state, &session, Some(err)).await,
    }
}

/// `POST /sessions/sign-out-others`: every session but this browser's.
pub async fn sign_out_others(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Response, PageError> {
    match sessions::sign_out_others(&state, &session).await {
        Ok(ended) => Ok(super::stay::back(
            "/sessions",
            format!("Signed out of {}.", sessions::sessions(ended)),
        )),
        Err(err) if err.status() == StatusCode::FORBIDDEN => Err(err.into()),
        Err(err) => page(&state, &session, Some(err)).await,
    }
}
