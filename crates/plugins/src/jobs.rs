//! Plugin jobs: what a plugin may queue, checked before it reaches the
//! queue. The queue itself (Tether's job table) is behind [`JobQueue`],
//! which the web crate implements.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use crate::host::tether::plugin::jobs::{Error, Job, JobError, NewJob};

/// Payload JSON per job.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// How far ahead a job may be queued.
pub const MAX_AHEAD: Duration = Duration::from_secs(120 * 24 * 60 * 60);
/// Queued jobs per plugin.
pub const MAX_QUEUED: i64 = 1_000;
/// `enqueue` and `cancel` calls in one plugin call.
pub const MAX_CALLS: usize = 100;
pub const MAX_KEY: usize = 100;

/// A job as the queue stores it: checked, with the run time parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub name: String,
    pub key: Option<String>,
    pub payload: serde_json::Value,
    pub run_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub type QueueFuture<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

/// Where plugins' jobs go. Implementations cap queued jobs per plugin at
/// [`MAX_QUEUED`] and answer `Error::TooMany` past it.
pub trait JobQueue: Send + Sync + std::fmt::Debug {
    fn enqueue(&self, plugin: String, job: Queued) -> QueueFuture<()>;
    fn cancel(&self, plugin: String, key: String) -> QueueFuture<bool>;
}

pub type Queue = Arc<dyn JobQueue>;

fn invalid(text: impl Into<String>) -> Error {
    Error::Invalid(text.into())
}

/// Job and schedule names: `[a-z0-9_]`, 1 to 40.
pub fn check_name(name: &str) -> Result<(), Error> {
    let fine = (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if fine {
        Ok(())
    } else {
        Err(invalid(
            "a job name is 1 to 40 lowercase letters, digits and _",
        ))
    }
}

pub fn check_key(key: &str) -> Result<(), Error> {
    let fine = (1..=MAX_KEY).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if fine {
        Ok(())
    } else {
        Err(invalid(format!(
            "a job key is 1 to {MAX_KEY} ASCII letters, digits and . _ : -"
        )))
    }
}

/// Checks a job a plugin wants queued, at `now`.
pub fn check(job: NewJob, now: chrono::DateTime<chrono::Utc>) -> Result<Queued, Error> {
    check_name(&job.name)?;
    if let Some(key) = &job.key {
        check_key(key)?;
    }
    if job.payload.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid(format!(
            "a job payload is bigger than {MAX_PAYLOAD_BYTES} bytes"
        )));
    }
    let payload: serde_json::Value =
        serde_json::from_str(&job.payload).map_err(|_| invalid("a job payload isn't JSON"))?;
    let run_at = match &job.run_at {
        None => None,
        Some(text) => {
            let at = chrono::DateTime::parse_from_rfc3339(text)
                .map_err(|_| invalid("run-at isn't an RFC 3339 time"))?
                .with_timezone(&chrono::Utc);
            let latest =
                now + chrono::Duration::from_std(MAX_AHEAD).unwrap_or(chrono::Duration::MAX);
            if at > latest {
                return Err(invalid("run-at is more than 120 days ahead"));
            }
            Some(at)
        }
    };
    Ok(Queued {
        name: job.name,
        key: job.key,
        payload,
        run_at,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test code

    use super::*;

    fn job(name: &str, payload: &str, run_at: Option<&str>) -> NewJob {
        NewJob {
            name: name.to_owned(),
            key: Some("moon:40161234".to_owned()),
            payload: payload.to_owned(),
            run_at: run_at.map(str::to_owned),
        }
    }

    #[test]
    fn jobs_are_checked() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let ok = check(
            job("ping", "{\"moon\": 1}", Some("2026-11-20T18:05:00Z")),
            now,
        )
        .unwrap();
        assert_eq!(ok.payload["moon"], 1);
        // In the past: runs as soon as possible.
        assert!(check(job("ping", "{}", Some("2020-01-01T00:00:00Z")), now).is_ok());
        for bad in [
            job("Ping", "{}", None),
            job("ping", "not json", None),
            job("ping", "{}", Some("tomorrow")),
            job("ping", "{}", Some("2027-03-01T00:00:00Z")),
            job(
                "ping",
                &format!("\"{}\"", "x".repeat(MAX_PAYLOAD_BYTES)),
                None,
            ),
            NewJob {
                key: Some("with space".to_owned()),
                ..job("ping", "{}", None)
            },
        ] {
            assert!(
                matches!(check(bad.clone(), now), Err(Error::Invalid(_))),
                "{bad:?}"
            );
        }
    }
}
