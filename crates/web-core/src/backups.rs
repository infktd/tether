//! Nightly encrypted backups (N7): the same snapshots as before migrations,
//! of core and every plugin with storage, kept apart from those (a week of
//! them per kind). `tether rollback --snapshot <name>` restores one.

use std::sync::Arc;
use std::time::Duration;

use tether_db::PgPool;
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};
use tether_snapshots::Snapshots;

pub const NIGHTLY_JOB: &str = "backups.nightly";

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        "backups.nightly",
        NIGHTLY_JOB,
        Duration::from_secs(24 * 60 * 60),
    )]
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, snapshots: Arc<Snapshots>) {
    registry.register(NIGHTLY_JOB, move |_job| {
        let (db, snapshots) = (db.clone(), snapshots.clone());
        async move {
            let taken = snapshots.back_up_all(&db).await.map_err(JobError::retry)?;
            tracing::info!(backups = taken.len(), "nightly backup done");
            Ok(())
        }
    });
}
