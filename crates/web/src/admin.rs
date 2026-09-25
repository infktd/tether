//! Admin actions, shared by the JSON API and the admin pages. Callers check
//! permissions; everything here is audited in the same transaction as the
//! change.

use axum::http::StatusCode;
use serde_json::{Value, json};
use tether_core::states::StateId;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use crate::AppState;
use crate::error::AppError;

/// Deactivates (`active = false`) or reactivates an account, as AA's
/// inactive users: Guest, no permissions, sessions ended, sign-in refused.
/// Audited; the account is re-evaluated at once. Never the owner.
pub async fn set_active(
    db: &tether_db::PgPool,
    actor: Actor,
    account: AccountId,
    active: bool,
) -> Result<(), AppError> {
    // Deactivating someone must not be a way past what you hold: only
    // accounts whose permissions are all yours (the owner can't be
    // deactivated at all).
    if let Actor::Account(me) = actor {
        let mine = tether_db::permissions::effective(db, me).await?;
        // A deactivated account holds nothing now; what counts is what it
        // gets back: its state's grants once re-evaluated.
        let theirs = if active {
            would_hold(db, account).await?
        } else {
            tether_db::permissions::effective(db, account).await?
        };
        if let Some(missing) = theirs.iter().find(|p| !mine.contains(*p)) {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                format!("That account holds {missing}, which you don't, so you can't change it."),
            ));
        }
    }
    let mut tx = db.begin().await?;
    let changed = if active {
        tether_db::accounts::reactivate(&mut *tx, account).await?
    } else {
        tether_db::accounts::deactivate(
            &mut tx,
            account,
            match actor {
                Actor::Account(a) => Some(a),
                _ => None,
            },
        )
        .await?
    };
    if !changed {
        return Err(
            match tether_db::accounts::is_active(&mut *tx, account).await? {
                None => AppError::not_found("No such account."),
                Some(_) if !active => AppError::bad_request(
                    "That account is already deactivated, or it's the owner, which can't be.",
                ),
                Some(_) => AppError::bad_request("That account is already active."),
            },
        );
    }
    // As AA's deactivation, the account leaves its groups (and so their
    // Discord roles); reactivating doesn't bring them back.
    let mut left = Vec::new();
    if !active {
        for group in groups::leave_all(&mut tx, account).await? {
            audit::record(
                &mut *tx,
                actor,
                "group.member.remove",
                Some(&format!("group:{}", group.0)),
                json!({ "account_id": account.0, "reason": "deactivated" }),
            )
            .await?;
            left.push(group.0);
        }
    }
    audit::record(
        &mut *tx,
        actor,
        if active {
            "account.reactivate"
        } else {
            "account.deactivate"
        },
        Some(&format!("account:{}", account.0)),
        json!({ "groups_left": left }),
    )
    .await?;
    tx.commit().await?;
    crate::states::evaluate_account(db, account).await?;
    Ok(())
}

/// What a deactivated account would hold once reactivated: the grants of
/// the state its main would put it in (it left its groups on deactivation).
async fn would_hold(
    db: &tether_db::PgPool,
    account: AccountId,
) -> Result<std::collections::BTreeSet<String>, AppError> {
    let mut conn = db.acquire().await?;
    let rules = tether_db::states::load_rules(&mut conn).await?;
    let main = tether_db::states::main(&mut *conn, account).await?;
    let state = rules.evaluate(main);
    Ok(tether_db::states::granted_to(&mut *conn, &[state])
        .await?
        .into_iter()
        .collect())
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
            // Shared lock: a concurrent change to the group's flags waits.
            groups::lock(&mut tx, group, false).await?;
            let group = groups::get(&mut *tx, group)
                .await?
                .ok_or_else(|| AppError::not_found("No such group."))?;
            if sensitive && group.flags.anyone_can_join() {
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
