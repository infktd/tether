//! Admin API: groups, permissions, audit log. Every change is audited in
//! the same transaction.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tether_core::permissions::{ADMIN_AUDIT, ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_TIERS};
use tether_core::tiers::Tier;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};
use tether_db::tiers as tier_db;

use crate::AppState;
use crate::admin::{self, MembershipChange};
use crate::auth::CurrentSession;
use crate::error::AppError;

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
    pub tier: Option<&'static str>,
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
                let (tier, group_id) = match g.grantee {
                    Grantee::Tier(t) => (Some(t.as_str()), None),
                    Grantee::Group(group) => (None, Some(group.0)),
                };
                GrantOut {
                    id: g.id,
                    permission: g.permission,
                    tier,
                    group_id,
                }
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantIn {
    pub permission: String,
    pub tier: Option<String>,
    pub group_id: Option<i64>,
}

/// `POST /api/admin/permissions/grants`: grant to a tier or a group.
#[utoipa::path(post, path = "/api/admin/permissions/grants", tag = "admin", security(("session" = [])), request_body = GrantIn,
    responses((status = 201, body = Created), (status = 400), (status = 403), (status = 404), (status = 409)))]
pub async fn grant(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<GrantIn>,
) -> Result<(StatusCode, Json<Created>), AppError> {
    session.require(&state, ADMIN_PERMISSIONS).await?;
    let grantee = admin::grantee(body.tier.as_deref(), body.group_id)?;
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

// ---- tier rules ------------------------------------------------------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TierRuleOut {
    pub entity_id: i64,
    pub kind: &'static str,
    pub tier: &'static str,
    pub name: String,
}

/// `GET /api/admin/tiers`
#[utoipa::path(get, path = "/api/admin/tiers", tag = "admin", security(("session" = [])),
    responses((status = 200, body = Vec<TierRuleOut>), (status = 403)))]
pub async fn list_tier_rules(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<TierRuleOut>>, AppError> {
    session.require(&state, ADMIN_TIERS).await?;
    let rules = tier_db::list_rules(&state.db).await?;
    Ok(Json(
        rules
            .into_iter()
            .map(|r| TierRuleOut {
                entity_id: r.entity_id,
                kind: r.kind.as_str(),
                tier: r.tier.as_str(),
                name: r.name,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TierRuleIn {
    pub entity_id: i64,
    pub tier: String,
}

/// `POST /api/admin/tiers`: make an alliance or corporation Member or
/// Allied. Its name and kind come from ESI, not the client.
#[utoipa::path(post, path = "/api/admin/tiers", tag = "admin", security(("session" = [])), request_body = TierRuleIn,
    responses((status = 204), (status = 400), (status = 403), (status = 404, description = "ESI doesn't know the id"), (status = 502, description = "ESI unavailable")))]
pub async fn set_tier_rule(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<TierRuleIn>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_TIERS).await?;
    let tier = Tier::parse(&body.tier)
        .ok_or_else(|| AppError::bad_request("tier must be member or allied."))?;
    admin::apply_tier_rule(
        &state,
        Actor::Account(session.account),
        body.entity_id,
        tier,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/admin/tiers/{entity_id}`
#[utoipa::path(delete, path = "/api/admin/tiers/{entity_id}", tag = "admin", security(("session" = [])),
    params(("entity_id" = i64, Path)), responses((status = 204), (status = 403), (status = 404)))]
pub async fn remove_tier_rule(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(entity_id): Path<i64>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_TIERS).await?;
    admin::remove_tier_rule(&state, session.account, entity_id).await?;
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
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EntityOut {
    pub id: i64,
    pub name: String,
}

/// `POST /api/admin/tiers/resolve`: exact alliance and corporation names to
/// ids, via ESI.
#[utoipa::path(post, path = "/api/admin/tiers/resolve", tag = "admin", security(("session" = [])), request_body = ResolveIn,
    responses((status = 200, body = ResolveOut), (status = 400), (status = 403), (status = 502)))]
pub async fn resolve_names(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<ResolveIn>,
) -> Result<Json<ResolveOut>, AppError> {
    session.require(&state, ADMIN_TIERS).await?;
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
    }))
}
