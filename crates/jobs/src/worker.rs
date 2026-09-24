use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tether_db::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::queue::{self, Job, JobId, JobState};

/// Why a handler failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// Worth trying again later (ESI down, Discord rate limit, ...).
    Retry(String),
    /// Will never succeed (bad payload, deleted target): dead-letter now.
    Permanent(String),
}

impl JobError {
    pub fn retry(message: impl fmt::Display) -> Self {
        Self::Retry(message.to_string())
    }

    pub fn permanent(message: impl fmt::Display) -> Self {
        Self::Permanent(message.to_string())
    }
}

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send>>;
type Handler = Arc<dyn Fn(Job) -> HandlerFuture + Send + Sync>;

/// Maps job kinds to handlers. Workers only claim kinds registered here, so
/// jobs of an unknown kind wait rather than failing.
#[derive(Clone, Default)]
pub struct Registry {
    handlers: HashMap<String, Handler>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F, Fut>(&mut self, kind: impl Into<String>, handler: F) -> &mut Self
    where
        F: Fn(Job) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), JobError>> + Send + 'static,
    {
        self.handlers.insert(
            kind.into(),
            Arc::new(move |job| Box::pin(handler(job)) as HandlerFuture),
        );
        self
    }

    fn kinds(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub workers: usize,
    /// How long an idle worker waits before looking for work again.
    pub poll_interval: Duration,
    /// How long a claimed job may run. A job still running after this is
    /// cancelled and retried; if its worker died, another worker may claim
    /// it once the lease expires.
    pub lease: Duration,
    /// Delay before the first retry; doubles on each attempt.
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            workers: 4,
            poll_interval: Duration::from_secs(1),
            lease: Duration::from_secs(300),
            backoff_base: Duration::from_secs(10),
            backoff_max: Duration::from_secs(3600),
        }
    }
}

impl WorkerConfig {
    fn backoff(&self, attempt: i32) -> Duration {
        let exponent = u32::try_from(attempt.saturating_sub(1))
            .unwrap_or(0)
            .min(20);
        self.backoff_base
            .saturating_mul(1 << exponent)
            .min(self.backoff_max)
    }
}

/// What a single `run_once` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was ready to run.
    Idle,
    Succeeded(JobId),
    Retrying(JobId),
    Dead(JobId),
    /// The job outlived its lease and was reclaimed by another worker, so
    /// this outcome was discarded.
    LostLease(JobId),
}

/// Claims and runs at most one job.
pub async fn run_once(
    pool: &PgPool,
    registry: &Registry,
    config: &WorkerConfig,
) -> Result<Outcome, sqlx::Error> {
    let kinds = registry.kinds();
    if kinds.is_empty() {
        return Ok(Outcome::Idle);
    }
    let Some(job) = queue::claim(pool, &kinds, config.lease).await? else {
        return Ok(Outcome::Idle);
    };
    let span = tracing::info_span!(
        "job",
        job.id = job.id.0,
        job.kind = %job.kind,
        job.attempt = job.attempt
    );
    execute(pool, registry, config, job).instrument(span).await
}

async fn execute(
    pool: &PgPool,
    registry: &Registry,
    config: &WorkerConfig,
    job: Job,
) -> Result<Outcome, sqlx::Error> {
    // Reclaimed after its worker died on the final attempt.
    if job.attempt > job.max_attempts {
        let reason = "lease expired on the final attempt";
        return record_failure(pool, config, &job, reason, true).await;
    }
    let Some(handler) = registry.handlers.get(&job.kind) else {
        // Unreachable: claim only returns registered kinds.
        return record_failure(pool, config, &job, "no handler registered", false).await;
    };

    let started = Instant::now();
    let mut task = tokio::spawn(handler(job.clone()));
    let result = match tokio::time::timeout(config.lease, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_error)) if join_error.is_panic() => Err(JobError::retry("handler panicked")),
        Ok(Err(_)) => Err(JobError::retry("handler was cancelled")),
        Err(_) => {
            task.abort();
            Err(JobError::retry(format!(
                "timed out after {}s",
                config.lease.as_secs()
            )))
        }
    };

    match result {
        Ok(()) => {
            if queue::complete(pool, &job).await? {
                tracing::info!(elapsed_ms = started.elapsed().as_millis(), "job succeeded");
                Ok(Outcome::Succeeded(job.id))
            } else {
                tracing::warn!("job finished after its lease was reclaimed; result discarded");
                Ok(Outcome::LostLease(job.id))
            }
        }
        Err(JobError::Retry(message)) => record_failure(pool, config, &job, &message, false).await,
        Err(JobError::Permanent(message)) => {
            record_failure(pool, config, &job, &message, true).await
        }
    }
}

async fn record_failure(
    pool: &PgPool,
    config: &WorkerConfig,
    job: &Job,
    message: &str,
    permanent: bool,
) -> Result<Outcome, sqlx::Error> {
    let retry_in = config.backoff(job.attempt);
    match queue::fail(pool, job, message, permanent, retry_in).await? {
        Some(JobState::Dead) => {
            tracing::error!(error = message, "job dead-lettered");
            Ok(Outcome::Dead(job.id))
        }
        Some(_) => {
            tracing::warn!(
                error = message,
                retry_in_s = retry_in.as_secs(),
                "job failed, will retry"
            );
            Ok(Outcome::Retrying(job.id))
        }
        None => {
            tracing::warn!(error = message, "job failed after its lease was reclaimed");
            Ok(Outcome::LostLease(job.id))
        }
    }
}

/// Worker tasks running inside the host process.
pub struct WorkerPool {
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    pub fn start(pool: PgPool, registry: Registry, config: WorkerConfig) -> Self {
        let (shutdown, _) = watch::channel(false);
        let registry = Arc::new(registry);
        let config = Arc::new(config);
        let tasks = (0..config.workers.max(1))
            .map(|worker| {
                let pool = pool.clone();
                let registry = Arc::clone(&registry);
                let config = Arc::clone(&config);
                let shutdown = shutdown.subscribe();
                tokio::spawn(
                    work(pool, registry, config, shutdown)
                        .instrument(tracing::info_span!("worker", worker)),
                )
            })
            .collect();
        tracing::info!(workers = config.workers.max(1), "job workers started");
        Self { shutdown, tasks }
    }

    /// Stops claiming new jobs and waits for running ones to finish.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            if let Err(err) = task.await {
                tracing::error!(error = %err, "job worker ended abnormally");
            }
        }
        tracing::info!("job workers stopped");
    }
}

async fn work(
    pool: PgPool,
    registry: Arc<Registry>,
    config: Arc<WorkerConfig>,
    mut shutdown: watch::Receiver<bool>,
) {
    while !*shutdown.borrow() {
        let idle = match run_once(&pool, &registry, &config).await {
            Ok(Outcome::Idle) => true,
            Ok(_) => false,
            Err(err) => {
                tracing::warn!(error = %err, "job queue unavailable");
                true
            }
        };
        if idle {
            tokio::select! {
                () = tokio::time::sleep(config.poll_interval) => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let config = WorkerConfig {
            backoff_base: Duration::from_secs(10),
            backoff_max: Duration::from_secs(60),
            ..WorkerConfig::default()
        };
        assert_eq!(config.backoff(1), Duration::from_secs(10));
        assert_eq!(config.backoff(2), Duration::from_secs(20));
        assert_eq!(config.backoff(3), Duration::from_secs(40));
        assert_eq!(config.backoff(4), Duration::from_secs(60));
        assert_eq!(config.backoff(100), Duration::from_secs(60));
        assert_eq!(config.backoff(0), Duration::from_secs(10));
    }
}
