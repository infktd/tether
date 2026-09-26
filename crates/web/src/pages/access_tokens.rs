//! `/dashboard/access-tokens`: personal access tokens for bots and scripts.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use tether_db::personal_tokens::PersonalToken;

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::personal_tokens::{self as tokens, MAX_DAYS};

#[derive(Template)]
#[template(path = "access_tokens.html")]
struct TokensPage {
    shell: Shell,
    tokens: Vec<PersonalToken>,
    offered: Vec<String>,
    max_days: i64,
    /// A token just made: shown this once.
    created: Option<String>,
    error: Option<String>,
}

impl TokensPage {
    fn when(&self, at: &chrono::DateTime<chrono::Utc>) -> String {
        at.format("%Y-%m-%d").to_string()
    }

    fn expired(&self, token: &PersonalToken) -> bool {
        token.expires_at <= chrono::Utc::now()
    }
}

async fn page(
    state: &AppState,
    session: &CurrentSession,
    created: Option<String>,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    // A browser session only: tokens never manage tokens.
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    let loaded = load(state, session, "access_tokens").await?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &TokensPage {
            shell: loaded.shell,
            tokens: tether_db::personal_tokens::for_account(&state.db, session.account).await?,
            offered: tokens::offered(state, session.account).await?,
            max_days: MAX_DAYS,
            created,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /dashboard/access-tokens`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    page(&state, &session, None, None).await
}

/// `POST /dashboard/access-tokens`: `name`, `days`, and a `scopes` field
/// per scope.
pub async fn create(
    State(state): State<AppState>,
    session: CurrentSession,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    };
    let scopes: Vec<String> = fields
        .iter()
        .filter(|(k, _)| k == "scopes")
        .map(|(_, v)| v.clone())
        .collect();
    match tokens::create(
        &state,
        session.account,
        field("name"),
        &scopes,
        field("days"),
    )
    .await
    {
        Ok(token) => {
            let mut response = page(&state, &session, Some(token.expose().clone()), None).await?;
            // Shown once: never from a cache or history.
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            Ok(response)
        }
        Err(err) => page(&state, &session, None, Some(err)).await,
    }
}

/// `POST /dashboard/access-tokens/{id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    match tokens::revoke(&state, session.account, id).await {
        Ok(()) => Ok(Redirect::to("/dashboard/access-tokens").into_response()),
        Err(err) => page(&state, &session, None, Some(err)).await,
    }
}
