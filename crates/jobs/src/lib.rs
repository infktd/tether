//! Postgres-backed job queue and workers.
//!
//! Jobs are rows in `core.jobs`. A worker claims one with
//! `FOR UPDATE SKIP LOCKED`, holds a lease while it runs, and records the
//! outcome. Failures retry with exponential backoff until `max_attempts`,
//! then the job is dead-lettered (`state = 'dead'`) for an admin to inspect.

mod queue;
pub mod schedule;
mod worker;

pub use queue::{Job, JobId, JobState, JobSummary, NewJob, counts, enqueue, list, retry};
pub use worker::{JobError, Outcome, Registry, WorkerConfig, WorkerPool, run_once};
