//! Admin actions, shared by the JSON API and the admin pages. Callers check
//! permissions; everything here is audited in the same transaction as the
//! change.

use axum::http::StatusCode;
use serde_json::{Value, json};
use tether_core::permissions::JoinPolicy;
use tether_core::states::StateId;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use crate::AppState;
use crate::error::{AppError, is_foreign_key_violation, is_unique_violation};

const MAX_DESCRIPTION: usize = 500;

pub async fn create_group(
    state: &AppState,
    actor: AccountId,
    name: &str,
    description: &str,
    join_policy: &str,
) -> Result<GroupId, AppError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request(
            "Group names are 1 to 100 characters.",
        ));
    }
    if description.trim().len() > MAX_DESCRIPTION {
        return Err(AppError::bad_request(
            "Group descriptions are at most 500 characters.",
        ));
    }
    let policy = JoinPolicy::parse(join_policy)
        .ok_or_else(|| AppError::bad_request("join_policy must be open, request or assigned."))?;
    let mut tx = state.db.begin().await?;
    let id = match groups::create(&mut *tx, name, description.trim(), policy).await {
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
        Actor::Account(actor),
        "group.create",
        Some(&format!("group:{}", id.0)),
        json!({ "name": name, "join_policy": policy.as_str() }),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn delete_group(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let group = groups::get(&mut *tx, GroupId(id))
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))?;
    groups::delete(&mut *tx, group.id).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "group.delete",
        Some(&format!("group:{id}")),
        json!({ "name": group.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipChange {
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

pub async fn change_membership(
    state: &AppState,
    actor: AccountId,
    group_id: i64,
    account_id: i64,
    change: MembershipChange,
) -> Result<(), AppError> {
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
            // Adding someone to a group hands them its permissions, so the
            // admin must already hold all of them: admin.groups alone mustn't
            // be a path to admin.permissions.
            let actor_has = permissions::effective(&state.db, actor).await?;
            let group_grants = permissions::of_group(&mut *tx, group).await?;
            if let Some(missing) = group_grants.iter().find(|p| !actor_has.contains(*p)) {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    format!(
                        "This group grants {missing}, which you don't have, so you can't add members to it."
                    ),
                ));
            }
            if change == MembershipChange::Approve
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
            Actor::Account(actor),
            change.action(),
            Some(&format!("group:{group_id}")),
            json!({ "account_id": account_id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Exactly one of a state or a group.
pub fn grantee(state_id: Option<i64>, group_id: Option<i64>) -> Result<Grantee, AppError> {
    match (state_id, group_id) {
        (Some(state), None) => Ok(Grantee::State(StateId(state))),
        (None, Some(group)) => Ok(Grantee::Group(GroupId(group))),
        _ => Err(AppError::bad_request(
            "Grant to exactly one of state_id or group_id.",
        )),
    }
}

pub async fn grant(
    state: &AppState,
    actor: AccountId,
    permission: &str,
    grantee: Grantee,
) -> Result<i64, AppError> {
    let mut tx = state.db.begin().await?;
    // In the grant's transaction: a plugin's permission stays locked until
    // the grant is in, so a racing uninstall can't leave it behind.
    if !permissions::is_known(&mut *tx, permission).await? {
        return Err(AppError::bad_request(format!(
            "Unknown permission {permission:?}."
        )));
    }
    // Anyone who logs in with EVE is Guest, and anyone signed in can join an
    // Open group: admin rights there would be admin rights for strangers.
    let sensitive = tether_core::permissions::is_sensitive(permission);
    match grantee {
        Grantee::State(id) => {
            let target = tether_db::states::get(&mut *tx, id)
                .await?
                .ok_or_else(|| AppError::not_found("No such state."))?;
            if sensitive && target.is_guest() {
                return Err(AppError::bad_request(format!(
                    "{permission} can't go to Guest: anyone who logs in with EVE is Guest."
                )));
            }
        }
        Grantee::Group(group) => {
            let group = groups::get(&mut *tx, group)
                .await?
                .ok_or_else(|| AppError::not_found("No such group."))?;
            if sensitive && group.join_policy == JoinPolicy::Open {
                return Err(AppError::bad_request(format!(
                    "{permission} can't go to an Open group: anyone can join it."
                )));
            }
        }
    }
    let Some(id) = permissions::grant(&mut *tx, permission, grantee).await? else {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That grant already exists.",
        ));
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "permission.grant",
        Some(&format!("grant:{id}")),
        grant_details(permission, grantee),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn revoke(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let grant = permissions::revoke(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such grant."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "permission.revoke",
        Some(&format!("grant:{id}")),
        grant_details(&grant.permission, grant.grantee),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

fn grant_details(permission: &str, grantee: Grantee) -> Value {
    match grantee {
        Grantee::State(state) => json!({ "permission": permission, "state_id": state.0 }),
        Grantee::Group(group) => json!({ "permission": permission, "group_id": group.0 }),
    }
}

pub fn names_unavailable(err: tether_esi::names::NamesError) -> AppError {
    match err {
        tether_esi::names::NamesError::Esi(err) => esi_unavailable(err),
        tether_esi::names::NamesError::Db(err) => err.into(),
    }
}

pub fn esi_unavailable(err: tether_esi::EsiError) -> AppError {
    tracing::warn!(error = %err, "ESI lookup failed");
    AppError::new(
        StatusCode::BAD_GATEWAY,
        "ESI didn't answer. Try again in a moment.",
    )
}
