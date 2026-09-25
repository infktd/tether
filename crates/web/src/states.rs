//! Keeps account states current: fetch affiliations from ESI, evaluate the
//! main against the states, store the result.

use serde::{Deserialize, Serialize};
use tether_core::states::{Affiliation, StateId, StateRules};
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::states as db;
use tether_esi::{Esi, EsiError, Priority};
use tether_jobs::{JobError, NewJob, Registry};

/// Job kind: refresh one account's affiliations and state.
pub const REFRESH_ACCOUNT_JOB: &str = "states.refresh_account";
/// Job kind: re-evaluate every account from stored affiliations, e.g. after
/// the states change.
pub const EVALUATE_ALL_JOB: &str = "states.evaluate_all";

#[derive(Debug, Serialize, Deserialize)]
struct RefreshAccount {
    account_id: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Esi(#[from] EsiError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Fetches affiliations for all of the account's characters (one bulk ESI
/// call), stores them and re-evaluates the state.
pub async fn refresh_account(
    db: &PgPool,
    esi: &Esi,
    account: AccountId,
    priority: Priority,
) -> Result<StateId, StateError> {
    let ids = db::character_ids(db, account).await?;
    let fresh: Vec<(i64, Affiliation)> = esi
        .affiliations(&ids, priority)
        .await?
        .into_iter()
        .map(|a| {
            (
                a.character_id,
                Affiliation {
                    corporation_id: a.corporation_id,
                    alliance_id: a.alliance_id,
                },
            )
        })
        .collect();
    db::update_affiliations(db, &fresh).await?;
    Ok(evaluate_account(db, account).await?.state)
}

/// A state evaluation's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluated {
    pub state: StateId,
    pub previous: Option<StateId>,
}

impl Evaluated {
    /// True when the account's state moved (what Discord role sync reacts
    /// to).
    pub fn changed(&self) -> bool {
        self.previous != Some(self.state)
    }
}

/// Re-evaluates the state from stored affiliations (no ESI call), e.g.
/// after the main changes. The rules are read in the same transaction as
/// the write, under a shared lock on the states, so an evaluation can never
/// store a result from rules a newer change has replaced.
pub async fn evaluate_account(db: &PgPool, account: AccountId) -> Result<Evaluated, sqlx::Error> {
    let mut tx = db.begin().await?;
    db::lock_shared(&mut tx).await?;
    let rules = db::load_rules(&mut tx).await?;
    let evaluated = evaluate_in(&mut tx, &rules, account, None).await?;
    tx.commit().await?;
    Ok(evaluated)
}

/// Evaluates and stores one account's state inside the caller's
/// transaction, auditing a change. `from` names the previous state when
/// it no longer exists (a deleted state).
pub(crate) async fn evaluate_in(
    tx: &mut sqlx::PgConnection,
    rules: &StateRules,
    account: AccountId,
    from: Option<&str>,
) -> Result<Evaluated, sqlx::Error> {
    let main = db::main(&mut *tx, account).await?;
    // The state comes from the main's affiliation alone, as in Alliance
    // Auth. Compliance (F11) is a flag on top: every character registered
    // with the state's scopes.
    let state = rules.evaluate(main);
    let compliant = state == rules.guest()
        || crate::compliance::problems(&mut *tx, account, state)
            .await?
            .is_empty();
    let before = db::set_account_state(&mut *tx, account, state, compliant).await?;
    let previous = before.map(|(state, _)| state);
    let target = Some(&format!("account:{}", account.0));
    if from.is_some() || (previous.is_some() && previous != Some(state)) {
        let from = match (from, previous) {
            (Some(name), _) => Some(name.to_owned()),
            (None, Some(id)) => db::get(&mut *tx, id).await?.map(|s| s.name),
            (None, None) => None,
        };
        let to = db::get(&mut *tx, state).await?.map(|s| s.name);
        tracing::info!(account = account.0, ?from, ?to, "state changed");
        audit::record(
            &mut *tx,
            Actor::System,
            "state.change",
            target.map(String::as_str),
            serde_json::json!({ "from": from, "to": to }),
        )
        .await?;
    }
    if before.is_some_and(|(_, was)| was != compliant) {
        tracing::info!(account = account.0, compliant, "compliance changed");
        audit::record(
            &mut *tx,
            Actor::System,
            "compliance.change",
            target.map(String::as_str),
            serde_json::json!({ "compliant": compliant }),
        )
        .await?;
    }
    // The Compliant group: compliant accounts in a state other than Guest.
    if let Some(group) = tether_db::compliance::managed_group(&mut *tx, "compliant").await? {
        let member = compliant && state != rules.guest();
        if tether_db::compliance::set_group_member(&mut *tx, group, account, member).await? {
            audit::record(
                &mut *tx,
                Actor::System,
                if member {
                    "group.member.add"
                } else {
                    "group.member.remove"
                },
                Some(&format!("group:{}", group.0)),
                serde_json::json!({ "account_id": account.0, "reason": "compliance" }),
            )
            .await?;
        }
    }
    Ok(Evaluated { state, previous })
}

/// Queues a background refresh, retried with backoff while ESI is down.
pub async fn enqueue_refresh(db: &PgPool, account: AccountId) -> Result<(), sqlx::Error> {
    let payload = serde_json::json!({ "account_id": account.0 });
    tether_jobs::enqueue(db, NewJob::new(REFRESH_ACCOUNT_JOB, payload)).await?;
    Ok(())
}

pub async fn enqueue_evaluate_all<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<(), sqlx::Error> {
    tether_jobs::enqueue(
        executor,
        NewJob::new(EVALUATE_ALL_JOB, serde_json::json!({})),
    )
    .await?;
    Ok(())
}

/// Every account, each in its own short transaction with the rules read
/// fresh (see [`evaluate_account`]).
pub async fn evaluate_all(db: &PgPool) -> Result<usize, sqlx::Error> {
    let accounts = tether_db::accounts::all_ids(db).await?;
    for account in &accounts {
        evaluate_account(db, *account).await?;
    }
    Ok(accounts.len())
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, esi: Esi) {
    let evaluate_db = db.clone();
    registry.register(EVALUATE_ALL_JOB, move |_job| {
        let db = evaluate_db.clone();
        async move {
            let count = evaluate_all(&db).await.map_err(JobError::retry)?;
            tracing::info!(accounts = count, "re-evaluated all states");
            Ok(())
        }
    });
    registry.register(REFRESH_ACCOUNT_JOB, move |job| {
        let (db, esi) = (db.clone(), esi.clone());
        async move {
            let payload: RefreshAccount =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            match refresh_account(&db, &esi, AccountId(payload.account_id), Priority::Bulk).await {
                Ok(_) => Ok(()),
                Err(err) => Err(JobError::retry(err)),
            }
        }
    });
}
