//! The Notifications page: only the recipient's own, newest first.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, StatusCode};
use axum::response::sse::Sse;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::hash_token;
use tether_db::notifications::{self as db, Notification};

use super::toolbar::{self, ListQuery, ToolbarView};
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
    /// The app that sent it (its id, which no other app can take); none
    /// for Tether's own.
    pub app: Option<String>,
}

fn row(n: Notification) -> Row {
    Row {
        id: n.id,
        level: n.level,
        title: n.title,
        message: n.message,
        at: n.created_at.format("%Y-%m-%d %H:%M").to_string(),
        read: n.read,
        app: n.plugin_id,
    }
}

#[derive(Template)]
#[template(path = "notifications.html")]
struct ListPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under the toolbar's search, filters or tab.
    searched: bool,
    rows: Vec<Row>,
    any_read: bool,
}

/// The toolbar: the search, the tab (`unread`, `read`), the level and the
/// app that sent them (`tether` for Tether's own).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    show: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    app: String,
}

/// The app filter's value for Tether's own notifications.
const TETHER: &str = "tether";

/// `GET /notifications`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let loaded = load(&state, &session, "notifications").await?;
    let all: Vec<Row> = db::list(&state.db, session.account)
        .await?
        .into_iter()
        .map(row)
        .collect();
    let any_read = all.iter().any(|r| r.read);
    let mut levels: Vec<String> = Vec::new();
    let mut apps: Vec<String> = Vec::new();
    for r in &all {
        if !levels.contains(&r.level) {
            levels.push(r.level.clone());
        }
        let app = r.app.clone().unwrap_or_else(|| TETHER.to_owned());
        if !apps.contains(&app) {
            apps.push(app);
        }
    }
    levels.sort();
    apps.sort();
    let show = Some(params.show.trim()).filter(|s| ["unread", "read"].contains(s));
    let level = Some(params.level.trim()).filter(|l| levels.iter().any(|x| x == l));
    let app = Some(params.app.trim()).filter(|a| apps.iter().any(|x| x == a));
    let list = ListQuery::new("/notifications")
        .param("q", &params.q)
        .param("show", show.unwrap_or(""))
        .param("level", level.unwrap_or(""))
        .param("app", app.unwrap_or(""));
    let words = list.words();
    let rows: Vec<Row> = all
        .into_iter()
        .filter(|r| show.is_none_or(|s| r.read == (s == "read")))
        .filter(|r| level.is_none_or(|l| r.level == l))
        .filter(|r| app.is_none_or(|a| r.app.as_deref().unwrap_or(TETHER) == a))
        .filter(|r| toolbar::matches(&words, &[&r.title, &r.message]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search notifications")
        .filter(
            &list,
            "Level",
            "level",
            levels.iter().map(|l| (l.clone(), l.clone())),
        )
        .filter(
            &list,
            "From",
            "app",
            apps.iter().map(|a| {
                let label = if a == TETHER { "Tether" } else { a.as_str() };
                (a.clone(), label.to_owned())
            }),
        )
        .views(
            &list,
            "show",
            [("all", "All"), ("unread", "Unread"), ("read", "Read")],
        );
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell: loaded.shell,
            searched: list.href() != list.path,
            toolbar,
            any_read,
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
/// bar's bell, and `app` events when an app's data changed, for
/// `assets/notifications.js`. A new stream ends the
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
    // Changes of the apps it may open, so an open page of one refreshes.
    let held = tether_db::permissions::effective(&state.db, session.account).await?;
    let blacklisted = tether_db::states::account_state(&state.db, session.account)
        .await?
        .is_some_and(|s| s.is_blacklist());
    let apps = state.plugins.watchable(blacklisted, |p| held.contains(p));
    let stream = UnreadStream::new(
        state.db.clone(),
        &state.notices,
        session.account,
        hash,
        apps,
    );
    // nginx buffers proxied responses unless told not to, which would hold
    // events back; the generated server block turns buffering off here too,
    // but an admin's own nginx config may not.
    Ok((
        [(HeaderName::from_static("x-accel-buffering"), "no")],
        Sse::new(stream).keep_alive(UnreadStream::keep_alive()),
    )
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
    Ok(super::stay::back("/notifications", "Notification deleted."))
}

/// `POST /notifications/read-all`
pub async fn read_all(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    db::mark_all_read(&state.db, session.account).await?;
    Ok(super::stay::back("/notifications", "All marked as read."))
}

/// `POST /notifications/delete-read`
pub async fn delete_read(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    db::delete_read(&state.db, session.account).await?;
    Ok(super::stay::back(
        "/notifications",
        "Read notifications deleted.",
    ))
}
