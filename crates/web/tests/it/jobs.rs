//! Plugin jobs: declared schedules and one-off jobs on the core queue,
//! replaced and cancelled by key, capped, run late with their scheduled
//! time, retried, and logged for admins.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};

const ID: &str = "nmu.jobs";

const RUNS: &str = "CREATE TABLE runs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    job_key text,
    payload jsonb,
    scheduled_at timestamptz,
    attempt int
);";

/// Two months out: queued, not due.
fn later() -> String {
    (chrono::Utc::now() + chrono::Duration::days(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn probe_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-storage"))
        .clone()
}

async fn install(h: &Harness, owner: &str, schedules: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Jobs\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nstorage = true\n{schedules}",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_runs.sql", RUNS.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn probe(h: &Harness, path: &str, query: &[(&str, &str)]) -> String {
    let query = query
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    run_probe(h, ID, path, query, false).await
}

fn registry(h: &Harness) -> Registry {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    registry
}

/// Runs everything that's due; returns the outcomes.
async fn work(h: &Harness) -> Vec<Outcome> {
    let registry = registry(h);
    let config = WorkerConfig::default();
    let mut outcomes = Vec::new();
    loop {
        match run_once(&h.db, &registry, &config).await.unwrap() {
            Outcome::Idle => return outcomes,
            outcome => outcomes.push(outcome),
        }
    }
}

async fn runs(db: &PgPool) -> Vec<(String, Option<String>, String, i32)> {
    sqlx::query_as(
        "SELECT name, job_key, to_char(scheduled_at AT TIME ZONE 'UTC', \
         'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), attempt FROM \"plugin_nmu.jobs\".runs ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

async fn queued(db: &PgPool) -> Vec<(Option<String>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT job_key, payload->'data' FROM core.jobs \
         WHERE plugin_id = $1 AND state = 'queued' ORDER BY id",
    )
    .bind(ID)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn uninstall(h: &Harness, owner: &str) {
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/uninstall"),
            &format!("confirmation={ID}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn one_off_jobs_run_late_with_their_scheduled_time(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;

    // Due in the past (a restart missed it): runs now, told when it was due.
    let out = probe(
        &h,
        "enqueue",
        &[
            ("name", "ping"),
            ("key", "moon:1"),
            ("payload", "{\"moon\": 1}"),
            ("at", "2026-01-01T12:00:00Z"),
        ],
    )
    .await;
    assert_eq!(out, "ok");
    // Far off: waits.
    probe(
        &h,
        "enqueue",
        &[
            ("name", "ping"),
            ("key", "moon:2"),
            ("at", later().as_str()),
        ],
    )
    .await;
    let out = work(&h).await;
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(
        runs(&h.db).await,
        [(
            "ping".to_owned(),
            Some("moon:1".to_owned()),
            "2026-01-01T12:00:00Z".to_owned(),
            1
        )]
    );
    assert_eq!(queued(&h.db).await.len(), 1, "moon:2 still waits");

    // What it logged is kept for admins.
    let detail = page(&h, "/admin/plugins/nmu.jobs", &owner).await.body;
    assert!(detail.contains("running ping (attempt 1)"), "{detail}");
    assert!(detail.contains("job:ping"), "{detail}");
    assert!(
        detail.contains("moon:2"),
        "the queued job is listed: {detail}"
    );
    uninstall(&h, &owner).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn keys_replace_and_cancel(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;

    for payload in ["{\"v\": 1}", "{\"v\": 2}"] {
        let out = probe(
            &h,
            "enqueue",
            &[
                ("name", "ping"),
                ("key", "moon:7"),
                ("payload", payload),
                ("at", later().as_str()),
            ],
        )
        .await;
        assert_eq!(out, "ok");
    }
    // Rescheduling moved it; no duplicate.
    assert_eq!(
        queued(&h.db).await,
        [(Some("moon:7".to_owned()), serde_json::json!({ "v": 2 }))]
    );
    assert_eq!(probe(&h, "cancel", &[("key", "moon:7")]).await, "ok true");
    assert_eq!(probe(&h, "cancel", &[("key", "moon:7")]).await, "ok false");
    assert!(queued(&h.db).await.is_empty());

    // Rules: names, keys, JSON, how far ahead.
    for query in [
        vec![("name", "Bad Name")],
        vec![("name", "ping"), ("key", "has space")],
        vec![("name", "ping"), ("payload", "not json")],
        vec![("name", "ping"), ("at", "2999-01-01T00:00:00Z")],
    ] {
        let out = probe(&h, "enqueue", &query).await;
        assert!(out.starts_with("err Error::Invalid"), "{query:?}: {out}");
    }
    uninstall(&h, &owner).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plugin_can_queue_only_so_many(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;
    // Fill up to the limit directly.
    sqlx::query(
        "INSERT INTO core.jobs (kind, payload, plugin_id, job_key, run_at) \
         SELECT 'plugin.job', '{}', $1, 'k' || n, now() + interval '1 day' \
         FROM generate_series(1, $2) AS n",
    )
    .bind(ID)
    .bind(tether_plugins::jobs::MAX_QUEUED as i32)
    .execute(&h.db)
    .await
    .unwrap();
    let out = probe(&h, "enqueue", &[("name", "ping")]).await;
    assert_eq!(out, "err Error::TooMany");
    // Replacing a queued one is still fine.
    let out = probe(&h, "enqueue", &[("name", "ping"), ("key", "k1")]).await;
    assert_eq!(out, "ok");
    // So many created in a day, finished or not, is a limit too.
    sqlx::query(
        "UPDATE core.jobs SET state = 'succeeded', finished_at = now() WHERE plugin_id = $1",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO core.jobs (kind, payload, plugin_id, state, finished_at) \
         SELECT 'plugin.job', '{}', $1, 'succeeded', now() FROM generate_series(1, $2)",
    )
    .bind(ID)
    .bind(tether_db::plugin_jobs::MAX_PER_DAY as i32)
    .execute(&h.db)
    .await
    .unwrap();
    let out = probe(&h, "enqueue", &[("name", "ping")]).await;
    assert_eq!(out, "err Error::TooMany");
    uninstall(&h, &owner).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn failures_retry_or_give_up(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;
    probe(&h, "enqueue", &[("name", "fail")]).await;
    probe(&h, "enqueue", &[("name", "boom")]).await;
    let outcomes = work(&h).await;
    assert!(
        outcomes.iter().any(|o| matches!(o, Outcome::Retrying(_))),
        "{outcomes:?}"
    );
    assert!(
        outcomes.iter().any(|o| matches!(o, Outcome::Dead(_))),
        "{outcomes:?}"
    );
    let detail = page(&h, "/admin/plugins/nmu.jobs", &owner).await.body;
    assert!(
        detail.contains("Gave up") && detail.contains("never"),
        "{detail}"
    );

    // A job whose plugin isn't running waits for it.
    send(&h.app, form("/admin/plugins/nmu.jobs/disable", "", &owner)).await;
    probe_while_stopped(&h).await;
    uninstall(&h, &owner).await;
}

async fn probe_while_stopped(h: &Harness) {
    tether_db::plugin_jobs::enqueue(&h.db, ID, "ping", None, &serde_json::json!({}), None, 100)
        .await
        .unwrap();
    let outcomes = work(h).await;
    assert!(
        matches!(outcomes.as_slice(), [Outcome::Deferred(_)]),
        "{outcomes:?}"
    );
    // Waiting for the plugin doesn't use up attempts.
    let attempts: i32 = sqlx::query_scalar(
        "SELECT attempts FROM core.jobs WHERE plugin_id = $1 AND state = 'queued' \
         AND payload->>'name' = 'ping'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(attempts, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_jobs_never_hold_up_core_work(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;
    // Long overdue, as far as the plugin is concerned...
    for _ in 0..20 {
        probe(
            &h,
            "enqueue",
            &[("name", "ping"), ("at", "1970-01-01T00:00:00Z")],
        )
        .await;
    }
    // ...but they queue from now, keeping what was asked as scheduled_at.
    let early: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 \
         AND (run_at < now() - interval '1 minute' OR scheduled_at > '1971-01-01')",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(early, 0);
    // Core workers don't claim plugin jobs at all.
    let core = tether_jobs::enqueue(
        &h.db,
        tether_jobs::NewJob::new("test.core", serde_json::json!({})),
    )
    .await
    .unwrap();
    let mut registry = Registry::new();
    registry.register("test.core", |_| async { Ok(()) });
    let outcome = run_once(&h.db, &registry, &WorkerConfig::default())
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Succeeded(core));
    assert_eq!(
        run_once(&h.db, &registry, &WorkerConfig::default())
            .await
            .unwrap(),
        Outcome::Idle
    );
    uninstall(&h, &owner).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_job_replaced_while_running_ends(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;
    probe(&h, "enqueue", &[("name", "requeue"), ("key", "moon:9")]).await;
    let registry = registry(&h);
    let first = run_once(&h.db, &registry, &WorkerConfig::default())
        .await
        .unwrap();
    assert!(matches!(first, Outcome::Dead(_)), "{first:?}");
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT state, last_error FROM core.jobs WHERE plugin_id = $1 ORDER BY id")
            .bind(ID)
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            (
                "dead".to_owned(),
                Some("replaced by a newer job with the same key".to_owned())
            ),
            ("queued".to_owned(), None),
        ]
    );
    // And an admin can't bring the replaced one back beside it.
    let dead: i64 =
        sqlx::query_scalar("SELECT id FROM core.jobs WHERE plugin_id = $1 AND state = 'dead'")
            .bind(ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(
        !tether_jobs::retry(&h.db, tether_jobs::JobId(dead))
            .await
            .unwrap()
    );
    uninstall(&h, &owner).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn declared_schedules_follow_the_plugin(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(
        &h,
        &owner,
        "\n[[capabilities.schedules]]\nname = \"sync\"\nevery = \"30m\"\n",
    )
    .await;
    let enabled = |db: PgPool| async move {
        sqlx::query_scalar::<_, bool>(
            "SELECT enabled FROM core.schedules WHERE name = 'plugin:nmu.jobs:sync'",
        )
        .fetch_optional(&db)
        .await
        .unwrap()
    };
    assert_eq!(enabled(h.db.clone()).await, Some(true));

    // Due now: the scheduler queues it and the plugin runs it by name.
    sqlx::query("UPDATE core.schedules SET next_run_at = now() - interval '1 minute'")
        .execute(&h.db)
        .await
        .unwrap();
    let queued = tether_jobs::schedule::run_due(&h.db).await.unwrap();
    assert!(
        queued.contains(&"plugin:nmu.jobs:sync".to_owned()),
        "{queued:?}"
    );
    work(&h).await;
    let ran = runs(&h.db).await;
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].0, "sync");
    assert!(
        page(&h, "/admin/plugins/nmu.jobs", &owner)
            .await
            .body
            .contains("every 30 minute(s)")
    );

    // Off with the plugin, back on with it, gone with it.
    send(&h.app, form("/admin/plugins/nmu.jobs/disable", "", &owner)).await;
    assert_eq!(enabled(h.db.clone()).await, Some(false));
    send(&h.app, form("/admin/plugins/nmu.jobs/enable", "", &owner)).await;
    assert_eq!(enabled(h.db.clone()).await, Some(true));
    probe(&h, "enqueue", &[("name", "ping"), ("at", later().as_str())]).await;
    uninstall(&h, &owner).await;
    assert_eq!(enabled(h.db.clone()).await, None);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE plugin_id = $1")
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pages_cant_queue_or_cancel_jobs(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "").await;
    let query = vec![("name".to_owned(), "ping".to_owned())];
    let out = run_probe(&h, ID, "enqueue", query, true).await;
    assert!(out.contains("pages can't queue"), "{out}");
    let query = vec![("key".to_owned(), "k".to_owned())];
    let out = run_probe(&h, ID, "cancel", query, true).await;
    assert!(out.contains("pages can't queue"), "{out}");
    assert!(queued(&h.db).await.is_empty());
    uninstall(&h, &owner).await;
}
