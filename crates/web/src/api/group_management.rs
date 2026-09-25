//! Group Management (AA's): requests, members and the Audit Log of the
//! groups you manage (all non-Internal groups with `group_management`, or
//! the ones you lead).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tether_db::accounts::AccountId;
use tether_db::groups::{self as group_db, GroupId};

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{self, Decision};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RequestOut {
    pub group_id: i64,
    pub group_name: String,
    pub account_id: i64,
    pub main_name: String,
    /// True for a request to leave.
    pub leave: bool,
    pub requested_at: DateTime<Utc>,
}

/// `GET /api/group-management/requests`: pending requests for the groups
/// you manage, oldest first.
#[utoipa::path(get, path = "/api/group-management/requests", tag = "group management", security(("session" = [])),
    responses((status = 200, body = Vec<RequestOut>)))]
pub async fn requests(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<RequestOut>>, AppError> {
    let managed = groups::managed_by(&state.db, session.account).await?;
    let requests = group_db::requests_for(&state.db, &managed).await?;
    Ok(Json(
        requests
            .into_iter()
            .map(|r| RequestOut {
                group_id: r.group_id,
                group_name: r.group_name,
                account_id: r.account_id,
                main_name: r.main_name,
                leave: r.leave,
                requested_at: r.requested_at,
            })
            .collect(),
    ))
}

async fn decide(
    state: &AppState,
    session: &CurrentSession,
    (id, account_id): (i64, i64),
    decision: Decision,
) -> Result<StatusCode, AppError> {
    groups::decide(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
        decision,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/group-management/groups/{id}/requests/{account_id}/accept`
#[utoipa::path(post, path = "/api/group-management/groups/{id}/requests/{account_id}/accept", tag = "group management", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)),
    responses((status = 204), (status = 400, description = "Their state can't be in the group now"),
              (status = 403), (status = 404)))]
pub async fn accept(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(ids): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    decide(&state, &session, ids, Decision::Accept).await
}

/// `POST /api/group-management/groups/{id}/requests/{account_id}/reject`
#[utoipa::path(post, path = "/api/group-management/groups/{id}/requests/{account_id}/reject", tag = "group management", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)),
    responses((status = 204), (status = 404)))]
pub async fn reject(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(ids): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    decide(&state, &session, ids, Decision::Reject).await
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MemberOut {
    pub account_id: i64,
    pub main_name: String,
    pub state: String,
}

/// `GET /api/group-management/groups/{id}/members`
#[utoipa::path(get, path = "/api/group-management/groups/{id}/members", tag = "group management", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 200, body = Vec<MemberOut>), (status = 404)))]
pub async fn members(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Json<Vec<MemberOut>>, AppError> {
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    Ok(Json(
        group_db::members(&state.db, group.id)
            .await?
            .into_iter()
            .map(|m| MemberOut {
                account_id: m.account_id,
                main_name: m.main_name,
                state: m.state,
            })
            .collect(),
    ))
}

/// `DELETE /api/group-management/groups/{id}/members/{account_id}`:
/// logged as Removed.
#[utoipa::path(delete, path = "/api/group-management/groups/{id}/members/{account_id}", tag = "group management", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)),
    responses((status = 204), (status = 403), (status = 404)))]
pub async fn remove_member(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    groups::kick(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LogOut {
    pub at: DateTime<Utc>,
    /// `join`, `leave` or `removed`.
    pub request_type: String,
    /// `accept` or `reject`.
    pub action: String,
    pub requestor_main: Option<String>,
    pub requestor_corporation: Option<String>,
    pub actor_name: Option<String>,
}

/// `GET /api/group-management/groups/{id}/audit-log`: the latest 200.
#[utoipa::path(get, path = "/api/group-management/groups/{id}/audit-log", tag = "group management", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 200, body = Vec<LogOut>), (status = 404)))]
pub async fn audit_log(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Json<Vec<LogOut>>, AppError> {
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    Ok(Json(
        group_db::audit_log(&state.db, group.id, 200)
            .await?
            .into_iter()
            .map(|e| LogOut {
                at: e.at,
                request_type: e.request_type,
                action: e.action,
                requestor_main: e.requestor_main,
                requestor_corporation: e.requestor_corporation,
                actor_name: e.actor_name,
            })
            .collect(),
    ))
}
