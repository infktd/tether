//! Your notifications: only the recipient sees them.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tether_db::notifications::{self as db, Notification};

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NotificationOut {
    pub id: i64,
    /// `danger`, `warning`, `info` or `success`.
    pub level: String,
    pub title: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub read: bool,
}

fn out(n: Notification) -> NotificationOut {
    NotificationOut {
        id: n.id,
        level: n.level,
        title: n.title,
        message: n.message,
        created_at: n.created_at,
        read: n.read,
    }
}

/// `GET /api/notifications`: your latest 50, newest first.
#[utoipa::path(get, path = "/api/notifications", tag = "notifications", security(("session" = [])),
    responses((status = 200, body = Vec<NotificationOut>)))]
pub async fn list(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<NotificationOut>>, AppError> {
    Ok(Json(
        db::list(&state.db, session.account)
            .await?
            .into_iter()
            .map(out)
            .collect(),
    ))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UnreadOut {
    pub unread: i64,
}

/// `GET /api/notifications/unread`
#[utoipa::path(get, path = "/api/notifications/unread", tag = "notifications", security(("session" = [])),
    responses((status = 200, body = UnreadOut)))]
pub async fn unread(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<UnreadOut>, AppError> {
    Ok(Json(UnreadOut {
        unread: db::unread(&state.db, session.account).await?,
    }))
}

/// `POST /api/notifications/{id}/open`: returns it and marks it read.
#[utoipa::path(post, path = "/api/notifications/{id}/open", tag = "notifications", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 200, body = NotificationOut), (status = 404)))]
pub async fn open(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Json<NotificationOut>, AppError> {
    db::open(&state.db, session.account, id)
        .await?
        .map(|n| Json(out(n)))
        .ok_or_else(|| AppError::not_found("No such notification."))
}

/// `DELETE /api/notifications/{id}`
#[utoipa::path(delete, path = "/api/notifications/{id}", tag = "notifications", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 204), (status = 404)))]
pub async fn delete(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    if db::delete(&state.db, session.account, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("No such notification."))
    }
}

/// `POST /api/notifications/read-all`
#[utoipa::path(post, path = "/api/notifications/read-all", tag = "notifications", security(("session" = [])),
    responses((status = 204)))]
pub async fn read_all(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<StatusCode, AppError> {
    db::mark_all_read(&state.db, session.account).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/delete-read`
#[utoipa::path(post, path = "/api/notifications/delete-read", tag = "notifications", security(("session" = [])),
    responses((status = 204)))]
pub async fn delete_read(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<StatusCode, AppError> {
    db::delete_read(&state.db, session.account).await?;
    Ok(StatusCode::NO_CONTENT)
}
