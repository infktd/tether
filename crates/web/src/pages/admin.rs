//! Admin pages: groups and permissions (states are in `states`). Plain forms that work
//! without JavaScript (post, then redirect); htmx boosts them. Every action
//! goes through `crate::admin`, the same code as the JSON API.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_STATES, JoinPolicy};
use tether_db::accounts;
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::admin::{self, MembershipChange};
use crate::auth::CurrentSession;
use crate::error::AppError;

/// Signed in (else the login page) and holding `permission` (else 403).
pub(crate) async fn guard(
    state: &AppState,
    session: Option<CurrentSession>,
    permission: &str,
    active: &'static str,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(state, permission).await?;
    let loaded = load(state, &session, active).await?;
    Ok((session, loaded.shell))
}

fn policy_label(policy: JoinPolicy) -> &'static str {
    match policy {
        JoinPolicy::Open => "Open",
        JoinPolicy::Request => "Request to join",
        JoinPolicy::Assigned => "Assigned by admins",
    }
}

/// A state in a `<select>`.
pub struct StateOption {
    pub id: i64,
    pub name: String,
}

pub(crate) async fn state_options(state: &AppState) -> Result<Vec<StateOption>, AppError> {
    Ok(tether_db::states::list(&state.db)
        .await?
        .into_iter()
        .map(|s| StateOption {
            id: s.id.0,
            name: s.name,
        })
        .collect())
}

/// A state's name for display, from a list loaded once.
pub(crate) fn state_name(states: &[StateOption], id: tether_core::states::StateId) -> String {
    states
        .iter()
        .find(|s| s.id == id.0)
        .map_or_else(|| format!("state {}", id.0), |s| s.name.clone())
}

/// `GET /admin`: the first admin page this account may see.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Redirect, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let perms = permissions::effective(&state.db, session.account).await?;
    for (permission, page) in [
        (tether_core::permissions::ADMIN_SYSTEM, "/admin/system"),
        (tether_core::permissions::ADMIN_PLUGINS, "/admin/plugins"),
        (ADMIN_GROUPS, "/admin/groups"),
        (ADMIN_PERMISSIONS, "/admin/permissions"),
        (ADMIN_STATES, "/admin/states"),
        (tether_core::permissions::ADMIN_DISCORD, "/admin/discord"),
        (tether_core::permissions::ADMIN_AUDIT, "/admin/audit"),
    ] {
        if perms.contains(permission) {
            return Ok(Redirect::to(page));
        }
    }
    Err(AppError::forbidden().into())
}

// ---- groups ----------------------------------------------------------------

pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub policy: &'static str,
    pub policy_label: &'static str,
    pub members: i64,
    pub pending: i64,
}

#[derive(Template)]
#[template(path = "admin_groups.html")]
struct GroupsPage {
    shell: Shell,
    groups: Vec<GroupRow>,
    error: Option<String>,
    form: NewGroupForm,
}

#[derive(Debug, Default, Deserialize)]
pub struct NewGroupForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    join_policy: String,
}

async fn groups_page(
    state: &AppState,
    shell: Shell,
    form: NewGroupForm,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let groups = groups::summaries(&state.db)
        .await?
        .into_iter()
        .map(|g| GroupRow {
            id: g.group.id.0,
            name: g.group.name,
            description: g.group.description,
            policy: g.group.join_policy.as_str(),
            policy_label: policy_label(g.group.join_policy),
            members: g.members,
            pending: g.pending,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupsPage {
        shell,
        groups,
        error: error.map(|e| e.message().to_owned()),
        form,
    };
    Ok(render(status, &page))
}

/// `GET /admin/groups`
pub async fn groups(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    groups_page(&state, shell, NewGroupForm::default(), None).await
}

/// `POST /admin/groups`
pub async fn create_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NewGroupForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::create_group(
        &state,
        session.account,
        &form.name,
        &form.description,
        &form.join_policy,
    )
    .await
    {
        Ok(id) => Ok(Redirect::to(&format!("/admin/groups/{}", id.0)).into_response()),
        Err(err) => groups_page(&state, shell, form, Some(err)).await,
    }
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: String,
}

pub struct RequestRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
}

#[derive(Template)]
#[template(path = "admin_group.html")]
struct GroupPage {
    shell: Shell,
    group: GroupRow,
    members: Vec<MemberRow>,
    requests: Vec<RequestRow>,
    error: Option<String>,
}

async fn group_page(
    state: &AppState,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let group = groups::summaries(&state.db)
        .await?
        .into_iter()
        .find(|g| g.group.id.0 == id)
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let members = groups::members(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|m| MemberRow {
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
            state: m.state,
            state_style: m.state_style,
        })
        .collect();
    let requests = groups::requests(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|r| RequestRow {
            account_id: r.account_id,
            main_id: r.main_id,
            main_name: r.main_name,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupPage {
        shell,
        group: GroupRow {
            id: group.group.id.0,
            name: group.group.name,
            description: group.group.description,
            policy: group.group.join_policy.as_str(),
            policy_label: policy_label(group.group.join_policy),
            members: group.members,
            pending: group.pending,
        },
        members,
        requests,
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

/// `GET /admin/groups/{id}`
pub async fn group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    group_page(&state, shell, id, None).await
}

/// `POST /admin/groups/{id}/delete`
pub async fn delete_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::delete_group(&state, session.account, id).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct AddMemberForm {
    character: String,
}

/// `POST /admin/groups/{id}/members`: by character name or id.
pub async fn add_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<AddMemberForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    let result = match accounts::find(&state.db, &form.character).await? {
        Some(account) => {
            admin::change_membership(
                &state,
                session.account,
                id,
                account.0,
                MembershipChange::Add,
            )
            .await
        }
        None => Err(AppError::not_found(
            "No account has a character with that name.",
        )),
    };
    match result {
        Ok(()) => Ok(Redirect::to(&format!("/admin/groups/{id}")).into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

async fn membership(
    state: AppState,
    session: Option<CurrentSession>,
    id: i64,
    account_id: i64,
    change: MembershipChange,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::change_membership(&state, session.account, id, account_id, change).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/groups/{id}")).into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/groups/{id}/members/{account_id}/remove`
pub async fn remove_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Remove).await
}

/// `POST /admin/groups/{id}/requests/{account_id}/approve`
pub async fn approve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Approve).await
}

/// `POST /admin/groups/{id}/requests/{account_id}/deny`
pub async fn deny(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Deny).await
}

// ---- permissions -----------------------------------------------------------

pub struct GrantBadge {
    pub id: i64,
    pub label: String,
    /// `state` or `group`.
    pub kind: &'static str,
}

pub struct PermissionRow {
    pub name: String,
    pub description: String,
    pub grants: Vec<GrantBadge>,
}

pub struct GroupOption {
    pub id: i64,
    pub name: String,
}

#[derive(Template)]
#[template(path = "admin_permissions.html")]
struct PermissionsPage {
    shell: Shell,
    rows: Vec<PermissionRow>,
    states: Vec<StateOption>,
    groups: Vec<GroupOption>,
    error: Option<String>,
}

async fn permissions_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let grants = permissions::list(&state.db).await?;
    let states = state_options(state).await?;
    let all_groups = groups::summaries(&state.db).await?;
    let group_name = |id: GroupId| {
        all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone())
    };
    let rows = permissions::available(&state.db)
        .await?
        .into_iter()
        .map(|(name, description)| PermissionRow {
            grants: grants
                .iter()
                .filter(|g| g.permission == name)
                .map(|g| match g.grantee {
                    Grantee::State(id) => GrantBadge {
                        id: g.id,
                        label: state_name(&states, id),
                        kind: "state",
                    },
                    Grantee::Group(group) => GrantBadge {
                        id: g.id,
                        label: group_name(group),
                        kind: "group",
                    },
                })
                .collect(),
            name,
            description,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = PermissionsPage {
        shell,
        rows,
        states,
        groups: all_groups
            .iter()
            .map(|g| GroupOption {
                id: g.group.id.0,
                name: g.group.name.clone(),
            })
            .collect(),
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

/// `GET /admin/permissions`
pub async fn permissions(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    permissions_page(&state, shell, None).await
}

#[derive(Debug, Deserialize)]
pub struct GrantForm {
    permission: String,
    /// `state:<id>` or `group:<id>`.
    grantee: String,
}

/// A `<select>` value: `state:<id>` or `group:<id>`.
pub(crate) fn parse_grantee(value: &str) -> Result<Grantee, AppError> {
    let choose = || AppError::bad_request("Choose a state or a group.");
    match value.split_once(':') {
        Some(("state", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(Some(id), None)),
        Some(("group", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(None, Some(id))),
        _ => Err(choose()),
    }
}

/// `POST /admin/permissions/grant`
pub async fn grant(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<GrantForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    let result = match parse_grantee(&form.grantee) {
        Ok(grantee) => admin::grant(&state, session.account, &form.permission, grantee)
            .await
            .map(|_| ()),
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => Ok(Redirect::to("/admin/permissions").into_response()),
        Err(err) => permissions_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/permissions/{grant_id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(grant_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    match admin::revoke(&state, session.account, grant_id).await {
        Ok(()) => Ok(Redirect::to("/admin/permissions").into_response()),
        Err(err) => permissions_page(&state, shell, Some(err)).await,
    }
}
