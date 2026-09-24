//! Admin API: groups, permissions, audit log. Every change is audited in
//! the same transaction.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tether_core::permissions::{
    ADMIN_AUDIT, ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_TIERS, CORE_PERMISSIONS, JoinPolicy,
    is_known,
};
use tether_core::tiers::Tier;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};
use tether_db::tiers as tier_db;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::{AppError, is_foreign_key_violation, is_unique_violation};

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
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request(
            "Group names are 1 to 100 characters.",
        ));
    }
    let policy = JoinPolicy::parse(&body.join_policy)
        .ok_or_else(|| AppError::bad_request("join_policy must be open, request or assigned."))?;

    let mut tx = state.db.begin().await?;
    let id = match groups::create(&mut *tx, name, body.description.trim(), policy).await {
        Ok(id) => id,
        Err(err) if is_unique_violation(&err) => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "A group with that name already exists.",
            ));
        }
        Err(err) => return Err(err.into()),
    };
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "group.create",
        Some(&format!("group:{}", id.0)),
        json!({ "name": name, "join_policy": policy.as_str() }),
    )
    .await?;
    tx.commit().await?;
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
    let mut tx = state.db.begin().await?;
    let group = groups::get(&mut *tx, GroupId(id))
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))?;
    groups::delete(&mut *tx, group.id).await?;
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "group.delete",
        Some(&format!("group:{id}")),
        json!({ "name": group.name }),
    )
    .await?;
    tx.commit().await?;
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
    change_membership(&state, &session, id, body.account_id, MembershipChange::Add).await
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
    change_membership(&state, &session, id, account_id, MembershipChange::Remove).await
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
    change_membership(&state, &session, id, account_id, MembershipChange::Approve).await
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
    change_membership(&state, &session, id, account_id, MembershipChange::Deny).await
}

#[derive(Debug, Clone, Copy)]
enum MembershipChange {
    Add,
    Remove,
    Approve,
    Deny,
}

impl MembershipChange {
    fn action(self) -> &'static str {
        match self {
            Self::Add => "group.member.add",
            Self::Remove => "group.member.remove",
            Self::Approve => "group.request.approve",
            Self::Deny => "group.request.deny",
        }
    }
}

async fn change_membership(
    state: &AppState,
    session: &CurrentSession,
    group_id: i64,
    account_id: i64,
    change: MembershipChange,
) -> Result<StatusCode, AppError> {
    let (group, account) = (GroupId(group_id), AccountId(account_id));
    let no_request = || AppError::not_found("No pending request from that account.");
    let mut tx = state.db.begin().await?;
    groups::get(&mut *tx, group)
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let changed = match change {
        MembershipChange::Remove => groups::remove_member(&mut *tx, group, account).await?,
        MembershipChange::Deny => {
            if !groups::remove_request(&mut *tx, group, account).await? {
                return Err(no_request());
            }
            true
        }
        MembershipChange::Add | MembershipChange::Approve => {
            if matches!(change, MembershipChange::Approve)
                && !groups::remove_request(&mut *tx, group, account).await?
            {
                return Err(no_request());
            }
            match groups::add_member(&mut *tx, group, account).await {
                Ok(added) => added,
                Err(err) if is_foreign_key_violation(&err) => {
                    return Err(AppError::not_found("No such account."));
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    if changed {
        audit::record(
            &mut *tx,
            Actor::Account(session.account),
            change.action(),
            Some(&format!("group:{group_id}")),
            json!({ "account_id": account_id }),
        )
        .await?;
    }
    tx.commit().await?;
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
    pub name: &'static str,
    pub description: &'static str,
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
        available: CORE_PERMISSIONS
            .iter()
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
    if !is_known(&body.permission) {
        return Err(AppError::bad_request(format!(
            "Unknown permission {:?}.",
            body.permission
        )));
    }
    let grantee = match (body.tier.as_deref(), body.group_id) {
        (Some(tier), None) => Grantee::Tier(
            Tier::parse(tier)
                .ok_or_else(|| AppError::bad_request("tier must be member, allied or guest."))?,
        ),
        (None, Some(group)) => Grantee::Group(GroupId(group)),
        _ => {
            return Err(AppError::bad_request(
                "Grant to exactly one of tier or group_id.",
            ));
        }
    };

    let mut tx = state.db.begin().await?;
    if let Grantee::Group(group) = grantee {
        groups::get(&mut *tx, group)
            .await?
            .ok_or_else(|| AppError::not_found("No such group."))?;
    }
    let Some(id) = permissions::grant(&mut *tx, &body.permission, grantee).await? else {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That grant already exists.",
        ));
    };
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "permission.grant",
        Some(&format!("grant:{id}")),
        grant_details(&body.permission, grantee),
    )
    .await?;
    tx.commit().await?;
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
    let mut tx = state.db.begin().await?;
    let grant = permissions::revoke(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such grant."))?;
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "permission.revoke",
        Some(&format!("grant:{id}")),
        grant_details(&grant.permission, grant.grantee),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn grant_details(permission: &str, grantee: Grantee) -> Value {
    match grantee {
        Grantee::Tier(tier) => json!({ "permission": permission, "tier": tier.as_str() }),
        Grantee::Group(group) => json!({ "permission": permission, "group_id": group.0 }),
    }
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
    let tier = match Tier::parse(&body.tier) {
        Some(tier @ (Tier::Member | Tier::Allied)) => tier,
        _ => return Err(AppError::bad_request("tier must be member or allied.")),
    };
    apply_tier_rule(
        &state,
        Actor::Account(session.account),
        body.entity_id,
        tier,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes an alliance or corporation Member or Allied. Its name and kind come
/// from ESI, never the client. Audited, and queues a re-evaluation of every
/// account. Callers check permissions.
pub async fn apply_tier_rule(
    state: &AppState,
    actor: Actor,
    entity_id: i64,
    tier: Tier,
) -> Result<tier_db::TierRule, AppError> {
    let entity = state
        .esi
        .names(&[entity_id])
        .await
        .map_err(esi_unavailable)?
        .into_iter()
        .find(|e| e.id == entity_id)
        .ok_or_else(|| AppError::not_found("ESI doesn't know that id."))?;
    let kind = entity
        .kind
        .ok_or_else(|| AppError::bad_request("That id isn't an alliance or corporation."))?;
    let rule = tier_db::TierRule {
        entity_id: entity.id,
        kind,
        tier,
        name: entity.name,
    };
    let mut tx = state.db.begin().await?;
    tier_db::set_rule(&mut *tx, &rule).await?;
    audit::record(
        &mut *tx,
        actor,
        "tier.rule.set",
        Some(&format!("{}:{}", kind.as_str(), entity.id)),
        json!({ "name": rule.name, "tier": tier.as_str() }),
    )
    .await?;
    crate::tiers::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(rule)
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
    let mut tx = state.db.begin().await?;
    if !tier_db::remove_rule(&mut *tx, entity_id).await? {
        return Err(AppError::not_found("No rule for that id."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(session.account),
        "tier.rule.remove",
        Some(&entity_id.to_string()),
        json!({}),
    )
    .await?;
    crate::tiers::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
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
        .resolve_names(&names)
        .await
        .map_err(esi_unavailable)?;
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

pub(crate) fn esi_unavailable(err: tether_esi::EsiError) -> AppError {
    tracing::warn!(error = %err, "ESI lookup failed");
    AppError::new(
        StatusCode::BAD_GATEWAY,
        "ESI didn't answer. Try again in a moment.",
    )
}
