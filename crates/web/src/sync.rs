//! The scheduled affiliation sync (F11): every linked character's
//! corporation and alliance from ESI, then every account's state.
//!
//! This is how a member who leaves the alliance loses access with no admin
//! action: within one sync their main's new affiliation drops them to
//! Guest.

use std::time::{Duration, Instant};

use serde::Serialize;
use tether_core::states::Affiliation;
use tether_db::PgPool;
use tether_db::states as db;
use tether_esi::{CharacterAffiliation, Esi, EsiError, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

use crate::states::{StateError, evaluate_account};

pub const AFFILIATION_SYNC_JOB: &str = "affiliation.sync";
/// ESI caches affiliations for an hour, so syncing more often gains nothing.
const EVERY: Duration = Duration::from_secs(60 * 60);
/// Where the last run's summary is kept for the dashboard.
pub const LAST_RUN_SETTING: &str = "sync.affiliation.last";
/// ESI's per-request limit for affiliations.
const BATCH: usize = 1000;

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        AFFILIATION_SYNC_JOB,
        AFFILIATION_SYNC_JOB,
        EVERY,
    )]
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SyncSummary {
    pub characters: usize,
    /// Ids ESI rejected (deleted characters and the like).
    pub skipped: Vec<i64>,
    pub accounts: usize,
    pub state_changes: usize,
    pub duration_ms: u64,
}

pub async fn affiliation_sync(db: &PgPool, esi: &Esi) -> Result<SyncSummary, StateError> {
    let started = Instant::now();
    let ids = db::all_character_ids(db).await?;
    let (fetched, skipped) = fetch(esi, &ids).await?;
    let fresh: Vec<(i64, Affiliation)> = fetched
        .into_iter()
        .map(|a| {
            (
                a.character_id,
                Affiliation {
                    corporation_id: a.corporation_id,
                    alliance_id: a.alliance_id,
                    faction_id: a.faction_id,
                },
            )
        })
        .collect();
    db::update_affiliations(db, &fresh).await?;

    let accounts = tether_db::accounts::all_ids(db).await?;
    let mut state_changes = 0;
    for account in &accounts {
        if evaluate_account(db, *account).await?.changed() {
            state_changes += 1;
        }
    }
    let summary = SyncSummary {
        characters: ids.len(),
        skipped,
        accounts: accounts.len(),
        state_changes,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let mut record = serde_json::to_value(&summary).unwrap_or_default();
    record["at"] = chrono::Utc::now().to_rfc3339().into();
    tether_db::settings::set(db, LAST_RUN_SETTING, record).await?;
    tracing::info!(
        characters = summary.characters,
        skipped = summary.skipped.len(),
        state_changes = summary.state_changes,
        duration_ms = summary.duration_ms,
        "affiliation sync done"
    );
    Ok(summary)
}

/// Fetches affiliations in batches. ESI rejects a whole batch if any id is
/// invalid, so a rejected batch is split in half until the bad ids are
/// isolated; those are skipped rather than stopping everyone's sync.
async fn fetch(esi: &Esi, ids: &[i64]) -> Result<(Vec<CharacterAffiliation>, Vec<i64>), EsiError> {
    let mut fetched = Vec::with_capacity(ids.len());
    let mut skipped = Vec::new();
    let mut pending: Vec<Vec<i64>> = ids.chunks(BATCH).map(<[i64]>::to_vec).collect();
    while let Some(batch) = pending.pop() {
        match esi.affiliations(&batch, Priority::Bulk).await {
            Ok(found) => fetched.extend(found),
            Err(EsiError::Status(400 | 404)) if batch.len() > 1 => {
                let (left, right) = batch.split_at(batch.len() / 2);
                pending.push(left.to_vec());
                pending.push(right.to_vec());
            }
            Err(EsiError::Status(400 | 404)) => {
                tracing::warn!(
                    character_id = batch[0],
                    "ESI doesn't know this character; skipped"
                );
                skipped.extend(batch);
            }
            Err(err) => return Err(err),
        }
    }
    skipped.sort_unstable();
    Ok((fetched, skipped))
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, esi: Esi) {
    registry.register(AFFILIATION_SYNC_JOB, move |_job| {
        let (db, esi) = (db.clone(), esi.clone());
        async move {
            affiliation_sync(&db, &esi)
                .await
                .map(|_| ())
                .map_err(JobError::retry)
        }
    });
}
