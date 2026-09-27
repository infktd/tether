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
/// Audited, and re-evaluated in the same transaction. Never a superuser.
pub async fn set_active(
    db: &tether_db::PgPool,
    actor: Actor,
    account: AccountId,
    active: bool,
) -> Result<(), AppError> {
    crate::sudo::check(if active {
        crate::sudo::Action::AccountReactivate
    } else {
        crate::sudo::Action::AccountDeactivate
    })?;
    let mut tx = db.begin().await?;
    // The same lock order as every evaluation: the states first.
    tether_db::states::lock_shared(&mut tx).await?;
    // Changing someone must not be a way past what you hold: only accounts
    // whose permissions are all yours (a superuser can't be deactivated at
    // all). Deactivating checks what they hold now; reactivating checks
    // what they hold afterwards (their state's grants, and those of any
    // Auto or compliance groups they rejoin), in this transaction, so
    // nothing is kept if the check fails.
    let mine = match actor {
        Actor::Account(me) => Some(tether_db::permissions::effective_in(&mut tx, me).await?),
        _ => None,
    };
    if !active && let Some(mine) = &mine {
        let theirs = tether_db::permissions::effective_in(&mut tx, account).await?;
        refuse_unless_held(mine, &theirs)?;
    }
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
                    "That account is already deactivated, or it's a superuser, which can't be.",
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
    let rules = tether_db::states::load_rules(&mut tx).await?;
    crate::states::evaluate_in(&mut tx, &rules, account, None).await?;
    if active && let Some(mine) = &mine {
        let theirs = tether_db::permissions::effective_in(&mut tx, account).await?;
        // Dropping the transaction rolls the reactivation back.
        refuse_unless_held(mine, &theirs)?;
    }
    tx.commit().await?;
    Ok(())
}

fn refuse_unless_held(
    mine: &std::collections::BTreeSet<String>,
    theirs: &std::collections::BTreeSet<String>,
) -> Result<(), AppError> {
    match theirs.iter().find(|p| !mine.contains(*p)) {
        Some(missing) => Err(AppError::new(
            StatusCode::FORBIDDEN,
            format!("That account holds {missing}, which you don't, so you can't change it."),
        )),
        None => Ok(()),
    }
}

/// Exactly one of a state, a group or a user (AA's user permissions).
pub fn grantee(
    state_id: Option<i64>,
    group_id: Option<i64>,
    account_id: Option<i64>,
) -> Result<Grantee, AppError> {
    match (state_id, group_id, account_id) {
        (Some(state), None, None) => Ok(Grantee::State(StateId(state))),
        (None, Some(group), None) => Ok(Grantee::Group(GroupId(group))),
        (None, None, Some(account)) => Ok(Grantee::Account(AccountId(account))),
        _ => Err(AppError::bad_request(
            "Grant to exactly one of state_id, group_id or account_id.",
        )),
    }
}

/// Granting must not be a way past what you hold: only a permission you
/// hold yourself goes to (or comes from) a state, a group or one person,
/// as adding people to a group or state needs its grants.
fn require_held(
    mine: &std::collections::BTreeSet<String>,
    permission: &str,
    doing: &str,
) -> Result<(), AppError> {
    if mine.contains(permission) {
        return Ok(());
    }
    Err(AppError::new(
        StatusCode::FORBIDDEN,
        format!("You don't hold {permission}, so you can't {doing} it."),
    ))
}

pub async fn grant(
    state: &AppState,
    actor: AccountId,
    permission: &str,
    grantee: Grantee,
) -> Result<i64, AppError> {
    let sensitive = tether_core::permissions::is_sensitive(permission);
    if sensitive {
        crate::sudo::check(crate::sudo::Action::SensitivePermission)?;
    }
    let mut tx = state.db.begin().await?;
    // In the grant's transaction: a plugin's permission stays locked until
    // the grant is in, so a racing uninstall can't leave it behind.
    if !permissions::is_known(&mut *tx, permission).await? {
        return Err(AppError::bad_request(format!(
            "Unknown permission {permission:?}."
        )));
    }
    let mine = permissions::effective_in(&mut tx, actor).await?;
    require_held(&mine, permission, "grant")?;
    // Anyone who logs in with EVE is Guest (or in a public state), and
    // anyone signed in can join an Open group: admin rights there would be
    // admin rights for strangers.
    match grantee {
        Grantee::State(id) => {
            // Shared lock: making the state public waits for this grant.
            tether_db::states::lock_shared(&mut tx).await?;
            let target = tether_db::states::get(&mut *tx, id)
                .await?
                .ok_or_else(|| AppError::not_found("No such state."))?;
            if sensitive && target.is_guest() {
                return Err(AppError::bad_request(format!(
                    "{permission} can't go to Guest: anyone who logs in with EVE is Guest."
                )));
            }
            if sensitive && target.public {
                return Err(AppError::bad_request(format!(
                    "{permission} can't go to {}: it's public, so anyone who logs in with EVE \
                     can be in it.",
                    target.name
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
        Grantee::Account(account) => {
            // Your own grants would outlast what gave you the permission.
            if account == actor {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "You can't grant permissions to yourself.",
                ));
            }
            if tether_db::accounts::is_active(&mut *tx, account)
                .await?
                .is_none()
            {
                return Err(AppError::not_found("No such account."));
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
    // Refusing drops the transaction, so the grant stays.
    if tether_core::permissions::is_sensitive(&grant.permission) {
        crate::sudo::check(crate::sudo::Action::SensitivePermission)?;
    }
    let mine = permissions::effective_in(&mut tx, actor).await?;
    require_held(&mine, &grant.permission, "revoke")?;
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

pub(crate) fn grant_details(permission: &str, grantee: Grantee) -> Value {
    match grantee {
        Grantee::State(state) => json!({ "permission": permission, "state_id": state.0 }),
        Grantee::Group(group) => json!({ "permission": permission, "group_id": group.0 }),
        Grantee::Account(account) => json!({ "permission": permission, "account_id": account.0 }),
    }
}

/// Makes an account a superuser (AA's `is_superuser`: every permission) or
/// stops it being one. Only superusers do it, freshly logged in (sudo
/// mode), audited; the last superuser stays one.
pub async fn set_superuser(
    db: &tether_db::PgPool,
    actor: AccountId,
    account: AccountId,
    on: bool,
) -> Result<(), AppError> {
    use tether_db::accounts::SuperuserChange;
    crate::sudo::check(crate::sudo::Action::SensitivePermission)?;
    let mut tx = db.begin().await?;
    // Who may is checked under the lock (a personal access token never
    // counts as a superuser).
    match tether_db::accounts::set_superuser(&mut tx, actor, account, on).await? {
        SuperuserChange::Changed => {}
        SuperuserChange::NotSuperuser => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "Only a superuser makes or unmakes superusers.",
            ));
        }
        SuperuserChange::Unchanged => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                if on {
                    "That account is already a superuser."
                } else {
                    "That account isn't a superuser."
                },
            ));
        }
        SuperuserChange::NotFound => return Err(AppError::not_found("No such account.")),
        SuperuserChange::Inactive => {
            return Err(AppError::bad_request(
                "That account is deactivated: reactivate it first.",
            ));
        }
        SuperuserChange::LastSuperuser => {
            return Err(AppError::bad_request(
                "That's the only superuser who can sign in: make someone else a superuser first.",
            ));
        }
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        if on {
            "account.superuser_grant"
        } else {
            "account.superuser_revoke"
        },
        Some(&format!("account:{}", account.0)),
        json!({}),
    )
    .await?;
    // What it holds changed, Discord access among it.
    tether_jobs::enqueue(
        &mut *tx,
        tether_jobs::NewJob::new(
            crate::discord_sync::SYNC_MEMBER_JOB,
            json!({ "account_id": account.0 }),
        )
        .max_attempts(10),
    )
    .await?;
    tx.commit().await?;
    // Superusers are never blacklisted: its state may change. The change
    // is done; if this fails, the scheduled evaluation catches up.
    if let Err(err) = crate::states::evaluate_account(db, account).await {
        tracing::warn!(account = account.0, error = %err, "re-evaluating after a superuser change");
    }
    Ok(())
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
