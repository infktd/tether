//! `/blacklist`: the Blacklist and the Pilot Log (AA's blacklist app).

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{BLACKLIST_ADD_NOTES, BLACKLIST_MANAGE, BLACKLIST_VIEW};
use tether_db::blacklist::{self as db, Listed, Note};

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::blacklist;
use crate::error::AppError;

pub fn when(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

#[derive(Template)]
#[template(path = "blacklist.html")]
struct BlacklistPage {
    shell: Shell,
    listed: Vec<Listed>,
    notes: Vec<Note>,
    query: String,
    manage: bool,
    add_notes: bool,
    me: i64,
    error: Option<String>,
}

impl BlacklistPage {
    /// Authors delete their own notes; managers anyone's.
    fn can_delete(&self, note: &Note) -> bool {
        self.manage || note.added_by == Some(self.me)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct NotesQuery {
    #[serde(default)]
    q: String,
}

async fn page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    query: String,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let held = tether_db::permissions::effective(&state.db, session.account).await?;
    let q: String = query.trim().chars().take(100).collect();
    let notes = db::notes(&state.db, None, (!q.is_empty()).then_some(q.as_str()), 200).await?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &BlacklistPage {
            shell,
            listed: db::list(&state.db).await?,
            notes,
            query,
            manage: held.contains(BLACKLIST_MANAGE),
            add_notes: held.contains(BLACKLIST_ADD_NOTES),
            me: session.account.0,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /blacklist`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<NotesQuery>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, BLACKLIST_VIEW, "blacklist").await?;
    page(&state, &session, shell, query.q, None).await
}

async fn done(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    result: Result<(), AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(()) => Ok(Redirect::to("/blacklist").into_response()),
        Err(err) => page(state, session, shell, String::new(), Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct AddForm {
    who: String,
    #[serde(default)]
    reason: String,
}

/// `POST /blacklist`
pub async fn add(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<AddForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, BLACKLIST_MANAGE, "blacklist").await?;
    session.require(&state, BLACKLIST_VIEW).await?;
    let result = blacklist::add(&state, session.account, &form.who, &form.reason).await;
    done(&state, &session, shell, result).await
}

/// `POST /blacklist/{entity_id}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(entity_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, BLACKLIST_MANAGE, "blacklist").await?;
    session.require(&state, BLACKLIST_VIEW).await?;
    let result = blacklist::remove(&state, session.account, entity_id).await;
    done(&state, &session, shell, result).await
}

#[derive(Debug, Deserialize)]
pub struct NoteForm {
    who: String,
    note: String,
}

/// `POST /blacklist/notes`
pub async fn add_note(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NoteForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, BLACKLIST_ADD_NOTES, "blacklist").await?;
    // Adding without seeing makes no sense: notes live on the page.
    session.require(&state, BLACKLIST_VIEW).await?;
    let result = blacklist::add_note(&state, session.account, &form.who, &form.note).await;
    done(&state, &session, shell, result).await
}

/// `POST /blacklist/notes/{id}/delete`
pub async fn delete_note(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, BLACKLIST_VIEW, "blacklist").await?;
    let manage = tether_db::permissions::effective(&state.db, session.account)
        .await?
        .contains(BLACKLIST_MANAGE);
    let result = blacklist::delete_note(&state, session.account, manage, id).await;
    done(&state, &session, shell, result).await
}
