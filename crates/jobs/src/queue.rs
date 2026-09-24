use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tether_db::PgPool;

/// Longest error message kept in `last_error`.
const MAX_ERROR_LEN: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub i64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Dead,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Dead => "dead",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "dead" => Some(Self::Dead),
            _ => None,
        }
    }
}

/// A job as handed to a handler.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: JobId,
    pub kind: String,
    pub payload: Value,
    /// 1 on the first run.
    pub attempt: i32,
    pub max_attempts: i32,
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub payload: Value,
    pub max_attempts: i32,
    /// `None` runs as soon as a worker is free.
    pub run_at: Option<DateTime<Utc>>,
}

impl NewJob {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
            max_attempts: 5,
            run_at: None,
        }
    }

    pub fn max_attempts(mut self, max_attempts: i32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    pub fn run_at(mut self, run_at: DateTime<Utc>) -> Self {
        self.run_at = Some(run_at);
        self
    }
}

pub async fn enqueue<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    job: NewJob,
) -> Result<JobId, sqlx::Error> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO core.jobs (kind, payload, max_attempts, run_at)
        VALUES ($1, $2, $3, COALESCE($4, now()))
        RETURNING id
        "#,
        job.kind,
        job.payload,
        job.max_attempts,
        job.run_at,
    )
    .fetch_one(executor)
    .await?;
    tracing::debug!(job.id = id, job.kind = job.kind, "job enqueued");
    Ok(JobId(id))
}

/// Claims the next runnable job of one of `kinds`, or a running job whose
/// lease has expired. Increments `attempts`; the new value identifies this
/// claim when recording the outcome.
pub(crate) async fn claim(
    pool: &PgPool,
    kinds: &[String],
    lease: Duration,
) -> Result<Option<Job>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        UPDATE core.jobs
        SET state = 'running',
            attempts = attempts + 1,
            locked_until = now() + make_interval(secs => $2),
            updated_at = now()
        WHERE id = (
            SELECT id FROM core.jobs
            WHERE kind = ANY($1)
              AND ((state = 'queued' AND run_at <= now())
                OR (state = 'running' AND locked_until < now()))
            ORDER BY run_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        RETURNING id, kind, payload, attempts, max_attempts
        "#,
        kinds,
        lease.as_secs_f64(),
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Job {
        id: JobId(r.id),
        kind: r.kind,
        payload: r.payload,
        attempt: r.attempts,
        max_attempts: r.max_attempts,
    }))
}

/// Records success. Returns false if this claim no longer owns the job
/// (its lease expired and another worker reclaimed it).
pub(crate) async fn complete(pool: &PgPool, job: &Job) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.jobs
        SET state = 'succeeded', locked_until = NULL, last_error = NULL,
            finished_at = now(), updated_at = now()
        WHERE id = $1 AND state = 'running' AND attempts = $2
        "#,
        job.id.0,
        job.attempt,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Records a failure: back to `queued` after `retry_in`, or `dead` when
/// attempts are used up or the failure is permanent. Returns the new state,
/// or `None` if this claim no longer owns the job.
pub(crate) async fn fail(
    pool: &PgPool,
    job: &Job,
    error: &str,
    permanent: bool,
    retry_in: Duration,
) -> Result<Option<JobState>, sqlx::Error> {
    let error = truncate(error, MAX_ERROR_LEN);
    let state = sqlx::query_scalar!(
        r#"
        UPDATE core.jobs
        SET state = CASE WHEN $3 OR attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
            run_at = now() + make_interval(secs => $4),
            finished_at = CASE WHEN $3 OR attempts >= max_attempts THEN now() END,
            locked_until = NULL,
            last_error = $5,
            updated_at = now()
        WHERE id = $1 AND state = 'running' AND attempts = $2
        RETURNING state
        "#,
        job.id.0,
        job.attempt,
        permanent,
        retry_in.as_secs_f64(),
        error,
    )
    .fetch_optional(pool)
    .await?;
    Ok(state.as_deref().and_then(JobState::parse))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "tether_db::MIGRATOR")]
    async fn stale_claim_cannot_record_an_outcome(pool: PgPool) {
        let kinds = vec!["work".to_owned()];
        enqueue(&pool, NewJob::new("work", Value::Null))
            .await
            .unwrap();
        let lease = Duration::from_millis(1);
        let stale = claim(&pool, &kinds, lease).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let current = claim(&pool, &kinds, Duration::from_secs(60))
            .await
            .unwrap()
            .expect("expired lease is claimable");
        assert_eq!((stale.id, current.attempt), (current.id, 2));

        assert!(!complete(&pool, &stale).await.unwrap());
        assert_eq!(
            fail(&pool, &stale, "late", false, Duration::ZERO)
                .await
                .unwrap(),
            None
        );
        assert!(complete(&pool, &current).await.unwrap());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("aé", 2), "a");
    }
}
