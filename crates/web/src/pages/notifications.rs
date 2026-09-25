//! The Notifications page: only the recipient's own, newest first.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::Sse;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use tether_core::hash_token;
use tether_db::notifications::{self as db, Notification};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::{CurrentSession, SESSION_COOKIE};
use crate::error::AppError;
use crate::notifications::UnreadStream;

pub struct Row {
    pub id: i64,
    pub level: String,
    pub title: String,
    pub message: String,
    pub at: String,
    pub read: bool,
}

fn row(n: Notification) -> Row {
    Row {
        id: n.id,
        level: n.level,
        title: n.title,
        message: n.message,
        at: n.created_at.format("%Y-%m-%d %H:%M").to_string(),
        read: n.read,
    }
}

#[derive(Template)]
#[template(path = "notifications.html")]
struct ListPage {
    shell: Shell,
    rows: Vec<Row>,
    any_read: bool,
}

/// `GET /notifications`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let loaded = load(&state, &session, "notifications").await?;
    let rows: Vec<Row> = db::list(&state.db, session.account)
        .await?
        .into_iter()
        .map(row)
        .collect();
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell: loaded.shell,
            any_read: rows.iter().any(|r| r.read),
            rows,
        },
    ))
}

#[derive(Template)]
#[template(path = "notification.html")]
struct OnePage {
    shell: Shell,
    n: Row,
}

/// `GET /notifications/{id}`: opens one, marking it read.
pub async fn show(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let n = db::open(&state.db, session.account, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such notification."))?;
    // Loaded after opening, so the unread count already leaves it out.
    let loaded = load(&state, &session, "notifications").await?;
    Ok(render(
        StatusCode::OK,
        &OnePage {
            shell: loaded.shell,
            n: row(n),
        },
    ))
}

/// `GET /notifications/stream`: server-sent `unread` events with the top
/// bar's bell, for `assets/notifications.js`. A new stream ends the
/// account's oldest beyond [`crate::notifications::MAX_STREAMS`].
pub async fn stream(
    State(state): State<AppState>,
    session: CurrentSession,
    jar: CookieJar,
) -> Result<Response, AppError> {
    // CurrentSession found this cookie's session, so it's there.
    let hash = jar
        .get(SESSION_COOKIE)
        .map(|c| hash_token(c.value()))
        .ok_or_else(AppError::unauthorized)?;
    let stream = UnreadStream::new(state.db.clone(), &state.notices, session.account, hash);
    Ok(Sse::new(stream)
        .keep_alive(UnreadStream::keep_alive())
        .into_response())
}

/// `POST /notifications/{id}/delete`
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    if !db::delete(&state.db, session.account, id).await? {
        return Err(AppError::not_found("No such notification.").into());
    }
    Ok(Redirect::to("/notifications").into_response())
}

/// `POST /notifications/read-all`
pub async fn read_all(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    db::mark_all_read(&state.db, session.account).await?;
    Ok(Redirect::to("/notifications").into_response())
}

/// `POST /notifications/delete-read`
pub async fn delete_read(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    db::delete_read(&state.db, session.account).await?;
    Ok(Redirect::to("/notifications").into_response())
}
