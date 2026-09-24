#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::time::{Duration, Instant};

use serde_json::json;
use sqlx::PgPool;
use tether_esi::{Esi, Priority, names};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn status_body() -> serde_json::Value {
    json!({"players": 20000, "server_version": "1", "start_time": "2026-09-23T11:05:49Z", "vip": false})
}

async fn esi() -> (MockServer, Esi) {
    let server = MockServer::start().await;
    let esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
    (server, esi)
}

#[tokio::test]
async fn every_response_updates_the_budget() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(status_body())
                .insert_header("X-ESI-Error-Limit-Remain", "99")
                .insert_header("X-ESI-Error-Limit-Reset", "42")
                .insert_header("X-Ratelimit-Group", "status")
                .insert_header("X-Ratelimit-Limit", "600/15m")
                .insert_header("X-Ratelimit-Remaining", "597"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(420)
                .set_body_json(json!({"error": "error limited"}))
                .insert_header("X-ESI-Error-Limit-Remain", "0")
                .insert_header("X-ESI-Error-Limit-Reset", "30"),
        )
        .mount(&server)
        .await;

    esi.players_online().await.unwrap();
    let after_ok = esi.budget();
    assert_eq!(after_ok.error_remain, Some(99));
    assert_eq!(after_ok.groups[0].remaining, Some(597));
    assert_eq!(after_ok.counts.ok, 1);

    assert!(esi.players_online().await.is_err());
    let after_420 = esi.budget();
    assert_eq!(
        after_420.counts.error_limited, 1,
        "a 420 is what we must never see in production"
    );
    assert_eq!(after_420.lowest_error_remain, Some(0));
}

async fn slow_affiliations(server: &MockServer, delay: Duration) {
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"character_id": 1, "corporation_id": 2}]))
                .set_delay(delay),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn interactive_requests_never_queue_behind_bulk_work() {
    let (server, esi) = esi().await;
    slow_affiliations(&server, Duration::from_millis(600)).await;

    // 8 bulk requests with 4 slots: two rounds, about 1.2s.
    let started = Instant::now();
    let mut bulk = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let esi = esi.clone();
        bulk.spawn(async move { esi.affiliations(&[1], Priority::Bulk).await.unwrap() });
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    esi.affiliations(&[1], Priority::Interactive).await.unwrap();
    let interactive = started.elapsed();
    bulk.join_all().await;
    let all_bulk = started.elapsed();

    assert!(
        interactive < Duration::from_millis(1000),
        "interactive waited: {interactive:?}"
    );
    assert!(
        all_bulk >= Duration::from_millis(1150),
        "bulk was gated: {all_bulk:?}"
    );
}

#[tokio::test]
async fn bulk_backs_off_when_the_error_budget_is_low_but_interactive_does_not() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(status_body())
                .insert_header("X-ESI-Error-Limit-Remain", "20")
                .insert_header("X-ESI-Error-Limit-Reset", "1"),
        )
        .mount(&server)
        .await;
    slow_affiliations(&server, Duration::ZERO).await;
    esi.players_online().await.unwrap(); // learns: 20 errors left, reset in 1s

    let started = Instant::now();
    esi.affiliations(&[1], Priority::Interactive).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "interactive isn't held back"
    );

    let started = Instant::now();
    esi.affiliations(&[1], Priority::Bulk).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "bulk waited for the reset: {:?}",
        started.elapsed()
    );
}

async fn mount_names(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(|req: &wiremock::Request| {
            let ids: Vec<i64> = serde_json::from_slice(&req.body).unwrap();
            let known = [
                (159826257_i64, "Otherworld Empire", "alliance"),
                (98133756, "CircleOfTwo Holding", "corporation"),
            ];
            if ids.iter().any(|id| !known.iter().any(|(k, _, _)| k == id)) {
                return ResponseTemplate::new(404)
                    .set_body_json(json!({"error": "Ensure all IDs are valid before resolving"}));
            }
            let body: Vec<_> = known
                .iter()
                .filter(|(k, _, _)| ids.contains(k))
                .map(|(id, name, cat)| json!({"id": id, "name": name, "category": cat}))
                .collect();
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(server)
        .await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn names_are_cached_in_postgres(db: PgPool) {
    let (server, esi) = esi().await;
    mount_names(&server).await;

    let first = names::resolve(&db, &esi, &[159826257, 98133756], Priority::Interactive)
        .await
        .unwrap();
    assert_eq!(first[&159826257].name, "Otherworld Empire");
    assert_eq!(first[&98133756].category, "corporation");

    let again = names::resolve(&db, &esi, &[159826257], Priority::Interactive)
        .await
        .unwrap();
    assert_eq!(again[&159826257].name, "Otherworld Empire");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "second lookup came from Postgres"
    );

    // Stale names are fetched again.
    sqlx::query("UPDATE core.entity_names SET fetched_at = now() - interval '8 days'")
        .execute(&db)
        .await
        .unwrap();
    names::resolve(&db, &esi, &[159826257], Priority::Interactive)
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_unknown_id_does_not_hide_the_known_ones(db: PgPool) {
    let (server, esi) = esi().await;
    mount_names(&server).await;

    let found = names::resolve(&db, &esi, &[159826257, 5], Priority::Interactive)
        .await
        .unwrap();

    assert_eq!(found.len(), 1);
    assert!(found.contains_key(&159826257));
}
