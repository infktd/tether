#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_core::tiers::{EntityKind, Tier};
use tether_db::tiers::{TierRule, set_rule};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// From tests/fixtures/esi/characters_affiliation.json.
const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536, alliance 159826257
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456
const MITTANI: &str = "443630591:The Mittani"; // NPC corp 1000167, no alliance

async fn rule(db: &PgPool, entity_id: i64, kind: EntityKind, tier: Tier) {
    set_rule(
        db,
        &TierRule {
            entity_id,
            kind,
            tier,
            name: format!("entity {entity_id}"),
        },
    )
    .await
    .unwrap();
}

async fn tier_of(h: &Harness, token: &str) -> String {
    me(h, token).await["tier"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tiers_follow_the_main_at_login(db: PgPool) {
    rule(&db, 159826257, EntityKind::Alliance, Tier::Member).await;
    rule(&db, 98133756, EntityKind::Corporation, Tier::Allied).await;
    let h = harness(db, true).await;

    let member = log_in_as(&h, CHRIBBA, None).await;
    let allied = log_in_as(&h, GIGX, None).await;
    let guest = log_in_as(&h, MITTANI, None).await;

    assert_eq!(tier_of(&h, &member).await, "member");
    assert_eq!(tier_of(&h, &allied).await, "allied");
    assert_eq!(tier_of(&h, &guest).await, "guest");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn switching_main_re_evaluates_the_tier(db: PgPool) {
    rule(&db, 159826257, EntityKind::Alliance, Tier::Member).await;
    let h = harness(db, true).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    let token = log_in_as(&h, MITTANI, Some(&token)).await;
    assert_eq!(tier_of(&h, &token).await, "member");

    let res = send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":443630591}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert_eq!(tier_of(&h, &token).await, "guest");

    send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":196379789}"#),
    )
    .await;
    assert_eq!(tier_of(&h, &token).await, "member");

    // Each change is in the audit log, attributed to the system.
    let changes: Vec<(Option<i64>, serde_json::Value)> = sqlx::query_as(
        "SELECT actor_account_id, details FROM core.audit_log WHERE action = 'tier.change' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    let to: Vec<&str> = changes
        .iter()
        .map(|(_, d)| d["to"].as_str().unwrap())
        .collect();
    assert_eq!(to, ["member", "guest", "member"]);
    assert!(changes.iter().all(|(actor, _)| actor.is_none()));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_outage_at_login_queues_a_retry_that_fixes_the_tier(db: PgPool) {
    rule(&db, 159826257, EntityKind::Alliance, Tier::Member).await;
    let esi_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(ResponseTemplate::new(503).set_body_json(
            serde_json::json!({"error": "The datasource tranquility is temporarily unavailable"}),
        ))
        .mount(&esi_server)
        .await;
    let h = harness_with_esi(db, true, esi_server).await;

    // Login still works; the tier stays at the default until ESI is back.
    let token = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(tier_of(&h, &token).await, "guest");
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'tiers.refresh_account' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);

    // ESI recovers; the queued job runs and fixes the tier.
    h.esi_server.reset().await;
    mount_affiliations(&h.esi_server).await;
    let mut registry = tether_jobs::Registry::new();
    tether_web::tiers::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    let outcome = tether_jobs::run_once(&h.db, &registry, &tether_jobs::WorkerConfig::default())
        .await
        .unwrap();

    assert!(matches!(outcome, tether_jobs::Outcome::Succeeded(_)));
    assert_eq!(tier_of(&h, &token).await, "member");
}
