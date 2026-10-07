//! Users (AA's admin site Users): find any account by any of its
//! characters, see its characters, state, groups and permissions,
//! deactivate or reactivate it, sign it out everywhere, grant it permissions of its own (AA's user
//! permissions, for `admin.permissions` holders) and, for superusers, make
//! it a superuser or not.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_core::permissions::{ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_USERS, PERMISSIONS_AUDIT};
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
    /// Granted to this account itself: `(grant id, permission)`.
    own_grants: Vec<(i64, String)>,
    /// The viewer may grant it permissions (`admin.permissions`): only
    /// those the viewer holds, listed here.
    grantable: Option<Vec<String>>,
    /// The viewer is a superuser (makes and unmakes superusers).
    superuser_controls: bool,
    /// Whether the viewer may open groups and the Permissions Audit.
    link_groups: bool,
    link_audit: bool,
    /// The Pilot Log on its characters, for those who may read it.
    notes: Option<Vec<NoteView>>,
    /// Its live sessions (browsers signed in), for Sign out everywhere.
    sessions: usize,
    /// The viewer's own account: signed out from Sessions, not here.
    viewer_is_target: bool,
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
    // The Pilot Log's notes on its characters, as the viewer may see them
    // there.
    let reader = crate::blacklist::access(state, session.account).await?;
    let notes = if reader.notes() {
        let ids: Vec<i64> = characters
            .iter()
            .map(|c: &CharacterView| c.row.id)
            .collect();
        Some(
            tether_db::blacklist::notes(
                &state.db,
                &reader.reader,
                tether_db::blacklist::Filter {
                    about: Some(&ids),
                    ..Default::default()
                },
                100,
            )
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
    let own_grants = tether_db::permissions::of_account(&state.db, account).await?;
    let grantable = viewer.contains(ADMIN_PERMISSIONS).then(|| {
        viewer
            .iter()
            .filter(|p| !own_grants.iter().any(|(_, g)| g == *p))
            .cloned()
            .collect()
    });
    let superuser_controls = tether_db::accounts::standing(&state.db, session.account)
        .await?
        .is_some_and(|s| s.is_owner);
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
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
                own_grants,
                grantable,
                superuser_controls,
                link_groups: viewer.contains(ADMIN_GROUPS),
                link_audit: viewer.contains(PERMISSIONS_AUDIT),
                notes,
                sessions: tether_db::auth::sessions_of(&state.db, account)
                    .await?
                    .len(),
                viewer_is_target: session.account == account,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
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
        Ok(()) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            if active {
                "Account reactivated."
            } else {
                "Account deactivated."
            },
        )),
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

/// `POST /admin/users/{id}/characters/{character}/remove`: a character off
/// the account at once (not its main), sudo mode.
pub async fn remove_character(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    match crate::ownership::remove_character(&state, session.account, AccountId(id), character)
        .await
    {
        Ok(name) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            format!("{name} was removed from the account."),
        )),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// `POST /admin/users/{id}/reactivate`
pub async fn reactivate(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    set_active(state, session, id, true).await
}

/// `POST /admin/users/{id}/sign-out`: end every session of the account
/// (sudo mode, audited; only an account whose permissions are all the
/// admin's, as deactivating).
pub async fn sign_out(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    match crate::sessions::sign_out_user(&state, session.account, AccountId(id)).await {
        Ok(ended) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            format!("Signed out of {}.", crate::sessions::sessions(ended)),
        )),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// `POST /admin/users/{id}/superuser`: make it a superuser (superusers
/// only, sudo mode).
pub async fn make_superuser(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    set_superuser(state, session, id, true).await
}

/// `POST /admin/users/{id}/superuser/revoke`
pub async fn revoke_superuser(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    set_superuser(state, session, id, false).await
}

async fn set_superuser(
    state: AppState,
    session: Option<CurrentSession>,
    id: i64,
    on: bool,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    match admin::set_superuser(&state.db, session.account, AccountId(id), on).await {
        Ok(()) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            if on {
                "Made a superuser."
            } else {
                "No longer a superuser."
            },
        )),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct GrantForm {
    permission: String,
}

/// `POST /admin/users/{id}/permissions`: grant this user a permission of
/// its own (AA's user permissions). Only one the admin holds.
pub async fn grant(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<GrantForm>,
) -> Result<Response, PageError> {
    // The answer is the user's page: Users is needed as well.
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    session.require(&state, ADMIN_PERMISSIONS).await?;
    let grantee = tether_db::permissions::Grantee::Account(AccountId(id));
    match admin::grant(&state, session.account, &form.permission, grantee).await {
        Ok(_) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            format!("Granted {}.", form.permission),
        )),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// `POST /admin/users/{id}/permissions/{grant_id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, grant_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_USERS, "users").await?;
    session.require(&state, ADMIN_PERMISSIONS).await?;
    let own = tether_db::permissions::of_account(&state.db, AccountId(id)).await?;
    let result = if own.iter().any(|(g, _)| *g == grant_id) {
        admin::revoke(&state, session.account, grant_id).await
    } else {
        Err(AppError::not_found("No such grant on this account."))
    };
    match result {
        Ok(()) => Ok(super::stay::back(
            &format!("/admin/users/{id}"),
            "Permission revoked.",
        )),
        Err(err) => user_page(&state, &session, shell, id, Some(err)).await,
    }
}
