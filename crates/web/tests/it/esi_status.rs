//! The ESI Status app end to end (aa-esi-status): installed from its real
//! component and migrations, its check reading ESI's own status through
//! the host's public `esi-status`, and the status page, open to anyone
//! signed in: the verdict, what needs attention and since when, the areas,
//! the last day's outages and each change.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.esi-status";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("esi-status")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/esi-status/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let first = plugin_file("migrations/0001_esi_status.sql");
    let second = plugin_file("migrations/0002_since_and_changes.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_esi_status.sql", first.as_bytes()),
        ("migrations/0002_since_and_changes.sql", second.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn mount_status(h: &Harness, markets: &str, priority: u8) {
    Mock::given(method("GET"))
        .and(path("/meta/status"))
        .and(header_exists("X-Compatibility-Date"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "routes": [
                { "method": "GET", "path": "/alliances", "status": "OK" },
                { "method": "GET", "path": "/characters/{character_id}", "status": "OK" },
                { "method": "GET", "path": "/markets/{region_id}/orders", "status": markets },
            ],
        })))
        .with_priority(priority)
        .mount(&h.esi_server)
        .await;
}

async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// The five-minute schedule, due now: it starts the chain of checks if
/// none is stored for two minutes, and the chain's first runs at once.
async fn check(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:check"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
}

/// The chain's queued check, run now.
async fn next_check(h: &Harness) {
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'check-next' \
         AND state = 'queued'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(h).await;
}

async fn count(h: &Harness, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// The chain's queued checks.
async fn queued_next(h: &Harness) -> i64 {
    count(
        h,
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'check-next' \
         AND state = 'queued'",
    )
    .await
}

async fn dead(h: &Harness) -> i64 {
    count(
        h,
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND state = 'dead'",
    )
    .await
}

async fn status_reads(h: &Harness) -> usize {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/meta/status"))
        .count()
}

async fn checks(h: &Harness) -> i64 {
    sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.esi-status".checks"#)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_status_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let pilot = log_in(&h, None).await;
    let at = format!("/plugins/{ID}");

    // Before the first check.
    let res = page(&h, &at, &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("No ESI status data yet"), "{}", res.body);

    mount_status(&h, "Degraded", 5).await;
    check(&h).await;
    // A minute on, the next check, queued once.
    assert_eq!(queued_next(&h).await, 1);
    let soon = count(
        &h,
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'check-next' \
         AND state = 'queued' AND run_at BETWEEN now() + interval '50 seconds' \
         AND now() + interval '70 seconds'",
    )
    .await;
    assert_eq!(soon, 1);
    // The schedule leaves a running chain alone: no extra read of ESI.
    let reads = status_reads(&h).await;
    check(&h).await;
    assert_eq!(status_reads(&h).await, reads);
    assert_eq!(checks(&h).await, 1);
    assert_eq!(queued_next(&h).await, 1);
    let res = page(&h, &at, &pilot).await;
    assert!(res.body.contains("compatibility date 20"), "{}", res.body);
    // The verdict, in words, and what it means.
    assert!(res.body.contains("1 of 3 routes not OK"), "{}", res.body);
    assert!(
        res.body
            .contains("Degraded: these routes have a good chance of being slow"),
        "{}",
        res.body
    );
    // What needs attention first, then the areas, the worst first.
    let attention = res.body.find("Needs attention").unwrap();
    let degraded = res.body.find("/markets/{region_id}/orders").unwrap();
    let areas = res.body.find("By area").unwrap();
    assert!(attention < degraded && degraded < areas, "{}", res.body);
    assert!(res.body.contains(">1 degraded<"), "{}", res.body);
    let markets = res.body[areas..].find(">Markets<").unwrap();
    let alliances = res.body[areas..].find(">Alliances<").unwrap();
    assert!(markets < alliances, "{}", res.body);

    // Recovered: the history shows the change.
    mount_status(&h, "OK", 1).await;
    for moved in [
        r#"UPDATE "plugin_tether.esi-status".checks SET checked_at = checked_at - interval '5 minutes'"#,
        r#"UPDATE "plugin_tether.esi-status".tracking SET started = started - interval '5 minutes'"#,
    ] {
        sqlx::query(moved).execute(&h.db).await.unwrap();
    }
    check(&h).await;
    let res = page(&h, &at, &pilot).await;
    assert!(
        !res.body.contains("good chance of being slow"),
        "{}",
        res.body
    );
    assert!(res.body.contains("All 3 routes OK"), "{}", res.body);
    assert!(!res.body.contains("Needs attention"), "{}", res.body);
    // The change, newest first, and the incident: the stretch it was
    // degraded, over.
    assert!(res.body.contains("Changes, newest first"), "{}", res.body);
    let incidents = res.body.find("Incidents, last 24 hours").unwrap();
    let areas = res.body.find("By area").unwrap();
    assert!(
        res.body[incidents..areas].contains(">Degraded<"),
        "{}",
        res.body
    );
    // Which area it was in, from the route's change.
    assert!(
        res.body[incidents..areas].contains(">Markets<"),
        "{}",
        res.body
    );
    assert!(
        !res.body.contains("Every route was OK at every check"),
        "{}",
        res.body
    );
    assert!(!res.body.contains(">Ongoing<"), "{}", res.body);
    assert_eq!(checks(&h).await, 2);
    let changed: (String, String) = sqlx::query_as(
        r#"SELECT was, status FROM "plugin_tether.esi-status".changes WHERE path = '/markets/{region_id}/orders'"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(changed, ("Degraded".to_owned(), "OK".to_owned()));

    // The chain's next check: one more row, the next queued, nothing
    // dead, and a route whose status held keeps its since.
    let since =
        r#"SELECT since::text FROM "plugin_tether.esi-status".routes WHERE path = '/alliances'"#;
    let before: String = sqlx::query_scalar(since).fetch_one(&h.db).await.unwrap();
    next_check(&h).await;
    assert_eq!(checks(&h).await, 3);
    assert_eq!(queued_next(&h).await, 1);
    assert_eq!(dead(&h).await, 0);
    let after: String = sqlx::query_scalar(since).fetch_one(&h.db).await.unwrap();
    assert_eq!(before, after);

    // Checks that stopped: the page says it's out of date.
    sqlx::query(
        r#"UPDATE "plugin_tether.esi-status".checks SET checked_at = checked_at - interval '1 hour'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let res = page(&h, &at, &pilot).await;
    assert!(
        res.body.contains("Out of date: the last check was 1h"),
        "{}",
        res.body
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_full_day_of_minute_checks_counts(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_status(&h, "OK", 5).await;
    check(&h).await;
    // A day of checks a minute apart before it, three of them degraded.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.esi-status".checks
               (checked_at, compatibility_date, ok, degraded, down, recovering, unknown)
           SELECT now() - make_interval(mins => n), '2026-08-18',
                  CASE WHEN n BETWEEN 1200 AND 1202 THEN 2 ELSE 3 END,
                  CASE WHEN n BETWEEN 1200 AND 1202 THEN 1 ELSE 0 END, 0, 0, 0
           FROM generate_series(1, 1439) AS n"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let pilot = log_in(&h, None).await;
    let res = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("of 1440 checks"), "{}", res.body);
    assert!(res.body.contains("99.8%"), "{}", res.body);
    let incidents = res.body.find("Incidents, last 24 hours").unwrap();
    let areas = res.body.find("By area").unwrap();
    assert!(
        res.body[incidents..areas].contains(">Degraded<"),
        "{}",
        res.body
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_check_that_fails_waits_for_the_next(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // ESI down (its daily downtime, say).
    Mock::given(method("GET"))
        .and(path("/meta/status"))
        .respond_with(
            ResponseTemplate::new(503).set_body_raw(r#"{"error":"downtime"}"#, "application/json"),
        )
        .mount(&h.esi_server)
        .await;
    // The schedule starts the chain; its check fails, and so does the
    // next.
    check(&h).await;
    next_check(&h).await;
    // Nothing stored for two minutes: the schedule restarts the chain,
    // without reading ESI itself.
    check(&h).await;
    assert_eq!(status_reads(&h).await, 3);
    assert_eq!(checks(&h).await, 0);
    // Each failure is a line in the app's log, as aa-esi-status logs it,
    // and waits for the next check, a minute on: no job dies of it.
    let logged = count(
        &h,
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND level = 'error' \
         AND message LIKE 'reading ESI''s status: %'",
    )
    .await;
    assert_eq!(logged, 3);
    assert_eq!(dead(&h).await, 0);
    assert_eq!(queued_next(&h).await, 1);
}
