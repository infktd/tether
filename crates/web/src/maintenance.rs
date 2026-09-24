//! Housekeeping on a schedule: expired sessions, login attempts and setup
//! sessions, and succeeded jobs older than a week.

use std::time::Duration;

use tether_db::PgPool;
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

pub const PRUNE_JOB: &str = "maintenance.prune";
/// Succeeded jobs are kept this long for the dashboard; dead ones stay.
const KEEP_SUCCEEDED: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        "maintenance.prune",
        PRUNE_JOB,
        Duration::from_secs(60 * 60),
    )]
}

pub async fn prune(db: &PgPool) -> Result<(), sqlx::Error> {
    let expired = tether_db::auth::prune_expired(db).await?;
    let jobs = tether_jobs::schedule::prune_succeeded(db, KEEP_SUCCEEDED).await?;
    tracing::info!(
        sessions = expired.sessions,
        login_attempts = expired.login_attempts,
        setup_sessions = expired.setup_sessions,
        jobs,
        "pruned"
    );
    Ok(())
}

pub fn register_jobs(registry: &mut Registry, db: PgPool) {
    registry.register(PRUNE_JOB, move |_job| {
        let db = db.clone();
        async move { prune(&db).await.map_err(JobError::retry) }
    });
}
