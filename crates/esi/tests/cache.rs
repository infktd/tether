#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The Postgres ESI cache (`core.esi_cache`) behind the shared client.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use eve_esi_client::cache::{Bytes, CacheKey, CachedResponse, EsiCache, HeaderMap, StatusCode};
use sqlx::PgPool;
use tether_core::Secret;
use tether_esi::cache::PgCache;
use tether_esi::{Esi, Priority};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CORP: i64 = 1164409536;

fn corporation() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/esi/corporations_1164409536.json"
    ))
    .unwrap()
}

fn http_date(from_now: Duration) -> String {
    (chrono::Utc::now() + from_now)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

fn esi(server: &MockServer, db: &PgPool) -> Esi {
    Esi::with_cache(
        "tether tests",
        Some(&server.uri()),
        Arc::new(PgCache::new(db.clone())),
    )
    .unwrap()
}

async fn rows(db: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT url, principal FROM core.esi_cache ORDER BY url")
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fresh_responses_are_served_from_postgres_after_a_restart(db: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(corporation(), "application/json")
                .insert_header("Expires", http_date(Duration::from_secs(3600)))
                .insert_header("ETag", "\"v1\""),
        )
        .mount(&server)
        .await;

    let first = esi(&server, &db);
    assert_eq!(
        first
            .corporation_ticker(CORP, Priority::Bulk)
            .await
            .unwrap(),
        "OTHER"
    );
    // A new client, as after a restart: the entry is still fresh.
    let restarted = esi(&server, &db);
    assert_eq!(
        restarted
            .corporation_ticker(CORP, Priority::Bulk)
            .await
            .unwrap(),
        "OTHER"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        rows(&db).await,
        [(
            format!("{}/corporations/{CORP}", server.uri()),
            String::new()
        )]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn stale_entries_are_revalidated_with_their_etag(db: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}")))
        .and(header("If-None-Match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304).insert_header("ETag", "\"v1\""))
        .with_priority(1)
        .mount(&server)
        .await;
    // An ETag and no Expires: never fresh, always revalidated.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(corporation(), "application/json")
                .insert_header("ETag", "\"v1\""),
        )
        .with_priority(2)
        .mount(&server)
        .await;

    let esi = esi(&server, &db);
    for _ in 0..2 {
        assert_eq!(
            esi.corporation_ticker(CORP, Priority::Bulk).await.unwrap(),
            "OTHER"
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].headers["if-none-match"], "\"v1\"");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn private_responses_are_not_stored_without_a_principal(db: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(corporation(), "application/json")
                .insert_header("Expires", http_date(Duration::from_secs(3600)))
                .insert_header("Cache-Control", "private"),
        )
        .mount(&server)
        .await;

    esi(&server, &db)
        .corporation_ticker(CORP, Priority::Bulk)
        .await
        .unwrap();
    assert!(rows(&db).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn calls_with_a_characters_token_never_touch_the_cache(db: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/members")))
        .and(header("Authorization", "Bearer the-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json([2112625428_i64, 95465499])
                .insert_header("Expires", http_date(Duration::from_secs(3600)))
                .insert_header("ETag", "\"m1\""),
        )
        .mount(&server)
        .await;

    let esi = esi(&server, &db);
    let token = Secret::new("the-token".to_owned());
    for _ in 0..2 {
        assert_eq!(
            esi.corporation_members(&token, CORP).await.unwrap(),
            [2112625428, 95465499]
        );
    }
    // Both went to ESI, and nothing was stored.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    assert!(rows(&db).await.is_empty());
}

fn entry(expires_in: Option<Duration>) -> CachedResponse {
    CachedResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: Bytes::from_static(b"{}"),
        etag: None,
        expires_at: expires_in.map(|d| SystemTime::now() + d),
    }
}

fn key(url: &str, principal: Option<&str>) -> CacheKey {
    CacheKey {
        url: url.to_owned(),
        principal: principal.map(str::to_owned),
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn entries_belong_to_their_principal(db: PgPool) {
    let cache = PgCache::new(db.clone());
    let url = "https://esi.evetech.net/corporations/1/assets";
    let one = key(url, Some("CHARACTER:EVE:1"));
    cache.put(&one, entry(Some(Duration::from_secs(60)))).await;

    assert!(cache.get(&one).await.is_some());
    assert!(cache.get(&key(url, None)).await.is_none());
    assert!(
        cache
            .get(&key(url, Some("CHARACTER:EVE:2")))
            .await
            .is_none()
    );
    // An empty principal would share the public key: never stored, and
    // never served the public entry.
    cache
        .put(&key(url, Some("")), entry(Some(Duration::from_secs(60))))
        .await;
    assert!(cache.get(&key(url, None)).await.is_none());
    cache
        .put(&key(url, None), entry(Some(Duration::from_secs(60))))
        .await;
    assert!(cache.get(&key(url, None)).await.is_some());
    assert!(cache.get(&key(url, Some(""))).await.is_none());

    cache.remove(&one).await;
    assert!(cache.get(&one).await.is_none());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pruning_drops_long_expired_entries_and_keeps_the_newest(db: PgPool) {
    let cache = PgCache::new(db.clone());
    for (url, expires_in) in [
        ("https://esi.test/fresh-1", Some(Duration::from_secs(600))),
        ("https://esi.test/fresh-2", Some(Duration::from_secs(600))),
        ("https://esi.test/etag-only", None),
        (
            "https://esi.test/long-expired",
            Some(Duration::from_secs(600)),
        ),
    ] {
        cache.put(&key(url, None), entry(expires_in)).await;
    }
    sqlx::query(
        "UPDATE core.esi_cache SET expires_at = now() - interval '2 days' \
         WHERE url = 'https://esi.test/long-expired'",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE core.esi_cache SET stored_at = now() - interval '1 hour' \
         WHERE url = 'https://esi.test/fresh-1'",
    )
    .execute(&db)
    .await
    .unwrap();

    assert_eq!(tether_esi::cache::prune(&db).await.unwrap(), 1);
    assert_eq!(rows(&db).await.len(), 3);

    // Over the cap, the least recently stored go first.
    assert_eq!(
        tether_db::esi_cache::prune(&db, 86_400.0, 2).await.unwrap(),
        1
    );
    let left: Vec<String> = rows(&db).await.into_iter().map(|(url, _)| url).collect();
    assert_eq!(
        left,
        ["https://esi.test/etag-only", "https://esi.test/fresh-2"]
    );
}
