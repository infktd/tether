use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use sqlx::PgPool;

const CHRIBBA: &str = "196379789:Chribba";
const CHRIBBA_ID: i64 = 196379789;
const MITTANI: &str = "443630591:The Mittani";
const MITTANI_ID: i64 = 443630591;

fn api(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE)
        .body(Body::empty())
        .unwrap()
}

async fn tokens(h: &Harness, session: &str) -> Vec<Value> {
    let res = send(&h.app, api("GET", "/api/tokens", session)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    serde_json::from_str::<Value>(&res.body)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn owners_see_refresh_and_delete_their_tokens(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    let other = log_in_as(&h, "90000009:Stranger", None).await;

    let mine = tokens(&h, &owner).await;
    assert_eq!(mine.len(), 2);
    assert_eq!(mine[0]["character_name"], "Chribba");
    assert_eq!(mine[0]["is_main"], true);
    assert_eq!(mine[1]["state"], "valid");
    // Never the token itself.
    assert!(
        !serde_json::to_string(&mine)
            .unwrap()
            .contains("refresh_token")
    );

    // Nobody else's.
    let theirs = send(
        &h.app,
        api("POST", &format!("/api/tokens/{MITTANI_ID}/refresh"), &other),
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    let theirs = send(
        &h.app,
        api("DELETE", &format!("/api/tokens/{MITTANI_ID}"), &other),
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);

    let page = page(&h, "/tokens", &owner).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("The Mittani") && page.body.contains("Working"));

    let refreshed = send(
        &h.app,
        api("POST", &format!("/api/tokens/{MITTANI_ID}/refresh"), &owner),
    )
    .await;
    assert_eq!(refreshed.body, r#"{"result":"valid"}"#);

    // Deleting wipes it; the character stays for now.
    let deleted = send(
        &h.app,
        form(&format!("/tokens/{MITTANI_ID}/delete"), "", &owner),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert!(deleted.body.contains("Token deleted"));
    let mine = tokens(&h, &owner).await;
    assert_eq!(mine[1]["state"], "deleted");
    assert_eq!(mine[1]["scopes"], serde_json::json!([]));
    let sealed: Vec<u8> = sqlx::query_scalar(
        "SELECT refresh_token FROM core.character_tokens WHERE character_id = $1",
    )
    .bind(MITTANI_ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(sealed.is_empty());
    assert_eq!(
        me(&h, &owner).await["characters"].as_array().unwrap().len(),
        2
    );
    let again = send(
        &h.app,
        api("DELETE", &format!("/api/tokens/{MITTANI_ID}"), &owner),
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'token.delete'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // A refresh of a dead token says so.
    let res = send(
        &h.app,
        api("POST", &format!("/api/tokens/{MITTANI_ID}/refresh"), &owner),
    )
    .await;
    assert_eq!(res.body, r#"{"result":"revoked"}"#);
    // Logging in with it again brings it back.
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(tokens(&h, &owner).await[1]["state"], "valid");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_refresh_catches_a_sale(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    h.sso
        .owner_hashes
        .lock()
        .unwrap()
        .insert(MITTANI_ID, "someone-else".into());
    let res = send(
        &h.app,
        api("POST", &format!("/api/tokens/{MITTANI_ID}/refresh"), &owner),
    )
    .await;
    assert_eq!(res.body, r#"{"result":"sold"}"#);
    let account = me(&h, &owner).await;
    assert_eq!(account["characters"].as_array().unwrap().len(), 1);
    assert_eq!(account["main"]["id"], CHRIBBA_ID);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deleting_a_sold_characters_token_doesnt_hide_the_sale(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    // A plugin call noticed the sale; the hourly sweep hasn't run yet.
    sqlx::query(
        "UPDATE core.character_tokens SET state = 'revoked', revoked_at = now(), \
         revoked_reason = 'owner hash changed' WHERE character_id = $1",
    )
    .bind(MITTANI_ID)
    .execute(&h.db)
    .await
    .unwrap();
    let res = send(
        &h.app,
        api("DELETE", &format!("/api/tokens/{MITTANI_ID}"), &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(checked.lost, 1);
    assert_eq!(
        me(&h, &owner).await["characters"].as_array().unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deleted_tokens_dont_trip_the_breaker(db: PgPool) {
    let h = harness(db, true).await;
    let mut owner = log_in_owner(&h, CHRIBBA).await;
    let alts: Vec<i64> = (0..7).map(|i| 90000100 + i).collect();
    for id in &alts {
        owner = log_in_as(&h, &format!("{id}:Alt {id}"), Some(&owner)).await;
    }
    for id in &alts {
        let res = send(&h.app, api("DELETE", &format!("/api/tokens/{id}"), &owner)).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    }
    sqlx::query("UPDATE core.character_tokens SET revoked_at = now() - interval '2 days' WHERE state = 'revoked'")
        .execute(&h.db)
        .await
        .unwrap();
    let lost = tether_web::ownership::sweep_dead(&h.db, false)
        .await
        .unwrap();
    assert_eq!(lost, alts.len());
    assert!(
        tether_db::settings::get(&h.db, tether_web::ownership::BREAKER_SETTING)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn refreshes_are_rate_limited(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let uri = format!("/api/tokens/{CHRIBBA_ID}/refresh");
    for _ in 0..10 {
        let res = send(&h.app, api("POST", &uri, &owner)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    }
    let res = send(&h.app, api("POST", &uri, &owner)).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}
