//! Plugins' jobs on the core queue, their declared schedules, and their
//! logs. Plugin jobs are `core.jobs` rows of kind [`KIND`] with
//! `plugin_id` set; schedules are `core.schedules` rows named
//! `plugin:<id>:<name>`.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::PgPool;

/// The job kind every plugin job has.
pub const KIND: &str = "plugin.job";

/// A declared schedule's row name.
pub fn schedule_name(plugin_id: &str, name: &str) -> String {
    format!("plugin:{plugin_id}:{name}")
}

fn schedule_prefix(plugin_id: &str) -> String {
    format!("plugin:{plugin_id}:")
}

/// The payload of a plugin job: whose it is, what it's called, its key,
/// and the plugin's own data.
pub fn payload(plugin_id: &str, name: &str, key: Option<&str>, data: &Value) -> Value {
    json!({ "plugin": plugin_id, "name": name, "key": key, "data": data })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Queued {
    Done,
    /// The plugin is at its limit of queued and running jobs, or of jobs
    /// created in a day.
    TooMany,
    /// The plugin isn't installed (any more).
    Gone,
}

/// Jobs one plugin may create in 24 hours, finished ones included: bounds
/// how fast its history grows between prunes.
pub const MAX_PER_DAY: i64 = 5_000;

/// Queues a plugin job; with a key, replaces the queued job with that key
/// (its retries start over). Refuses past `max_active` queued and running
/// jobs, unless it only replaces one, and past [`MAX_PER_DAY`] created.
///
/// A plugin may ask for any time; a past one runs now. `run_at` (which
/// orders the queue) is never earlier than now, so a plugin can't jump
/// ahead of work already waiting; `scheduled_at` keeps what it asked for.
pub async fn enqueue(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    key: Option<&str>,
    data: &Value,
    run_at: Option<DateTime<Utc>>,
    max_active: i64,
) -> Result<Queued, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // One enqueue per plugin at a time, so the caps hold under concurrent
    // calls; and the plugin must still exist.
    let exists = sqlx::query_scalar!(
        r#"SELECT true AS "exists!" FROM core.plugins WHERE id = $1 FOR UPDATE"#,
        plugin_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Ok(Queued::Gone);
    }
    let replaces = match key {
        Some(key) => {
            sqlx::query_scalar!(
                r#"
            SELECT EXISTS (SELECT 1 FROM core.jobs
                WHERE plugin_id = $1 AND job_key = $2 AND state = 'queued') AS "exists!"
            "#,
                plugin_id,
                key
            )
            .fetch_one(&mut *tx)
            .await?
        }
        None => false,
    };
    // Replacing a queued job creates no row, so neither cap applies to it.
    if replaces {
        upsert(&mut tx, plugin_id, name, key, data, run_at).await?;
        tx.commit().await?;
        return Ok(Queued::Done);
    }
    // Jobs the plugin queued itself: its schedules' runs don't count.
    let today = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "n!" FROM core.jobs
        WHERE plugin_id = $1 AND schedule IS NULL AND created_at > now() - interval '1 day'
        "#,
        plugin_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if today >= MAX_PER_DAY {
        return Ok(Queued::TooMany);
    }
    let active = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "n!" FROM core.jobs
        WHERE plugin_id = $1 AND state IN ('queued', 'running')
        "#,
        plugin_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if active >= max_active {
        return Ok(Queued::TooMany);
    }
    upsert(&mut tx, plugin_id, name, key, data, run_at).await?;
    tx.commit().await?;
    Ok(Queued::Done)
}

/// The insert (or keyed replace) itself.
async fn upsert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    plugin_id: &str,
    name: &str,
    key: Option<&str>,
    data: &Value,
    run_at: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.jobs (kind, payload, plugin_id, job_key, run_at, scheduled_at)
        VALUES ($1, $2, $3, $4, GREATEST(COALESCE($5, now()), now()), COALESCE($5, now()))
        ON CONFLICT (plugin_id, job_key) WHERE state = 'queued' AND job_key IS NOT NULL
        DO UPDATE SET payload = EXCLUDED.payload, run_at = EXCLUDED.run_at,
            scheduled_at = EXCLUDED.scheduled_at, attempts = 0, last_error = NULL,
            updated_at = now()
        "#,
        KIND,
        payload(plugin_id, name, key, data),
        plugin_id,
        key,
        run_at,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Removes the queued job with this key; whether there was one.
pub async fn cancel(pool: &PgPool, plugin_id: &str, key: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        "DELETE FROM core.jobs WHERE plugin_id = $1 AND job_key = $2 AND state = 'queued'",
        plugin_id,
        key
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Makes the plugin's schedules exactly `schedules` (name, interval in
/// seconds), switched on. Existing ones keep their next run.
pub async fn sync_schedules(
    pool: &PgPool,
    plugin_id: &str,
    schedules: &[(String, i32)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let names: Vec<String> = schedules
        .iter()
        .map(|(name, _)| schedule_name(plugin_id, name))
        .collect();
    sqlx::query!(
        "DELETE FROM core.schedules WHERE starts_with(name, $1) AND NOT (name = ANY($2))",
        schedule_prefix(plugin_id),
        &names,
    )
    .execute(&mut *tx)
    .await?;
    for ((name, every), row) in schedules.iter().zip(&names) {
        sqlx::query!(
            r#"
            INSERT INTO core.schedules (name, kind, payload, every_secs, enabled)
            VALUES ($1, $2, $3, $4, true)
            ON CONFLICT (name) DO UPDATE
            SET kind = EXCLUDED.kind, payload = EXCLUDED.payload,
                every_secs = EXCLUDED.every_secs, enabled = true, updated_at = now()
            "#,
            row,
            KIND,
            payload(plugin_id, name, None, &json!({})),
            every,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn set_schedules_enabled<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.schedules SET enabled = $2, updated_at = now() WHERE starts_with(name, $1)",
        schedule_prefix(plugin_id),
        enabled,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Removes a plugin's schedules and every job of it, finished or not.
pub async fn remove(tx: &mut sqlx::PgConnection, plugin_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM core.schedules WHERE starts_with(name, $1)",
        schedule_prefix(plugin_id)
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM core.jobs WHERE plugin_id = $1", plugin_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub name: String,
    pub every_secs: i32,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
    pub last_enqueued_at: Option<DateTime<Utc>>,
}

pub async fn schedules(pool: &PgPool, plugin_id: &str) -> Result<Vec<ScheduleRow>, sqlx::Error> {
    let prefix = schedule_prefix(plugin_id);
    let rows = sqlx::query!(
        r#"
        SELECT name, every_secs, enabled, next_run_at, last_enqueued_at
        FROM core.schedules WHERE starts_with(name, $1) ORDER BY name
        "#,
        prefix
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ScheduleRow {
            name: r.name.strip_prefix(&prefix).unwrap_or(&r.name).to_owned(),
            every_secs: r.every_secs,
            enabled: r.enabled,
            next_run_at: r.next_run_at,
            last_enqueued_at: r.last_enqueued_at,
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub name: String,
    pub key: Option<String>,
    pub state: String,
    pub attempts: i32,
    pub run_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

/// Queued and running jobs, soonest first, and recent dead ones, newest
/// first; `limit` of each.
pub async fn jobs(
    pool: &PgPool,
    plugin_id: &str,
    limit: i64,
) -> Result<(i64, Vec<JobRow>, Vec<JobRow>), sqlx::Error> {
    let active = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.jobs WHERE plugin_id = $1 AND state IN ('queued', 'running')"#,
        plugin_id
    )
    .fetch_one(pool)
    .await?;
    let upcoming = sqlx::query!(
        r#"
        SELECT id, payload->>'name' AS "name?", job_key, state, attempts, run_at, last_error
        FROM core.jobs WHERE plugin_id = $1 AND state IN ('queued', 'running')
        ORDER BY run_at, id LIMIT $2
        "#,
        plugin_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    let dead = sqlx::query!(
        r#"
        SELECT id, payload->>'name' AS "name?", job_key, state, attempts, run_at, last_error
        FROM core.jobs WHERE plugin_id = $1 AND state = 'dead'
        ORDER BY id DESC LIMIT $2
        "#,
        plugin_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    macro_rules! rows {
        ($rows:expr) => {
            $rows
                .into_iter()
                .map(|r| JobRow {
                    id: r.id,
                    name: r.name.unwrap_or_default(),
                    key: r.job_key,
                    state: r.state,
                    attempts: r.attempts,
                    run_at: r.run_at,
                    last_error: r.last_error,
                })
                .collect()
        };
    }
    Ok((active, rows!(upcoming), rows!(dead)))
}

/// One line a plugin logged.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: &'static str,
    pub message: String,
}

/// Removes a plugin's finished jobs older than `hours`.
pub async fn prune_finished(pool: &PgPool, hours: i32) -> Result<u64, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        DELETE FROM core.jobs
        WHERE plugin_id IS NOT NULL AND state IN ('succeeded', 'dead')
          AND finished_at < now() - make_interval(hours => $1)
        "#,
        hours
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Records log lines and trims the plugin's log to its newest `keep`.
pub async fn record_logs(
    pool: &PgPool,
    plugin_id: &str,
    source: &str,
    lines: &[LogLine],
    keep: i64,
) -> Result<(), sqlx::Error> {
    if lines.is_empty() {
        return Ok(());
    }
    let levels: Vec<&str> = lines.iter().map(|l| l.level).collect();
    let messages: Vec<&str> = lines.iter().map(|l| l.message.as_str()).collect();
    sqlx::query!(
        r#"
        INSERT INTO core.plugin_logs (plugin_id, source, level, message)
        SELECT $1, $2, level, message FROM UNNEST($3::text[], $4::text[]) AS l (level, message)
        "#,
        plugin_id,
        source,
        &levels as &[&str],
        &messages as &[&str],
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        r#"
        DELETE FROM core.plugin_logs
        WHERE plugin_id = $1 AND id <= (
            SELECT id FROM core.plugin_logs WHERE plugin_id = $1
            ORDER BY id DESC OFFSET $2 LIMIT 1
        )
        "#,
        plugin_id,
        keep,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LogRow {
    pub at: DateTime<Utc>,
    pub level: String,
    pub source: String,
    pub message: String,
}

/// Newest first.
pub async fn logs(pool: &PgPool, plugin_id: &str, limit: i64) -> Result<Vec<LogRow>, sqlx::Error> {
    sqlx::query_as!(
        LogRow,
        r#"
        SELECT at, level, source, message FROM core.plugin_logs
        WHERE plugin_id = $1 ORDER BY id DESC LIMIT $2
        "#,
        plugin_id,
        limit
    )
    .fetch_all(pool)
    .await
}

/// Keeps the newest `keep` log lines of each plugin.
pub async fn prune_logs(pool: &PgPool, keep: i64) -> Result<u64, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        DELETE FROM core.plugin_logs l
        USING (
            SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY plugin_id ORDER BY id DESC) AS n
                FROM core.plugin_logs
            ) ranked WHERE n > $1
        ) old
        WHERE l.id = old.id
        "#,
        keep
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}
