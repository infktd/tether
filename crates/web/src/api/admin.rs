//! Admin API: groups, permissions, audit log. Every change is audited in
//! the same transaction.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tether_core::permissions::{
    ADMIN_AUDIT, ADMIN_GROUPS, ADMIN_PERMISSIONS, CORE_PERMISSIONS, JoinPolicy, is_known,
};
use tether_core::tiers::Tier;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::{AppError, is_foreign_key_violation, is_unique_violation};

// ---- groups ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NewGroup {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub join_policy: String,
}

#[derive(Debug, Serialize)]
pub struct Created {
    pub id: i64,
}

/// `POST /api/admin/groups`
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

#[derive(Debug, Deserialize)]
pub struct MemberIn {
    pub account_id: i64,
}

/// `POST /api/admin/groups/{id}/members`
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
pub async fn remove_member(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    change_membership(&state, &session, id, account_id, MembershipChange::Remove).await
}

/// `POST /api/admin/groups/{id}/requests/{account_id}/approve`
pub async fn approve_request(
    State(state): State<AppState>,
    session: CurrentSession,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<StatusCode, AppError> {
    session.require(&state, ADMIN_GROUPS).await?;
    change_membership(&state, &session, id, account_id, MembershipChange::Approve).await
}

/// `POST /api/admin/groups/{id}/requests/{account_id}/deny`
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

#[derive(Debug, Serialize)]
pub struct RequestOut {
    pub account_id: i64,
    pub main_name: String,
}

/// `GET /api/admin/groups/{id}/requests`
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

#[derive(Debug, Serialize)]
pub struct PermissionsOut {
    pub available: Vec<PermissionInfo>,
    pub grants: Vec<GrantOut>,
}

#[derive(Debug, Serialize)]
pub struct PermissionInfo {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Serialize)]
pub struct GrantOut {
    pub id: i64,
    pub permission: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<i64>,
}

/// `GET /api/admin/permissions`
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

#[derive(Debug, Deserialize)]
pub struct GrantIn {
    pub permission: String,
    pub tier: Option<String>,
    pub group_id: Option<i64>,
}

/// `POST /api/admin/permissions/grants`: grant to a tier or a group.
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

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    pub before: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AuditEntryOut {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_account_id: Option<i64>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub details: Value,
}

/// `GET /api/admin/audit?limit=&before=`: newest first.
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
