//! Plugin jobs on the core queue: what `enqueue` and `cancel` do, the
//! handler that hands a due job to its plugin, and plugin logs.
//!
//! Plugin jobs have workers of their own, so however many a plugin queues,
//! core jobs keep theirs. A job whose plugin isn't running (disabled, or
//! still loading after a restart) waits for it without using attempts;
//! uninstalling a plugin removes its jobs and schedules.

use std::sync::Arc;
use std::time::Duration;

use tether_db::PgPool;
use tether_db::plugin_jobs::{self as db, LogLine};
use tether_jobs::{JobError, Registry};
use tether_plugins::PluginLimits;
use tether_plugins::host::{Level, LogRecord};
use tether_plugins::jobs::{self, Error, JobQueue, QueueFuture, Queued};

use crate::plugins::Plugins;

/// Log lines kept per plugin.
pub const KEEP_LOGS: i64 = 1_000;
/// Finished plugin jobs are kept this long for the admin page.
pub const KEEP_FINISHED_HOURS: i32 = 24;
/// Workers for plugin jobs, apart from core's: however plugins behave,
/// core jobs always have workers of their own.
pub const WORKERS: usize = 2;
/// How long a job waits when its plugin isn't running (disabled, or still
/// loading after a restart). It isn't counted as an attempt.
pub const NOT_RUNNING_DELAY: Duration = Duration::from_secs(60);
/// How long a job waits when its plugin is already running a job; not an
/// attempt either. Other plugins' jobs go ahead meanwhile.
pub const BUSY_DELAY: Duration = Duration::from_secs(5);

/// A job gets more room than a page: syncing from ESI takes a while.
pub fn job_limits() -> PluginLimits {
    PluginLimits {
        memory_bytes: 64 * 1024 * 1024,
        cpu: Duration::from_secs(10),
        deadline: Duration::from_secs(60),
    }
}

/// The core queue, as plugins see it.
#[derive(Debug, Clone)]
pub struct PluginQueue {
    db: PgPool,
}

impl PluginQueue {
    pub fn new(db: PgPool) -> Arc<Self> {
        Arc::new(Self { db })
    }
}

fn unavailable(plugin: &str, err: sqlx::Error) -> Error {
    tracing::error!(plugin, error = %err, "plugin job queue");
    Error::Unavailable
}

impl JobQueue for PluginQueue {
    fn enqueue(&self, plugin: String, job: Queued) -> QueueFuture<()> {
        let db = self.db.clone();
        Box::pin(async move {
            let queued = db::enqueue(
                &db,
                &plugin,
                &job.name,
                job.key.as_deref(),
                &job.payload,
                job.run_at,
                jobs::MAX_QUEUED,
            )
            .await
            .map_err(|e| unavailable(&plugin, e))?;
            match queued {
                db::Queued::Done => Ok(()),
                db::Queued::TooMany => Err(Error::TooMany),
                db::Queued::Gone => Err(Error::Unavailable),
            }
        })
    }

    fn cancel(&self, plugin: String, key: String) -> QueueFuture<bool> {
        let db = self.db.clone();
        Box::pin(async move {
            db::cancel(&db, &plugin, &key)
                .await
                .map_err(|e| unavailable(&plugin, e))
        })
    }
}

fn level(level: Level) -> &'static str {
    match level {
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

/// Keeps what a plugin logged during a call, for its admin page. Failing
/// to is logged, not fatal: the call itself went fine.
pub async fn record_logs(db: &PgPool, plugin: &str, source: &str, logs: &[LogRecord]) {
    let lines: Vec<LogLine> = logs
        .iter()
        .map(|l| LogLine {
            level: level(l.level),
            message: l.message.clone(),
        })
        .collect();
    if let Err(err) = db::record_logs(db, plugin, source, &lines, KEEP_LOGS).await {
        tracing::error!(plugin, error = %err, "recording plugin logs");
    }
}

/// Registers the handler for plugin jobs.
pub fn register_jobs(registry: &mut Registry, db: PgPool, plugins: Arc<Plugins>) {
    registry.register(db::KIND, move |job| {
        let (db, plugins) = (db.clone(), plugins.clone());
        async move { run(&db, &plugins, job).await }
    });
}

async fn run(db: &PgPool, plugins: &Plugins, job: tether_jobs::Job) -> Result<(), JobError> {
    let payload = &job.payload;
    let (Some(plugin_id), Some(name)) = (payload["plugin"].as_str(), payload["name"].as_str())
    else {
        return Err(JobError::permanent("not a plugin job"));
    };
    let Some(plugin) = plugins.get(plugin_id) else {
        return Err(JobError::Defer(NOT_RUNNING_DELAY));
    };
    let call = jobs::Job {
        name: name.to_owned(),
        key: payload["key"].as_str().map(str::to_owned),
        payload: payload["data"].to_string(),
        scheduled_at: job
            .scheduled_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        attempt: u32::try_from(job.attempt).unwrap_or(0),
    };
    let run = plugins.host().run_job(&plugin, call, &job_limits()).await;
    match run {
        Ok(run) => {
            record_logs(db, plugin_id, &format!("job:{name}"), &run.logs).await;
            match run.result {
                Ok(()) => Ok(()),
                Err(jobs::JobError::Retry(why)) => Err(JobError::Retry(why)),
                Err(jobs::JobError::Permanent(why)) => Err(JobError::Permanent(why)),
            }
        }
        Err(tether_plugins::CallError::Busy) => Err(JobError::Defer(BUSY_DELAY)),
        // Out of memory, CPU or time, or a trap: worth another go. Trap
        // text can carry the plugin's own names: clean it for admins.
        Err(err) => Err(JobError::Retry(tether_plugins::host::printable(
            &err.to_string(),
            tether_plugins::host::MAX_LOG_TEXT,
        ))),
    }
}

/// The schedules a manifest declares, for [`db::sync_schedules`].
pub fn declared(manifest: &tether_plugins::manifest::Manifest) -> Vec<(String, i32)> {
    manifest
        .capabilities
        .schedules
        .iter()
        .filter_map(|s| {
            let every = s.interval().ok()?.as_secs();
            Some((s.name.clone(), i32::try_from(every).ok()?))
        })
        .collect()
}
