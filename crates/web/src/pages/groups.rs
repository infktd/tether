//! The users' Groups page (AA's Available Groups, and the direct join
//! link) and Group Management (Group Requests, Group Membership and each
//! group's Audit Log). Every action goes through `crate::groups`, the same
//! code as the JSON API.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use tether_db::accounts::AccountId;
use tether_db::groups::{self as group_db, Group, GroupId};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{self, Decision, Joined, Left};

/// A group's badge: Internal, Open or Requestable (AA's labels).
pub(crate) fn label(group: &Group) -> &'static str {
    group.flags.label()
}

pub struct GroupCard {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub internal: bool,
    pub is_member: bool,
    /// `join`, `leave`, or empty.
    pub pending: &'static str,
}

fn card(a: groups::Available) -> GroupCard {
    GroupCard {
        id: a.group.id.0,
        label: label(&a.group),
        internal: a.group.flags.internal,
        name: a.group.name,
        description: a.group.description,
        is_member: a.is_member,
        pending: match a.pending {
            Some(true) => "leave",
            Some(false) => "join",
            None => "",
        },
    }
}

#[derive(Template)]
#[template(path = "groups.html")]
struct GroupsPage {
    shell: Shell,
    mine: Vec<GroupCard>,
    available: Vec<GroupCard>,
    notice: Option<String>,
    error: Option<String>,
}

async fn groups_page(
    state: &AppState,
    session: &CurrentSession,
    notice: Option<String>,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let loaded = load(state, session, "groups").await?;
    let (mine, available) = groups::available(&state.db, session.account)
        .await?
        .into_iter()
        .map(card)
        .partition(|g| g.is_member);
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &GroupsPage {
            shell: loaded.shell,
            mine,
            available,
            notice,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /groups`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    groups_page(&state, &session, None, None).await
}

#[derive(Template)]
#[template(path = "group_join.html")]
struct JoinPage {
    shell: Shell,
    group: GroupCard,
    error: Option<String>,
}

/// `GET /groups/{id}`: the direct join link (works for Hidden groups).
pub async fn direct(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let group = groups::direct(&state.db, session.account, GroupId(id)).await?;
    let loaded = load(&state, &session, "groups").await?;
    Ok(render(
        StatusCode::OK,
        &JoinPage {
            shell: loaded.shell,
            group: card(group),
            error: None,
        },
    ))
}

async fn after(
    state: &AppState,
    session: &CurrentSession,
    result: Result<&'static str, AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(notice) => groups_page(state, session, Some(notice.to_owned()), None).await,
        Err(err) => groups_page(state, session, None, Some(err)).await,
    }
}

/// `POST /groups/{id}/join`
pub async fn join(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::join(&state.db, session.account, GroupId(id))
        .await
        .map(|joined| match joined {
            Joined::Added => "You joined the group.",
            Joined::Requested => "Request sent: the group's leaders will decide.",
        });
    after(&state, &session, result).await
}

/// `POST /groups/{id}/leave`
pub async fn leave(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::leave(&state.db, session.account, GroupId(id))
        .await
        .map(|left| match left {
            Left::Removed => "You left the group.",
            Left::Requested => "Leave request sent: the group's leaders will decide.",
        });
    after(&state, &session, result).await
}

/// `POST /groups/{id}/retract`
pub async fn retract(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::retract(&state.db, session.account, GroupId(id))
        .await
        .map(|()| "Request withdrawn.");
    after(&state, &session, result).await
}

// ---- Group Management ------------------------------------------------------

/// Signed in and managing at least one group (else 403).
async fn manager(
    state: &AppState,
    session: Option<CurrentSession>,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let loaded = load(state, &session, "group_management").await?;
    if loaded.shell.group_management.is_none() {
        return Err(AppError::forbidden().into());
    }
    Ok((session, loaded.shell))
}

pub struct RequestRow {
    pub group_id: i64,
    pub group_name: String,
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub requested_at: String,
}

#[derive(Template)]
#[template(path = "group_management.html")]
struct RequestsPage {
    shell: Shell,
    joins: Vec<RequestRow>,
    leaves: Vec<RequestRow>,
    error: Option<String>,
}

async fn requests_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let managed = groups::managed_by(&state.db, session.account).await?;
    let (leaves, joins): (Vec<_>, Vec<_>) = group_db::requests_for(&state.db, &managed)
        .await?
        .into_iter()
        .partition(|r| r.leave);
    let row = |r: group_db::PendingRequest| RequestRow {
        group_id: r.group_id,
        group_name: r.group_name,
        account_id: r.account_id,
        main_id: r.main_id,
        main_name: r.main_name,
        requested_at: r.requested_at.format("%Y-%m-%d %H:%M").to_string(),
    };
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &RequestsPage {
            shell,
            joins: joins.into_iter().map(row).collect(),
            leaves: leaves.into_iter().map(row).collect(),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /group-management`: Group Requests.
pub async fn requests(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    requests_page(&state, &session, shell, None).await
}

async fn decide(
    state: AppState,
    session: Option<CurrentSession>,
    (id, account_id): (i64, i64),
    decision: Decision,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    match groups::decide(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
        decision,
    )
    .await
    {
        Ok(()) => Ok(Redirect::to("/group-management").into_response()),
        Err(err) => requests_page(&state, &session, shell, Some(err)).await,
    }
}

/// `POST /group-management/{id}/requests/{account_id}/accept`
pub async fn accept(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(ids): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    decide(state, session, ids, Decision::Accept).await
}

/// `POST /group-management/{id}/requests/{account_id}/reject`
pub async fn reject(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(ids): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    decide(state, session, ids, Decision::Reject).await
}

pub struct ManagedRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub members: i64,
    pub pending: i64,
}

#[derive(Template)]
#[template(path = "group_membership.html")]
struct MembershipPage {
    shell: Shell,
    groups: Vec<ManagedRow>,
}

/// `GET /group-management/membership`: Group Membership.
pub async fn membership(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    let managed = groups::managed_by(&state.db, session.account).await?;
    let groups = group_db::summaries(&state.db)
        .await?
        .into_iter()
        .filter(|g| managed.contains(&g.group.id))
        .map(|g| ManagedRow {
            id: g.group.id.0,
            label: label(&g.group),
            name: g.group.name,
            description: g.group.description,
            members: g.members,
            pending: g.pending,
        })
        .collect();
    Ok(render(StatusCode::OK, &MembershipPage { shell, groups }))
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: String,
}

#[derive(Template)]
#[template(path = "group_members.html")]
struct MembersPage {
    shell: Shell,
    id: i64,
    name: String,
    label: &'static str,
    restricted: bool,
    join_link: String,
    members: Vec<MemberRow>,
    error: Option<String>,
}

async fn members_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    let members = group_db::members(&state.db, group.id)
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
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &MembersPage {
            shell,
            id,
            label: label(&group),
            restricted: group.flags.restricted,
            join_link: format!("{}/groups/{id}", state.site.origin()),
            name: group.name,
            members,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /group-management/{id}`: a group's members (View Members) and its
/// direct join link.
pub async fn members(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    members_page(&state, &session, shell, id, None).await
}

/// `POST /group-management/{id}/members/{account_id}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    match groups::kick(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
    )
    .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/group-management/{id}")).into_response()),
        Err(err) => members_page(&state, &session, shell, id, Some(err)).await,
    }
}

pub struct LogRow {
    pub at: String,
    pub requestor: String,
    pub corporation: String,
    /// Join, Leave or Removed.
    pub kind: &'static str,
    /// Accept or Reject.
    pub action: &'static str,
    pub accepted: bool,
    pub actor: String,
}

#[derive(Template)]
#[template(path = "group_audit.html")]
struct AuditPage {
    shell: Shell,
    id: i64,
    name: String,
    entries: Vec<LogRow>,
}

/// `GET /group-management/{id}/audit`: the group's Audit Log.
pub async fn audit(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    let entries = group_db::audit_log(&state.db, group.id, 200)
        .await?
        .into_iter()
        .map(|e| LogRow {
            at: e.at.format("%Y-%m-%d %H:%M").to_string(),
            requestor: e.requestor_main.unwrap_or_else(|| "(no main)".to_owned()),
            corporation: e.requestor_corporation.unwrap_or_default(),
            kind: match e.request_type.as_str() {
                "join" => "Join",
                "leave" => "Leave",
                _ => "Removed",
            },
            accepted: e.action == "accept",
            action: if e.action == "accept" {
                "Accept"
            } else {
                "Reject"
            },
            actor: e.actor_name.unwrap_or_default(),
        })
        .collect();
    Ok(render(
        StatusCode::OK,
        &AuditPage {
            shell,
            id,
            name: group.name,
            entries,
        },
    ))
}
