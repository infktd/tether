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
//!
//! HTTP too: `http?url=&method=post&body=&secret=&h=<name>:<value>`, and
//! `http_repeat?url=&n=` for limits. Notices: `notify-account`,
//! `notify-holders`, `notify-submitter-reference` and `notify-submitter`.
//!
//! Pages are read-only, so tests that write or queue go through `submit`,
//! which runs the same probe.

use tether_plugin_sdk::discord::{self, Mention};
use tether_plugin_sdk::doctrines;
use tether_plugin_sdk::downloads;
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::http;
use tether_plugin_sdk::identity;
use tether_plugin_sdk::jobs::{self, Job, JobError, NewJob};
use tether_plugin_sdk::notify;
use tether_plugin_sdk::storage::{self, Statement, Value};
use tether_plugin_sdk::{Page, PageError, Plugin, Request, Submission, SubmitResult, log};

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

fn probe(request: Request) -> Result<Page, PageError> {
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
        // esi?endpoint=&character=<id> (or source=<id>)&page=&<param>=
        // (any other argument is one of the endpoint's params)
        "esi" => {
            let subject = match (arg("character"), arg("source")) {
                (Some(id), _) => Subject::Character(id.parse().unwrap_or_default()),
                (None, Some(id)) => Subject::DataSource(id.parse().unwrap_or_default()),
                (None, None) => Subject::Character(0),
            };
            let params: Vec<(String, String)> = request
                .query
                .iter()
                .filter(|(k, _)| !["endpoint", "character", "source", "page"].contains(&k.as_str()))
                .cloned()
                .collect();
            let page = arg("page").and_then(|p| p.parse().ok());
            match esi::get(&arg("endpoint").unwrap_or_default(), subject, &params, page) {
                Ok(r) => format!("ok pages={} {}", r.pages, r.body),
                Err(e) => format!("err {e:?}"),
            }
        }
        // esi-post?endpoint=&character=<id>&body=&times=<n, 1 if absent>
        "esi-post" => {
            let character = arg("character")
                .and_then(|c| c.parse().ok())
                .unwrap_or_default();
            let times: usize = arg("times").and_then(|t| t.parse().ok()).unwrap_or(1);
            (0..times)
                .map(|_| {
                    match esi::post(
                        &arg("endpoint").unwrap_or_default(),
                        Subject::Character(character),
                        &arg("body").unwrap_or_default(),
                    ) {
                        Ok(r) => format!("ok {}", r.body),
                        Err(e) => format!("err {e:?}"),
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ")
        }
        // doctrines-publish?list=<[{"key","name","link","groups"}] as JSON>&see_all=
        "doctrines-publish" => {
            let list: Vec<serde_json::Value> =
                serde_json::from_str(&arg("list").unwrap_or_default()).unwrap_or_default();
            let shared: Vec<doctrines::Doctrine> = list
                .iter()
                .map(|d| doctrines::Doctrine {
                    key: d["key"].as_str().unwrap_or_default().to_owned(),
                    name: d["name"].as_str().unwrap_or_default().to_owned(),
                    link: d["link"].as_str().unwrap_or_default().to_owned(),
                    groups: d["groups"]
                        .as_array()
                        .map(|g| g.iter().filter_map(serde_json::Value::as_i64).collect()),
                })
                .collect();
            match doctrines::publish(&shared, arg("see_all").as_deref()) {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("err {e:?}"),
            }
        }
        // doctrines-published: "name link source" per line
        "doctrines-published" => match doctrines::published() {
            Ok(shared) => shared
                .iter()
                .map(|d| format!("{} {} {}", d.name, d.link, d.source))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("err {e:?}"),
        },
        // download-build?name=&title=&permission=&header=<JSON>&rows=<JSON>&finish=no
        // (&stale=yes: begins twice, then carries on with the first build)
        "download-build" => {
            let json = |key: &str| -> Vec<serde_json::Value> {
                serde_json::from_str(&arg(key).unwrap_or_default()).unwrap_or_default()
            };
            let cells = |v: &serde_json::Value| -> Vec<String> {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .map(|c| c.as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let header = json("header")
                .iter()
                .map(|c| c.as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>();
            let rows: Vec<Vec<String>> = json("rows").iter().map(cells).collect();
            let name = arg("name").unwrap_or_default();
            let begin = || {
                downloads::begin(
                    &name,
                    &arg("title").unwrap_or_default(),
                    &arg("permission").unwrap_or_default(),
                    &header,
                )
            };
            let built = begin()
                .and_then(|build| {
                    if arg("stale").as_deref() == Some("yes") {
                        begin()?;
                    }
                    Ok(build)
                })
                .and_then(|build| downloads::append(&name, build, &rows).map(|()| build))
                .and_then(|build| {
                    if arg("finish").as_deref() == Some("no") {
                        Ok(())
                    } else {
                        downloads::finish(&name, build)
                    }
                });
            match built {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("err {e:?}"),
            }
        }
        // notify-account?account=&title=&message=
        "notify-account" => match notify::account(
            arg("account")
                .and_then(|a| a.parse().ok())
                .unwrap_or_default(),
            &arg("title").unwrap_or_default(),
            &arg("message").unwrap_or_default(),
            notify::Level::Success,
        ) {
            Ok(sent) => format!("ok {sent}"),
            Err(e) => format!("err {e:?}"),
        },
        // notify-holders?permission=&title=&message=&except=
        "notify-holders" => match notify::holders(
            &arg("permission").unwrap_or_default(),
            &arg("title").unwrap_or_default(),
            &arg("message").unwrap_or_default(),
            notify::Level::Info,
            arg("except").and_then(|a| a.parse().ok()),
        ) {
            Ok(reached) => format!("ok {reached}"),
            Err(e) => format!("err {e:?}"),
        },
        // notify-submitter-reference (twice, to show it's the same)
        "notify-submitter-reference" => {
            match (notify::submitter_reference(), notify::submitter_reference()) {
                (Ok(a), Ok(b)) if a == b => format!("ok {a}"),
                (Ok(a), Ok(b)) => format!("differ {a} {b}"),
                (Err(e), _) | (_, Err(e)) => format!("err {e:?}"),
            }
        }
        // notify-submitter?reference=&title=&message=
        "notify-submitter" => match notify::submitter(
            &arg("reference").unwrap_or_default(),
            &arg("title").unwrap_or_default(),
            &arg("message").unwrap_or_default(),
            notify::Level::Info,
        ) {
            Ok(sent) => format!("ok {sent}"),
            Err(e) => format!("err {e:?}"),
        },
        "download-files" => downloads::files()
            .iter()
            .map(|f| format!("{} {} {}", f.name, f.title, f.rows))
            .collect::<Vec<_>>()
            .join("\n"),
        // http?url=&method=post&body=&secret=&h=<name>:<value>
        "http" => {
            let url = arg("url").unwrap_or_default();
            let mut req = match arg("method").as_deref() {
                Some("post") => {
                    http::Request::post(url, arg("body").unwrap_or_default().into_bytes())
                }
                _ => http::Request::get(url),
            };
            for (k, v) in &request.query {
                if k == "h"
                    && let Some((name, value)) = v.split_once(':')
                {
                    req = req.header(name, value);
                }
            }
            if let Some(secret) = arg("secret") {
                req = req.secret(secret);
            }
            match req.send() {
                Ok(r) => format!(
                    "ok status={} headers={:?} body={}",
                    r.status,
                    r.headers,
                    r.text().unwrap_or("(binary)")
                ),
                Err(e) => format!("err {e:?}"),
            }
        }
        // http_repeat?url=&n=: how many GETs went through, and how the
        // rest were refused.
        "http_repeat" => {
            let url = arg("url").unwrap_or_default();
            let n: usize = arg("n").and_then(|n| n.parse().ok()).unwrap_or(1);
            let mut outcomes: Vec<String> = Vec::new();
            for _ in 0..n {
                outcomes.push(match http::get(&url) {
                    Ok(_) => "ok".to_owned(),
                    Err(e) => format!("{e:?}"),
                });
            }
            let ok = outcomes.iter().filter(|o| *o == "ok").count();
            let too_many = outcomes.iter().filter(|o| o.contains("TooMany")).count();
            format!("ok={ok} too_many={too_many}")
        }
        // A main page, for the host's parts around it (Add owner).
        "" => "home".to_owned(),
        "viewer" => format!("{:?}", identity::viewer()),
        "acting" => format!("{:?}", identity::acting()),
        "superuser" => identity::superuser().to_string(),
        "characters" => format!("{:?}", esi::characters()),
        "owners" => format!("{:?}", identity::owners()),
        "members" => format!("{:?}", identity::members()),
        "sources" => format!("{:?}", esi::data_sources()),
        // send?channel=&text=&state=Member
        "send" => {
            let mention = arg("state").map_or(Mention::None, Mention::State);
            let channel = arg("channel")
                .or_else(|| discord::channels().first().map(|c| c.id.clone()))
                .unwrap_or_default();
            match discord::send(&channel, &arg("text").unwrap_or_default(), mention) {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("err {e:?}"),
            }
        }
        // embed?title=&channel=&state=&image=: a card with every part.
        "embed" => {
            let mention = arg("state").map_or(Mention::None, Mention::State);
            let channel = arg("channel")
                .or_else(|| discord::channels().first().map(|c| c.id.clone()))
                .unwrap_or_default();
            let image = arg("image").and_then(|i| i.parse().ok()).unwrap_or(35835);
            let embed = discord::Embed::new(arg("title").unwrap_or_default())
                .description("Chunk arrives <t:1793592000:R> @everyone")
                .color(0x2e_cc71)
                .author("Acme Corp", Some(discord::Image::Corporation(98000001)))
                .thumbnail(discord::Image::TypeRender(image))
                .field("System", "Mazitah")
                .wide_field("Structure", "Mazitah - Refinery")
                .footer("Structures")
                .timestamp("2026-11-02T04:00:00+00:00");
            match discord::send_embed(&channel, &embed, mention) {
                Ok(()) => "ok".to_owned(),
                Err(e) => format!("err {e:?}"),
            }
        }
        _ => return Err(PageError::NotFound),
    };
    Ok(Page::new("Probe").text(shown(outcome)))
}

impl Plugin for Probe {
    /// Pages are read-only: writes and job calls fail here.
    fn render(request: Request) -> Result<Page, PageError> {
        probe(request)
    }

    /// The same, from a form post, where writes and jobs are allowed.
    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        probe(submission.request).map(SubmitResult::Page)
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
