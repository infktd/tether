//! Users (AA's admin site Users): find any account by any of its
//! characters, see its characters, state, groups and permissions, and
//! deactivate or reactivate it.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{ADMIN_GROUPS, ADMIN_USERS, PERMISSIONS_AUDIT};
use tether_db::accounts::AccountId;
use tether_db::audit::Actor;
use tether_db::users::{self as db, CharacterRow, Row, Status};

use super::admin::{StateOption, guard, state_options};
use super::{PageError, Shell, render};
use crate::AppState;
use crate::admin;
use crate::auth::CurrentSession;
use crate::error::AppError;

fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M EVE").to_string()
}

#[derive(Debug, Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
    /// A state id, or empty for all.
    #[serde(default)]
    state: String,
    /// `active`, `inactive`, or empty for all.
    #[serde(default)]
    status: String,
}

#[derive(Template)]
#[template(path = "admin_users.html")]
struct ListPage {
    shell: Shell,
    query: String,
    /// The state filter; 0 for all.
    state: i64,
    status: String,
    states: Vec<StateOption>,
    rows: Vec<Row>,
    total: i64,
}

/// `GET /admin/users`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(search): Query<Search>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    let query: String = search.q.trim().chars().take(100).collect();
    let state_id: Option<i64> = search.state.parse().ok();
    let status = match search.status.as_str() {
        "active" => Status::Active,
        "inactive" => Status::Inactive,
        _ => Status::All,
    };
    let (rows, total) = db::search(&state.db, &query, state_id, status).await?;
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell,
            query,
            state: state_id.unwrap_or(0),
            status: match status {
                Status::All => String::new(),
                Status::Active => "active".into(),
                Status::Inactive => "inactive".into(),
            },
            states: state_options(&state).await?,
            rows,
            total,
        },
    ))
}

pub struct CharacterView {
    pub row: CharacterRow,
    pub added: String,
    pub last_login: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_user.html")]
struct OnePage {
    shell: Shell,
    id: i64,
    main_id: i64,
    main_name: String,
    owner: bool,
    active: bool,
    joined: String,
    state: String,
    state_style: &'static str,
    characters: Vec<CharacterView>,
    groups: Vec<(i64, String)>,
    permissions: Vec<String>,
    /// Whether the viewer may open groups and the Permissions Audit.
    link_groups: bool,
    link_audit: bool,
    /// The Pilot Log on its characters, for those who may read it.
    notes: Option<Vec<NoteView>>,
    error: Option<String>,
}

pub struct NoteView {
    pub name: String,
    pub note: String,
    pub by: String,
    pub at: String,
}

async fn user_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let account = AccountId(id);
    let found = tether_db::accounts::get(&state.db, account)
        .await?
        .ok_or_else(|| AppError::not_found("No such account."))?;
    let access = tether_db::states::account_state(&state.db, account)
        .await?
        .ok_or_else(|| AppError::not_found("No such account."))?;
    let joined = db::created_at(&state.db, account)
        .await?
        .map(when)
        .unwrap_or_default();
    let characters: Vec<CharacterView> = db::characters(&state.db, account)
        .await?
        .into_iter()
        .map(|row| CharacterView {
            added: when(row.added_at),
            last_login: row.last_login_at.map(when),
            row,
        })
        .collect();
    let viewer = tether_db::permissions::effective(&state.db, session.account).await?;
    let notes = if viewer.contains(tether_core::permissions::BLACKLIST_VIEW) {
        let ids: Vec<i64> = characters
            .iter()
            .map(|c: &CharacterView| c.row.id)
            .collect();
        Some(
            tether_db::blacklist::notes(&state.db, Some(&ids), None, 100)
                .await?
                .into_iter()
                .map(|n| NoteView {
                    at: when(n.added_at),
                    name: n.name,
                    note: n.note,
                    by: n.added_by_name,
                })
                .collect(),
        )
    } else {
        None
    };
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &OnePage {
            shell,
            id,
            main_id: found.main.as_ref().map_or(0, |m| m.id),
            main_name: found
                .main
                .map_or_else(|| "(no main)".to_owned(), |m| m.name),
            owner: found.is_owner,
            active: found.active,
            joined,
            state_style: access.style(),
            state: access.name,
            characters,
            groups: db::groups(&state.db, account).await?,
            permissions: tether_db::permissions::effective(&state.db, account)
                .await?
                .into_iter()
                .collect(),
            link_groups: viewer.contains(ADMIN_GROUPS),
            link_audit: viewer.contains(PERMISSIONS_AUDIT),
            notes,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/users/{id}`
pub async fn show(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    user_page(&state, &session, shell, id, None).await
}

async fn set_active(
    state: AppState,
    session: Option<CurrentSession>,
    id: i64,
    active: bool,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    match admin::set_active(
        &state.db,
        Actor::Account(session.account),
        AccountId(id),
        active,
    )
    .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/admin/users/{id}")).into_response()),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// `POST /admin/users/{id}/deactivate`
pub async fn deactivate(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    set_active(state, session, id, false).await
}

/// `POST /admin/users/{id}/reactivate`
pub async fn reactivate(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    set_active(state, session, id, true).await
}
