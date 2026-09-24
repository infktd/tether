//! Runs whatever SQL a request carries through the storage API and shows
//! the outcome as text, so tests can probe what a plugin role may do.
//!
//! `?sql=...` (repeated for `transaction`) and `?p=<kind>:<value>` for
//! each parameter: `n:` null, `b:true`, `i:42`, `f:1.5`, `t:text`,
//! `ts:<rfc3339>`, `j:<json>`, `x:<hex bytes>`.
//!
//! Jobs too: `enqueue?name=&key=&payload=&at=` and `cancel?key=`. Each job
//! run is recorded in a `runs` table (when the package's migration made
//! one); a job named `fail` asks to be retried, `boom` gives up.

use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::storage::{self, Statement, Value};
use tether_plugin_sdk::{Page, PageError, Plugin, Request, log};

struct Probe;

fn param(spec: &str) -> Value {
    let (kind, value) = spec.split_once(':').unwrap_or((spec, ""));
    match kind {
        "n" => Value::Null,
        "b" => Value::Boolean(value == "true"),
        "i" => Value::Integer(value.parse().unwrap_or_default()),
        "f" => Value::Float(value.parse().unwrap_or_default()),
        "ts" => Value::timestamp(value),
        "j" => Value::json(value),
        "x" => Value::Bytes(
            (0..value.len() / 2)
                .map(|i| u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap_or_default())
                .collect(),
        ),
        _ => Value::Text(value.to_owned()),
    }
}

fn shown(text: String) -> String {
    text.chars().take(1900).collect()
}

impl Plugin for Probe {
    fn render(request: Request) -> Result<Page, PageError> {
        let sql: Vec<&str> = request
            .query
            .iter()
            .filter(|(k, _)| k == "sql")
            .map(|(_, v)| v.as_str())
            .collect();
        let params: Vec<Value> = request
            .query
            .iter()
            .filter(|(k, _)| k == "p")
            .map(|(_, v)| param(v))
            .collect();
        let first = sql.first().copied().unwrap_or("");
        let arg = |name: &str| {
            request
                .query
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let outcome = match request.path.as_str() {
            "query" => match storage::query(first, &params) {
                Ok(rows) => format!("ok rows={} {:?}", rows.rows.len(), rows),
                Err(e) => format!("err {e:?}"),
            },
            "execute" => match storage::execute(first, &params) {
                Ok(n) => format!("ok changed={n}"),
                Err(e) => format!("err {e:?}"),
            },
            "transaction" => {
                let statements: Vec<Statement> = sql
                    .iter()
                    .map(|s| Statement::new(*s, params.clone()))
                    .collect();
                match storage::transaction(&statements) {
                    Ok(counts) => format!("ok changed={counts:?}"),
                    Err(e) => format!("err {e:?}"),
                }
            }
            "enqueue" => {
                let mut job = NewJob::new(arg("name").unwrap_or_default());
                if let Some(key) = arg("key") {
                    job = job.key(key);
                }
                if let Some(payload) = arg("payload") {
                    job = job.payload(payload);
                }
                if let Some(at) = arg("at") {
                    job = job.at(at);
                }
                match jobs::enqueue(job) {
                    Ok(()) => "ok".to_owned(),
                    Err(e) => format!("err {e:?}"),
                }
            }
            "cancel" => match jobs::cancel(&arg("key").unwrap_or_default()) {
                Ok(found) => format!("ok {found}"),
                Err(e) => format!("err {e:?}"),
            },
            _ => return Err(PageError::NotFound),
        };
        Ok(Page::new("Probe").text(shown(outcome)))
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        log::info(format!("running {} (attempt {})", job.name, job.attempt));
        // Plugins without storage (or without the table) just log.
        let _ = storage::execute(
            "INSERT INTO runs (name, job_key, payload, scheduled_at, attempt) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                job.name.clone().into(),
                job.key.clone().into(),
                Value::json(job.payload.clone()),
                Value::timestamp(job.scheduled_at.clone()),
                i64::from(job.attempt).into(),
            ],
        );
        match job.name.as_str() {
            "fail" => Err(JobError::Retry("not yet".into())),
            // Queues itself again under its key, then asks to be retried.
            "requeue" => {
                let mut again = NewJob::new("requeue").at("2020-01-01T00:00:00Z");
                if let Some(key) = &job.key {
                    again = again.key(key.clone());
                }
                let _ = jobs::enqueue(again);
                Err(JobError::Retry("again".into()))
            }
            "boom" => Err(JobError::Permanent("never".into())),
            _ => Ok(()),
        }
    }
}

tether_plugin_sdk::export!(Probe);
