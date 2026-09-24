//! Keeps account tiers current: fetch affiliations from ESI, evaluate the
//! main against the tier rules, store the result.

use serde::{Deserialize, Serialize};
use tether_core::tiers::{Affiliation, Tier};
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::tiers as db;
use tether_esi::{Esi, EsiError};
use tether_jobs::{JobError, NewJob, Registry};

/// Job kind: refresh one account's affiliations and tier.
pub const REFRESH_ACCOUNT_JOB: &str = "tiers.refresh_account";

#[derive(Debug, Serialize, Deserialize)]
struct RefreshAccount {
    account_id: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum TierError {
    #[error(transparent)]
    Esi(#[from] EsiError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Fetches affiliations for all of the account's characters (one bulk ESI
/// call), stores them and re-evaluates the tier.
pub async fn refresh_account(
    db: &PgPool,
    esi: &Esi,
    account: AccountId,
) -> Result<Tier, TierError> {
    let ids = db::character_ids(db, account).await?;
    let fresh: Vec<(i64, Affiliation)> = esi
        .affiliations(&ids)
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
    Ok(evaluate_account(db, account).await?)
}

/// Re-evaluates the tier from stored affiliations (no ESI call), e.g. after
/// the main changes.
pub async fn evaluate_account(db: &PgPool, account: AccountId) -> Result<Tier, sqlx::Error> {
    let rules = db::load_rules(db).await?;
    let tier = rules.evaluate(db::main_affiliation(db, account).await?);
    let mut tx = db.begin().await?;
    let previous = db::set_account_tier(&mut *tx, account, tier).await?;
    if previous != Some(tier) {
        tracing::info!(
            account = account.0,
            from = previous.map(Tier::as_str),
            to = tier.as_str(),
            "tier changed"
        );
        audit::record(
            &mut *tx,
            Actor::System,
            "tier.change",
            Some(&format!("account:{}", account.0)),
            serde_json::json!({ "from": previous.map(Tier::as_str), "to": tier.as_str() }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(tier)
}

/// Queues a background refresh, retried with backoff while ESI is down.
pub async fn enqueue_refresh(db: &PgPool, account: AccountId) -> Result<(), sqlx::Error> {
    let payload = serde_json::json!({ "account_id": account.0 });
    tether_jobs::enqueue(db, NewJob::new(REFRESH_ACCOUNT_JOB, payload)).await?;
    Ok(())
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, esi: Esi) {
    registry.register(REFRESH_ACCOUNT_JOB, move |job| {
        let (db, esi) = (db.clone(), esi.clone());
        async move {
            let payload: RefreshAccount =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            match refresh_account(&db, &esi, AccountId(payload.account_id)).await {
                Ok(_) => Ok(()),
                Err(err) => Err(JobError::retry(err)),
            }
        }
    });
}
