//! Housekeeping on a schedule: expired sessions, login attempts, setup
//! sessions and Discord link attempts, plugin uploads nobody approved, old
//! plugin log lines, plugin data access and HTTP logs, and succeeded jobs
//! older than a week.

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
    let discord_links = tether_db::discord::prune_attempts(db).await?;
    let plugin_uploads =
        tether_db::plugins::prune_uploads(db, crate::plugins::UPLOAD_HOURS).await?;
    let plugin_logs = tether_db::plugin_jobs::prune_logs(db, crate::plugin_jobs::KEEP_LOGS).await?;
    let plugin_access = tether_db::plugin_esi::prune_access_log(
        db,
        crate::plugin_services::ACCESS_LOG_DAYS,
        crate::plugin_services::ACCESS_LOG_KEEP,
    )
    .await?;
    let plugin_http = tether_db::plugin_http::prune_log(
        db,
        crate::plugin_http::LOG_DAYS,
        crate::plugin_http::LOG_KEEP,
    )
    .await?;
    let plugin_jobs =
        tether_db::plugin_jobs::prune_finished(db, crate::plugin_jobs::KEEP_FINISHED_HOURS).await?;
    let jobs = tether_jobs::schedule::prune_succeeded(db, KEEP_SUCCEEDED).await?;
    let access_tokens = tether_db::personal_tokens::prune(db).await?;
    tracing::info!(
        sessions = expired.sessions,
        login_attempts = expired.login_attempts,
        setup_sessions = expired.setup_sessions,
        discord_links,
        plugin_uploads,
        plugin_logs,
        plugin_access,
        plugin_http,
        plugin_jobs,
        jobs,
        access_tokens,
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
