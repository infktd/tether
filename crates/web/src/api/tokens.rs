//! Token Management: your characters' tokens (never the tokens themselves).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::tokens::{self, Refreshed};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TokenOut {
    pub character_id: i64,
    pub character_name: String,
    pub is_main: bool,
    pub scopes: Vec<String>,
    /// `valid`, `revoked` or `deleted`.
    pub state: &'static str,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

/// `GET /api/tokens`
#[utoipa::path(get, path = "/api/tokens", tag = "account", security(("session" = [])),
    responses((status = 200, body = Vec<TokenOut>)))]
pub async fn list(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<TokenOut>>, AppError> {
    Ok(Json(
        tokens::list(&state.db, session.account)
            .await?
            .into_iter()
            .map(|t| TokenOut {
                state: match (t.revoked, t.revoked_reason.as_deref()) {
                    (false, _) => "valid",
                    (true, Some("deleted")) => "deleted",
                    (true, _) => "revoked",
                },
                character_id: t.character_id,
                character_name: t.character_name,
                is_main: t.is_main,
                scopes: t.scopes,
                created_at: t.created_at,
                last_refreshed_at: t.last_refreshed_at,
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RefreshOut {
    /// `valid`, `revoked` (log in with it again) or `sold` (it left the
    /// account).
    pub result: &'static str,
}

/// `POST /api/tokens/{character_id}/refresh`: refresh now, with the
/// ownership check.
#[utoipa::path(post, path = "/api/tokens/{character_id}/refresh", tag = "account", security(("session" = [])),
    params(("character_id" = i64, Path)),
    responses((status = 200, body = RefreshOut), (status = 404), (status = 502, description = "EVE SSO didn't answer")))]
pub async fn refresh(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(character): Path<i64>,
) -> Result<Json<RefreshOut>, AppError> {
    let result = match tokens::refresh(
        &state.db,
        &state.vault,
        &state.limits,
        session.account,
        character,
    )
    .await?
    {
        Refreshed::Valid => "valid",
        Refreshed::Revoked => "revoked",
        Refreshed::Sold => "sold",
    };
    Ok(Json(RefreshOut { result }))
}

/// `DELETE /api/tokens/{character_id}`
#[utoipa::path(delete, path = "/api/tokens/{character_id}", tag = "account", security(("session" = [])),
    params(("character_id" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(character): Path<i64>,
) -> Result<StatusCode, AppError> {
    tokens::delete(&state.db, &state.vault, session.account, character).await?;
    Ok(StatusCode::NO_CONTENT)
}
