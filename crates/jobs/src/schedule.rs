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
/// existing `next_run_at` is kept, so restarts don't reset the clock, but
/// never left further off than one new interval: a shortened schedule
/// runs on its new clock at once.
pub async fn ensure(pool: &PgPool, spec: &ScheduleSpec) -> Result<(), sqlx::Error> {
    let every = i32::try_from(spec.every.as_secs().max(1)).unwrap_or(i32::MAX);
    sqlx::query!(
        r#"
        INSERT INTO core.schedules (name, kind, payload, every_secs)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (name) DO UPDATE
        SET kind = EXCLUDED.kind, payload = EXCLUDED.payload,
            every_secs = EXCLUDED.every_secs, updated_at = now(),
            next_run_at = LEAST(
                core.schedules.next_run_at,
                now() + make_interval(secs => EXCLUDED.every_secs)
            )
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
        SELECT name, kind, payload, every_secs, next_run_at,
               EXISTS (SELECT 1 FROM core.jobs j
                       WHERE j.schedule = s.name AND j.state IN ('queued', 'running')) AS "busy!"
        FROM core.schedules s
        WHERE enabled AND next_run_at <= now()
          -- A plugin schedule outlives its plugin only by mistake; skip it
          -- rather than fail every schedule's tick.
          AND (s.payload->>'plugin' IS NULL
               OR EXISTS (SELECT 1 FROM core.plugins p WHERE p.id = s.payload->>'plugin'))
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
            r#"
            INSERT INTO core.jobs (kind, payload, schedule, plugin_id, scheduled_at)
            VALUES ($1, $2, $3, $2::jsonb ->> 'plugin', $4)
            "#,
            s.kind,
            s.payload,
            s.name,
            s.next_run_at,
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

/// What asking a schedule to run now did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunNow {
    /// Its job is queued.
    Queued,
    /// Its previous run is still queued or running.
    Busy,
    /// It was queued less than [`RUN_NOW_GAP`] ago.
    TooSoon,
    /// No such schedule, or it's switched off (its plugin disabled).
    Off,
}

/// How often an admin may run one schedule by hand: a run that just
/// happened has done its work, and a plugin's jobs spend ESI budget.
pub const RUN_NOW_GAP: Duration = Duration::from_secs(60);

/// Queues a schedule's job now, as its next tick would, and restarts its
/// interval from now. Only that schedule: a run already queued isn't
/// doubled (nor a running one of Tether's own; an app's gets one more
/// queued behind it), and it runs by hand at most once per
/// [`RUN_NOW_GAP`].
pub async fn run_now(pool: &PgPool, name: &str) -> Result<RunNow, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let outcome = run_now_in(&mut tx, name, RUN_NOW_GAP).await?;
    if outcome == RunNow::Queued {
        tx.commit().await?;
    }
    Ok(outcome)
}

/// [`run_now`] in the caller's transaction, with its own gap: not if it
/// was queued less than `gap` ago. Nothing is queued until the caller
/// commits, so what it records about the run (its audit) goes with it.
pub async fn run_now_in(
    tx: &mut sqlx::PgConnection,
    name: &str,
    gap: Duration,
) -> Result<RunNow, sqlx::Error> {
    let found = sqlx::query!(
        r#"
        SELECT kind, payload, every_secs, enabled,
               COALESCE(last_enqueued_at > now() - make_interval(secs => $2), false) AS "too_soon!",
               -- Queued already; or running, for Tether's own schedules.
               -- An app's running one gets one run queued behind it: apps
               -- run one job at a time, and the running one may have
               -- read before what prompted this.
               EXISTS (SELECT 1 FROM core.jobs j
                       WHERE j.schedule = s.name
                         AND (j.state = 'queued'
                              OR (j.state = 'running' AND s.payload->>'plugin' IS NULL))) AS "busy!"
        FROM core.schedules s
        WHERE name = $1
          AND (s.payload->>'plugin' IS NULL
               OR EXISTS (SELECT 1 FROM core.plugins p WHERE p.id = s.payload->>'plugin'))
        FOR UPDATE
        "#,
        name,
        gap.as_secs_f64(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(s) = found else {
        return Ok(RunNow::Off);
    };
    if !s.enabled {
        return Ok(RunNow::Off);
    }
    if s.busy {
        return Ok(RunNow::Busy);
    }
    if s.too_soon {
        return Ok(RunNow::TooSoon);
    }
    sqlx::query!(
        r#"
        INSERT INTO core.jobs (kind, payload, schedule, plugin_id)
        VALUES ($1, $2, $3, $2::jsonb ->> 'plugin')
        "#,
        s.kind,
        s.payload,
        name,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        UPDATE core.schedules
        SET last_enqueued_at = now(), next_run_at = now() + make_interval(secs => $2)
        WHERE name = $1
        "#,
        name,
        f64::from(s.every_secs),
    )
    .execute(&mut *tx)
    .await?;
    Ok(RunNow::Queued)
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

/// A schedule's latest job, and how it went.
#[derive(Debug, Clone)]
pub struct LastRun {
    pub schedule: String,
    pub state: crate::JobState,
    pub run_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
}

/// Each schedule's latest job still kept (succeeded ones go after
/// [`prune_succeeded`]'s week), for System's Health page.
pub async fn last_runs(pool: &PgPool) -> Result<Vec<LastRun>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT DISTINCT ON (schedule)
            schedule AS "schedule!", state, run_at, finished_at, last_error
        FROM core.jobs
        WHERE schedule IS NOT NULL
        ORDER BY schedule, id DESC
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(LastRun {
                state: crate::JobState::parse(&r.state)?,
                schedule: r.schedule,
                run_at: r.run_at,
                finished_at: r.finished_at,
                last_error: r.last_error,
            })
        })
        .collect())
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
