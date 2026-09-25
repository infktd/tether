//! Character ownership, as Alliance Auth checks it: every stored token is
//! refreshed every 4 hours. A refresh that shows another owner hash means
//! the character was sold; a dead token means its owner no longer proves
//! control. Either way the character leaves its account (and if it was the
//! main, the account has none until its owner picks one).

use std::time::Duration;

use tether_db::PgPool;
use tether_db::accounts::{self, LossCause, Lost};
use tether_db::compliance as db;
use tether_esi::vault::{TokenVault, VaultError};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

/// Job kind: the ownership check.
pub const OWNERSHIP_CHECK_JOB: &str = "ownership.check";
/// Each token is refreshed at most this often (AA: every 4 hours).
const CHECK_EVERY_HOURS: i32 = 4;
/// Tokens per hourly run: four runs cover 8,000 characters.
const BATCH: i64 = 2000;

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        OWNERSHIP_CHECK_JOB,
        OWNERSHIP_CHECK_JOB,
        Duration::from_secs(60 * 60),
    )]
}

/// After a character left its account (already audited, in the same
/// transaction): logs it and re-evaluates the account.
pub async fn after_lost(db: &PgPool, lost: &Lost) -> Result<(), sqlx::Error> {
    if lost.owner_lost {
        tracing::warn!(
            character_id = lost.character_id,
            "the owner account lost its last character; first-run setup is open again to \
             whoever holds SETUP_TOKEN"
        );
    }
    tracing::info!(
        character_id = lost.character_id,
        from = lost.from.0,
        was_main = lost.was_main,
        "character left its account"
    );
    crate::states::evaluate_account(db, lost.from).await?;
    // After the re-evaluation, and never in its way: a failed notice is
    // only logged.
    if let Err(err) = crate::notifications::character_lost(db, lost).await {
        tracing::warn!(error = %err, character_id = lost.character_id, "notifying a lost character");
    }
    Ok(())
}

/// If more than this many tokens (or a tenth of those refreshed, whichever
/// is more) come back revoked in one run, something is wrong with SSO or
/// the app, not with the characters: nobody loses anything this run.
const BREAKER_MIN: usize = 5;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    pub checked: usize,
    pub lost: usize,
}

/// Refreshes tokens due a check, then takes away every character whose
/// token is dead, however it was found (here, on a plugin call, on Corp
/// Stats).
pub async fn check(db: &PgPool, vault: &TokenVault) -> Result<Checked, JobError> {
    let due = db::tokens_due(db, CHECK_EVERY_HOURS, BATCH)
        .await
        .map_err(JobError::retry)?;
    let mut checked = 0;
    let mut revoked = 0;
    for character_id in &due {
        match vault.verify(*character_id).await {
            Ok(()) | Err(VaultError::OwnerChanged) => {}
            Err(VaultError::Revoked) => revoked += 1,
            // SSO is down or not set up: the rest wait for the next run
            // (a dead SSO isn't proof of anything).
            Err(VaultError::Unavailable(_) | VaultError::NotConfigured) => break,
            Err(err) => tracing::warn!(character_id, error = %err, "ownership check"),
        }
        db::mark_checked(db, *character_id)
            .await
            .map_err(JobError::retry)?;
        checked += 1;
    }
    // Sales are proven by the owner hash: they always go through.
    let mut lost = sweep_sold(db).await?;
    if revoked > BREAKER_MIN.max(checked / 10) {
        tracing::error!(
            checked,
            revoked,
            "too many tokens revoked at once: suspecting SSO or the app, not the characters; \
             nobody loses a character for a dead token this run"
        );
        trip(db, revoked, checked).await?;
        return Ok(Checked { checked, lost });
    }
    lost += sweep_dead(db, false).await?;
    tracing::info!(checked, lost, "ownership check done");
    Ok(Checked { checked, lost })
}

/// Where a tripped breaker is recorded, for `doctor` and admins.
pub const BREAKER_SETTING: &str = "ownership.breaker";

async fn trip(db: &PgPool, dead: usize, total: usize) -> Result<(), JobError> {
    tether_db::settings::set(
        db,
        BREAKER_SETTING,
        serde_json::json!({ "at": chrono::Utc::now().to_rfc3339(), "dead": dead, "total": total }),
    )
    .await
    .map_err(JobError::retry)
}

async fn sweep_sold(db: &PgPool) -> Result<usize, JobError> {
    let mut lost = 0;
    for (character_id, reason, _) in db::revoked_characters(db).await.map_err(JobError::retry)? {
        if reason.as_deref() == Some("owner hash changed")
            && let Some(gone) = accounts::lose_ownership(db, character_id, LossCause::Sold)
                .await
                .map_err(JobError::retry)?
        {
            after_lost(db, &gone).await.map_err(JobError::retry)?;
            lost += 1;
        }
    }
    Ok(lost)
}

/// Takes away characters whose token has been dead past the grace. If a
/// tenth or more of all tokens are, something is wrong with SSO or the
/// app: nothing happens (the breaker trips, `doctor` says so) unless
/// `force` (an admin, having looked: `tether ownership sweep --force`).
pub async fn sweep_dead(db: &PgPool, force: bool) -> Result<usize, JobError> {
    let now = chrono::Utc::now();
    let past_grace: Vec<(i64, Option<String>)> = db::revoked_characters(db)
        .await
        .map_err(JobError::retry)?
        .into_iter()
        .filter(|(_, reason, at)| {
            reason.as_deref() != Some("owner hash changed")
                && at.is_some_and(|at| now - at > chrono::Duration::days(1))
        })
        .map(|(id, reason, _)| (id, reason))
        .collect();
    // Tokens their owners deleted (Token Management) say nothing about
    // SSO: they go, and don't count toward the breaker.
    let suspicious = past_grace
        .iter()
        .filter(|(_, reason)| reason.as_deref() != Some("deleted"))
        .count();
    let total = db::token_count(db).await.map_err(JobError::retry)?;
    if !force && suspicious > BREAKER_MIN.max(total / 10) {
        tracing::error!(
            dead = suspicious,
            total,
            "a tenth or more of all tokens are revoked: suspecting SSO or the app, not the \
             characters; nobody loses a character for a dead token until an admin checks \
             (`tether ownership sweep --force`)"
        );
        trip(db, suspicious, total).await?;
        return Ok(0);
    }
    tether_db::settings::delete(db, BREAKER_SETTING)
        .await
        .map_err(JobError::retry)?;
    let mut lost = 0;
    for (character_id, _) in past_grace {
        if let Some(gone) = accounts::lose_ownership(db, character_id, LossCause::Token)
            .await
            .map_err(JobError::retry)?
        {
            after_lost(db, &gone).await.map_err(JobError::retry)?;
            lost += 1;
        }
    }
    Ok(lost)
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, vault: std::sync::Arc<TokenVault>) {
    registry.register(OWNERSHIP_CHECK_JOB, move |_job| {
        let (db, vault) = (db.clone(), vault.clone());
        async move {
            check(&db, &vault).await?;
            Ok(())
        }
    });
}
