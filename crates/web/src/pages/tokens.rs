//! Token Management: the signed-in account's tokens.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::tokens::{self, Refreshed};

pub struct ScopeRow {
    pub scope: String,
    pub description: String,
}

pub struct TokenRow {
    pub character_id: i64,
    pub name: String,
    pub is_main: bool,
    pub scopes: Vec<ScopeRow>,
    pub revoked: bool,
    pub deleted: bool,
    pub created: String,
    pub refreshed: String,
}

#[derive(Template)]
#[template(path = "tokens.html")]
struct TokensPage {
    shell: Shell,
    rows: Vec<TokenRow>,
    notice: Option<String>,
    error: Option<String>,
}

async fn tokens_page(
    state: &AppState,
    session: &CurrentSession,
    notice: Option<&str>,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let loaded = load(state, session, "tokens").await?;
    let rows = tokens::list(&state.db, session.account)
        .await?
        .into_iter()
        .map(|t| TokenRow {
            character_id: t.character_id,
            name: t.character_name,
            is_main: t.is_main,
            scopes: t
                .scopes
                .iter()
                .map(|s| ScopeRow {
                    description: tether_core::scopes::describe(s).to_owned(),
                    scope: s.clone(),
                })
                .collect(),
            revoked: t.revoked,
            deleted: t.revoked_reason.as_deref() == Some("deleted"),
            created: t.created_at.format("%Y-%m-%d").to_string(),
            refreshed: t.last_refreshed_at.map_or_else(
                || "never".to_owned(),
                |at| at.format("%Y-%m-%d %H:%M").to_string(),
            ),
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &TokensPage {
            shell: loaded.shell,
            rows,
            notice: notice.map(str::to_owned),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /tokens`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    tokens_page(&state, &session, None, None).await
}

/// `POST /tokens/{character_id}/refresh`
pub async fn refresh(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(character): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    match tokens::refresh(&state.db, &state.vault, &state.limits, session.account, character).await {
        Ok(Refreshed::Valid) => tokens_page(&state, &session, Some("Refreshed: the token works."), None).await,
        Ok(Refreshed::Revoked) => {
            tokens_page(
                &state,
                &session,
                Some("EVE says that token no longer works. Log in with the character again through Add Character."),
                None,
            )
            .await
        }
        // The page may now be Guest's (if it was the main), so it's
        // loaded fresh.
        Ok(Refreshed::Sold) => {
            tokens_page(
                &state,
                &session,
                Some("That character now belongs to another EVE account, so it has left yours."),
                None,
            )
            .await
        }
        Err(err) => tokens_page(&state, &session, None, Some(err)).await,
    }
}

/// `POST /tokens/{character_id}/delete`
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(character): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    match tokens::delete(&state.db, &state.vault, session.account, character).await {
        Ok(()) => {
            tokens_page(
                &state,
                &session,
                Some("Token deleted. The character leaves your account in a day unless you log in with it again."),
                None,
            )
            .await
        }
        Err(err) => tokens_page(&state, &session, None, Some(err)).await,
    }
}
