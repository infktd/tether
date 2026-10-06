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

async fn check(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:check"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
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
    let checks: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.esi-status".checks"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(checks, 2);
    let changed: (String, String) = sqlx::query_as(
        r#"SELECT was, status FROM "plugin_tether.esi-status".changes WHERE path = '/markets/{region_id}/orders'"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(changed, ("Degraded".to_owned(), "OK".to_owned()));

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
