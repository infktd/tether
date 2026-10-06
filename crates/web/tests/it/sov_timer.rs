//! The Sovereignty Timer app end to end (aa-sov-timer): installed from its
//! real component and migration, its sync reading ESI's public campaigns,
//! constellations, sovereignty (ADM) and names, and the page reading the
//! scores live: every campaign, upcoming ones and active ones, with the
//! defenders' progress.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.sov-timer";
const ALLIANCE: i64 = 99005338;
const CONSTELLATION: i64 = 20000069;
const REGION: i64 = 10000012;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("sov-timer")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/sov-timer/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_sov_timer.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_sov_timer.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn campaign(id: i64, system: i64, start: DateTime<Utc>, score: f64) -> serde_json::Value {
    serde_json::json!({
        "campaign_id": id, "event_type": "ihub_defense", "solar_system_id": system,
        "constellation_id": CONSTELLATION, "structure_id": 1_000_000 + id,
        "defender_id": ALLIANCE, "defender_score": score, "attackers_score": 1.0 - score,
        "start_time": rfc(start),
    })
}

async fn mount_campaigns(h: &Harness, campaigns: serde_json::Value, priority: u8) {
    Mock::given(method("GET"))
        .and(path("/sovereignty/campaigns"))
        .respond_with(ResponseTemplate::new(200).set_body_json(campaigns))
        .with_priority(priority)
        .mount(&h.esi_server)
        .await;
}

async fn sync(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:sync"))
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
async fn sovereignty_timer_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let now = Utc::now();
    mount_campaigns(
        &h,
        serde_json::json!([
            campaign(1, 30000474, now - Duration::hours(1), 0.6),
            campaign(2, 30000475, now + Duration::hours(2), 0.6),
            campaign(3, 30000476, now + Duration::hours(10), 0.6),
        ]),
        5,
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/universe/constellations/{CONSTELLATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "constellation_id": CONSTELLATION, "name": "O-EIMK", "region_id": REGION,
            "position": { "x": 1.0, "y": 2.0, "z": 3.0 }, "systems": [30000474],
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sovereignty/systems"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "solar_systems": [{ "solar_system_id": 30000474, "claim": { "alliance": {
                "alliance_id": ALLIANCE, "corporation_id": 98000001,
                "claimed_since": "2026-01-01T00:00:00Z", "is_capital_system": false,
                "sovereignty_hub": { "id": 1 },
                "development": { "activity_defense_multiplier": 4.2, "industrial_level": 0,
                    "military_level": 0, "strategic_level": 0 },
            } } }],
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": 30000474, "name": "1-SMEB", "category": "solar_system" },
            { "id": 30000475, "name": "49-U6U", "category": "solar_system" },
            { "id": 30000476, "name": "8QT-H4", "category": "solar_system" },
            { "id": REGION, "name": "Catch", "category": "region" },
            { "id": ALLIANCE, "name": "Pandemic Horde", "category": "alliance" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    sync(&h).await;

    let at = format!("/plugins/{ID}");
    let res = page(&h, &at, &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for text in [
        "1-SMEB",
        "49-U6U",
        "8QT-H4",
        "O-EIMK",
        "Catch",
        "Pandemic Horde",
        "IHub defense",
        "4.2",
        "Upcoming (&#60; 4 hrs)",
    ] {
        assert!(res.body.contains(text), "{text}: {}", res.body);
    }
    // No change yet on the active one, which is Active rather than a
    // countdown; the others show no score before they start.
    assert!(res.body.contains("60% · no change"), "{}", res.body);
    assert!(res.body.contains(">Active<"), "{}", res.body);
    assert_eq!(res.body.matches("60%").count(), 1, "{}", res.body);

    // The next sync: the attackers gain on the active one.
    mount_campaigns(
        &h,
        serde_json::json!([
            campaign(1, 30000474, now - Duration::hours(1), 0.55),
            campaign(2, 30000475, now + Duration::hours(2), 0.6),
            campaign(3, 30000476, now + Duration::hours(10), 0.6),
        ]),
        1,
    )
    .await;
    sync(&h).await;
    let res = page(&h, &at, &owner).await;
    assert!(res.body.contains("55% · attackers gaining"), "{}", res.body);
    // Each sync queues the next, 30 seconds on, once.
    let next: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE job_key = 'sync-next' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(next, 1);

    // Only for basic_access.
    let pilot = log_in(&h, None).await;
    assert_eq!(page(&h, &at, &pilot).await.status, StatusCode::NOT_FOUND);
}
