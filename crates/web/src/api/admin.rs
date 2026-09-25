//! Admin API: groups, permissions, states, audit log. Every change is audited in
//! the same transaction.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tether_core::permissions::{ADMIN_AUDIT, ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_STATES};
use tether_core::states::StateId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};
use tether_db::states as state_db;

use crate::AppState;
use crate::admin::{self, MembershipChange};
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::state_admin::{self, Change};

// ---- groups ---------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct NewGroup {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub join_policy: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Created {
    pub id: i64,
}

/// `POST /api/admin/groups`
#[utoipa::path(post, path = "/api/admin/groups", tag = "admin", security(("session" = [])), request_body = NewGroup,
    responses((status = 201, body = Created), (status = 400), (status = 403), (status = 409, description = "Name taken")))]
pub async fn create_group(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<NewGroup>,
) -> Result<(StatusCode, Json<Created>), AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    let id = admin::create_group(
        &state,
        session.account,
        &body.name,
        &body.description,
        &body.join_policy,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(Created { id: id.0 })))
}

/// `DELETE /api/admin/groups/{id}`
#[utoipa::path(delete, path = "/api/admin/groups/{id}", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn delete_group(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    admin::delete_group(&state, session.account, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MemberIn {
    pub account_id: i64,
}

/// `POST /api/admin/groups/{id}/members`
#[utoipa::path(post, path = "/api/admin/groups/{id}/members", tag = "admin", security(("session" = [])), request_body = MemberIn,
    params(("id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn add_member(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
    Json(body): Json<MemberIn>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    admin::change_membership(
        &state,
        session.account,
        id,
        body.account_id,
        MembershipChange::Add,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/admin/groups/{id}/members/{account_id}`
#[utoipa::path(delete, path = "/api/admin/groups/{id}/members/{account_id}", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn remove_member(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    admin::change_membership(
        &state,
        session.account,
        id,
        account_id,
        MembershipChange::Remove,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/admin/groups/{id}/requests/{account_id}/approve`
#[utoipa::path(post, path = "/api/admin/groups/{id}/requests/{account_id}/approve", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn approve_request(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    admin::change_membership(
        &state,
        session.account,
        id,
        account_id,
        MembershipChange::Approve,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/admin/groups/{id}/requests/{account_id}/deny`
#[utoipa::path(post, path = "/api/admin/groups/{id}/requests/{account_id}/deny", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path), ("account_id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn deny_request(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    admin::change_membership(
        &state,
        session.account,
        id,
        account_id,
        MembershipChange::Deny,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RequestOut {
    pub account_id: i64,
    pub main_name: String,
}

/// `GET /api/admin/groups/{id}/requests`
#[utoipa::path(get, path = "/api/admin/groups/{id}/requests", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 200, body = Vec<RequestOut>), (status = 403)))]
pub async fn list_requests(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<Json<Vec<RequestOut>>, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    let requests = groups::requests(&state.db, GroupId(id)).await?;
    Ok(Json(
        requests
            .into_iter()
            .map(|r| RequestOut {
                account_id: r.account_id,
                main_name: r.main_name,
            })
            .collect(),
    ))
}

// ---- permissions ----------------------------------------------------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PermissionsOut {
    pub available: Vec<PermissionInfo>,
    pub grants: Vec<GrantOut>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PermissionInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct GrantOut {
    pub id: i64,
    pub permission: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<i64>,
}

/// `GET /api/admin/permissions`
#[utoipa::path(get, path = "/api/admin/permissions", tag = "admin", security(("session" = [])),
    responses((status = 200, body = PermissionsOut), (status = 403)))]
pub async fn list_permissions(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<PermissionsOut>, AppError> {
    session.require(&state, ADMIN_PERMISSIONS).await?;
    let grants = permissions::list(&state.db).await?;
    Ok(Json(PermissionsOut {
        available: permissions::available(&state.db)
            .await?
            .into_iter()
            .map(|(name, description)| PermissionInfo { name, description })
            .collect(),
        grants: grants
            .into_iter()
            .map(|g| {
                let (state_id, group_id) = match g.grantee {
                    Grantee::State(s) => (Some(s.0), None),
                    Grantee::Group(group) => (None, Some(group.0)),
                };
                GrantOut {
                    id: g.id,
                    permission: g.permission,
                    state_id,
                    group_id,
                }
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantIn {
    pub permission: String,
    pub state_id: Option<i64>,
    pub group_id: Option<i64>,
}

/// `POST /api/admin/permissions/grants`: grant to a state or a group.
#[utoipa::path(post, path = "/api/admin/permissions/grants", tag = "admin", security(("session" = [])), request_body = GrantIn,
    responses((status = 201, body = Created), (status = 400), (status = 403), (status = 404), (status = 409)))]
pub async fn grant(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<GrantIn>,
) -> Result<(StatusCode, Json<Created>), AppError> {
    session.require(&state, ADMIN_PERMISSIONS).await?;
    let grantee = admin::grantee(body.state_id, body.group_id)?;
    let id = admin::grant(&state, session.account, &body.permission, grantee).await?;
    Ok((StatusCode::CREATED, Json(Created { id })))
}

/// `DELETE /api/admin/permissions/grants/{id}`
#[utoipa::path(delete, path = "/api/admin/permissions/grants/{id}", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn revoke(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_PERMISSIONS).await?;
    admin::revoke(&state, session.account, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- audit log ------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    pub before: Option<i64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AuditEntryOut {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_account_id: Option<i64>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    #[schema(value_type = Object)]
    pub details: Value,
}

/// `GET /api/admin/audit?limit=&before=`: newest first.
#[utoipa::path(get, path = "/api/admin/audit", tag = "admin", security(("session" = [])), params(AuditQuery),
    responses((status = 200, body = Vec<AuditEntryOut>), (status = 403)))]
pub async fn audit_log(
    State(state): State<AppState>,
    session: CurrentSession,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntryOut>>, AppError> {
    session.require(&state, ADMIN_AUDIT).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let entries = audit::list(&state.db, limit, query.before).await?;
    Ok(Json(
        entries
            .into_iter()
            .map(|e| AuditEntryOut {
                id: e.id,
                at: e.at,
                actor_account_id: e.actor_account_id,
                actor_name: e.actor_name,
                action: e.action,
                target: e.target,
                details: e.details,
            })
            .collect(),
    ))
}

// ---- states ---------------------------------------------------------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StateOut {
    pub id: i64,
    pub name: String,
    /// `member`, `blue` or `guest` for the built-in states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<&'static str>,
    /// Higher wins; Guest is 0.
    pub priority: i32,
    /// Accounts in the state now.
    pub accounts: i64,
    pub covers: Vec<CoveredOut>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CoveredOut {
    pub entity_id: i64,
    /// `alliance`, `corporation` or `character`.
    pub kind: &'static str,
    pub name: String,
}

/// `GET /api/admin/states`: highest priority first.
#[utoipa::path(get, path = "/api/admin/states", tag = "admin", security(("session" = [])),
    responses((status = 200, body = Vec<StateOut>), (status = 403)))]
pub async fn list_states(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<StateOut>>, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let states = state_db::list(&state.db).await?;
    let covered = state_db::covered(&state.db).await?;
    let counts = state_db::counts(&state.db).await?;
    Ok(Json(
        states
            .into_iter()
            .map(|s| StateOut {
                id: s.id.0,
                builtin: s.builtin.map(tether_core::states::Builtin::as_str),
                priority: s.priority,
                accounts: counts.get(&s.id).copied().unwrap_or(0),
                covers: covered
                    .iter()
                    .filter(|c| c.state == s.id)
                    .map(|c| CoveredOut {
                        entity_id: c.entity_id,
                        kind: c.kind.as_str(),
                        name: c.name.clone(),
                    })
                    .collect(),
                name: s.name,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct StateNameIn {
    pub name: String,
}

/// `POST /api/admin/states`: a new state, just above Guest.
#[utoipa::path(post, path = "/api/admin/states", tag = "admin", security(("session" = [])), request_body = StateNameIn,
    responses((status = 201, body = Created), (status = 400), (status = 403), (status = 409, description = "Name taken")))]
pub async fn create_state(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<StateNameIn>,
) -> Result<(StatusCode, Json<Created>), AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let change = Change::Create { name: body.name };
    let id = state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?
    .ok_or_else(|| AppError::internal("no id for a new state"))?;
    Ok((StatusCode::CREATED, Json(Created { id: id.0 })))
}

/// `PATCH /api/admin/states/{id}`: rename a state an admin made.
#[utoipa::path(patch, path = "/api/admin/states/{id}", tag = "admin", security(("session" = [])), request_body = StateNameIn,
    params(("id" = i64, Path)), responses((status = 204), (status = 400), (status = 403), (status = 404), (status = 409)))]
pub async fn rename_state(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
    Json(body): Json<StateNameIn>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let change = Change::Rename {
        state: StateId(id),
        name: body.name,
    };
    state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/admin/states/{id}`: delete a state an admin made; its
/// accounts are re-evaluated.
#[utoipa::path(delete, path = "/api/admin/states/{id}", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path)), responses((status = 204), (status = 400), (status = 403), (status = 404)))]
pub async fn delete_state(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let change = Change::Delete { state: StateId(id) };
    state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MoveIn {
    /// `up` (higher priority) or `down`.
    pub direction: String,
    /// The state expected to be passed; refused with 409 if the order
    /// changed.
    pub past: Option<i64>,
}

/// `POST /api/admin/states/{id}/move`: swap with the state above or below.
/// Changing who is in a state needs every permission granted to it.
#[utoipa::path(post, path = "/api/admin/states/{id}/move", tag = "admin", security(("session" = [])), request_body = MoveIn,
    params(("id" = i64, Path)), responses((status = 204), (status = 400), (status = 403), (status = 404)))]
pub async fn move_state(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
    Json(body): Json<MoveIn>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let up = match body.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => return Err(AppError::bad_request("direction must be up or down.")),
    };
    let change = Change::Move {
        state: StateId(id),
        up,
        past: body.past.map(StateId),
    };
    state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CoverIn {
    pub entity_id: i64,
}

/// `POST /api/admin/states/{id}/covers`: add an alliance, corporation or
/// character. Its name and kind come from ESI, not the client.
#[utoipa::path(post, path = "/api/admin/states/{id}/covers", tag = "admin", security(("session" = [])), request_body = CoverIn,
    params(("id" = i64, Path)),
    responses((status = 204), (status = 400), (status = 403), (status = 404, description = "No such state, or ESI doesn't know the id"), (status = 409), (status = 502, description = "ESI unavailable")))]
pub async fn add_cover(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
    Json(body): Json<CoverIn>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let change = Change::Add {
        state: StateId(id),
        entity_id: body.entity_id,
    };
    state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/admin/states/{id}/covers/{entity_id}`
#[utoipa::path(delete, path = "/api/admin/states/{id}/covers/{entity_id}", tag = "admin", security(("session" = [])),
    params(("id" = i64, Path), ("entity_id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn remove_cover(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, entity_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let change = Change::Remove {
        state: StateId(id),
        entity_id,
    };
    state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ResolveIn {
    pub names: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ResolveOut {
    pub alliances: Vec<EntityOut>,
    pub corporations: Vec<EntityOut>,
    pub characters: Vec<EntityOut>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EntityOut {
    pub id: i64,
    pub name: String,
}

/// `POST /api/admin/states/resolve`: exact alliance, corporation and
/// character names to ids, via ESI.
#[utoipa::path(post, path = "/api/admin/states/resolve", tag = "admin", security(("session" = [])), request_body = ResolveIn,
    responses((status = 200, body = ResolveOut), (status = 400), (status = 403), (status = 502)))]
pub async fn resolve_names(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<ResolveIn>,
) -> Result<Json<ResolveOut>, AppError> {
    session.require(&state, ADMIN_STATES).await?;
    let names: Vec<String> = body
        .names
        .iter()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() || names.len() > 50 {
        return Err(AppError::bad_request("Send 1 to 50 names."));
    }
    let resolved = state
        .esi
        .resolve_names(&names, tether_esi::Priority::Interactive)
        .await
        .map_err(admin::esi_unavailable)?;
    let out = |v: Vec<tether_esi::Entity>| {
        v.into_iter()
            .map(|e| EntityOut {
                id: e.id,
                name: e.name,
            })
            .collect()
    };
    Ok(Json(ResolveOut {
        alliances: out(resolved.alliances),
        corporations: out(resolved.corporations),
        characters: out(resolved.characters),
    }))
}
