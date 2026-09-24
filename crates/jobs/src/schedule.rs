//! Recurring jobs on the Postgres queue.
//!
//! Schedules live in `core.schedules`; code declares them at startup with
//! [`ensure`]. A [`Scheduler`] task enqueues a job whenever one is due.
//! Schedules run at fixed intervals; plugin manifests declare theirs the
//! same way (`every = "30m"`) and reuse the same table.

use std::time::Duration;

use serde_json::Value;
use tether_db::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// A schedule as declared in code.
#[derive(Debug, Clone)]
pub struct ScheduleSpec {
    pub name: String,
    /// The job kind to enqueue.
    pub kind: String,
    pub payload: Value,
    pub every: Duration,
}

impl ScheduleSpec {
    pub fn new(name: impl Into<String>, kind: impl Into<String>, every: Duration) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            payload: Value::Object(Default::default()),
            every,
        }
    }
}

/// Creates the schedule, or updates its kind, payload and interval. An
/// existing `next_run_at` is kept, so restarts don't reset the clock.
pub async fn ensure(pool: &PgPool, spec: &ScheduleSpec) -> Result<(), sqlx::Error> {
    let every = i32::try_from(spec.every.as_secs().max(1)).unwrap_or(i32::MAX);
    sqlx::query!(
        r#"
        INSERT INTO core.schedules (name, kind, payload, every_secs)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (name) DO UPDATE
        SET kind = EXCLUDED.kind, payload = EXCLUDED.payload,
            every_secs = EXCLUDED.every_secs, updated_at = now()
        "#,
        spec.name,
        spec.kind,
        spec.payload,
        every,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Enqueues a job for every due, enabled schedule whose previous run isn't
/// still queued or running, and moves each due schedule's next run on by
/// its interval. Returns the names enqueued. Safe to call from several
/// processes at once.
pub async fn run_due(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let due = sqlx::query!(
        r#"
        SELECT name, kind, payload, every_secs,
               EXISTS (SELECT 1 FROM core.jobs j
                       WHERE j.schedule = s.name AND j.state IN ('queued', 'running')) AS "busy!"
        FROM core.schedules s
        WHERE enabled AND next_run_at <= now()
        ORDER BY next_run_at
        FOR UPDATE SKIP LOCKED
        "#
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut enqueued = Vec::new();
    for s in due {
        sqlx::query!(
            "UPDATE core.schedules SET next_run_at = now() + make_interval(secs => $2) WHERE name = $1",
            s.name,
            f64::from(s.every_secs),
        )
        .execute(&mut *tx)
        .await?;
        if s.busy {
            tracing::debug!(schedule = s.name, "previous run still in flight; skipping");
            continue;
        }
        sqlx::query!(
            "INSERT INTO core.jobs (kind, payload, schedule) VALUES ($1, $2, $3)",
            s.kind,
            s.payload,
            s.name,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE core.schedules SET last_enqueued_at = now() WHERE name = $1",
            s.name
        )
        .execute(&mut *tx)
        .await?;
        enqueued.push(s.name);
    }
    tx.commit().await?;
    if !enqueued.is_empty() {
        tracing::info!(schedules = ?enqueued, "scheduled jobs enqueued");
    }
    Ok(enqueued)
}

/// Calls [`run_due`] every `tick` until shut down.
pub struct Scheduler {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Scheduler {
    pub fn start(pool: PgPool, tick: Duration) -> Self {
        let (shutdown, mut stop) = watch::channel(false);
        let task = tokio::spawn(async move {
            while !*stop.borrow() {
                if let Err(err) = run_due(&pool).await {
                    tracing::warn!(error = %err, "scheduler couldn't check schedules");
                }
                tokio::select! {
                    () = tokio::time::sleep(tick) => {}
                    _ = stop.changed() => {}
                }
            }
        });
        Self { shutdown, task }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        if let Err(err) = self.task.await {
            tracing::error!(error = %err, "scheduler ended abnormally");
        }
    }
}

/// A schedule, for admins.
#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub name: String,
    pub kind: String,
    pub every_secs: i32,
    pub enabled: bool,
    pub next_run_at: chrono::DateTime<chrono::Utc>,
    pub last_enqueued_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list(pool: &PgPool) -> Result<Vec<ScheduleRow>, sqlx::Error> {
    sqlx::query_as!(
        ScheduleRow,
        r#"
        SELECT name, kind, every_secs, enabled, next_run_at, last_enqueued_at
        FROM core.schedules ORDER BY name
        "#
    )
    .fetch_all(pool)
    .await
}

/// Deletes succeeded jobs older than `keep`. Dead jobs stay for inspection.
pub async fn prune_succeeded(pool: &PgPool, keep: Duration) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.jobs WHERE state = 'succeeded' AND finished_at < now() - make_interval(secs => $1)",
        keep.as_secs_f64(),
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
