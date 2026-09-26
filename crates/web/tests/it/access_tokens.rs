//! Personal access tokens (F19): for bots and scripts, scoped, expiring,
//! stored hashed, and never more than their account may do.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

/// A script's request: the token, and no cookies or Origin.
fn with_token(method: Method, uri: &str, token: &str, body: Option<&str>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    match body {
        Some(json) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json.to_owned()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn make_token(h: &Harness, session: &str, body: &str) -> Res {
    send(&h.app, form("/dashboard/access-tokens", body, session)).await
}

fn token_from(page: &str) -> String {
    let start = page.find("tether_pat_").expect("the token is shown once");
    page[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tokens_do_only_what_they_carry(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let made = make_token(
        &h,
        &owner,
        "name=Audit+bot&days=30&scopes=account%3Aread&scopes=admin.audit&scopes=admin.groups",
    )
    .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let token = token_from(&made.body);
    // Stored hashed, never as typed.
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.personal_tokens WHERE position($1::bytea in token_hash) > 0",
    )
    .bind(token.as_bytes())
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(stored, 0);
    // Shown once: the list has only its prefix.
    let listed = page(&h, "/dashboard/access-tokens", &owner).await.body;
    assert!(!listed.contains(&token) && listed.contains("Audit bot"));

    let me = send(&h.app, with_token(Method::GET, "/api/me", &token, None)).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    let audit = send(
        &h.app,
        with_token(Method::GET, "/api/admin/audit", &token, None),
    )
    .await;
    assert_eq!(audit.status, StatusCode::OK, "{}", audit.body);
    // The owner holds admin.states; the token doesn't carry it.
    let states = send(
        &h.app,
        with_token(Method::GET, "/api/admin/states", &token, None),
    )
    .await;
    assert_eq!(states.status, StatusCode::FORBIDDEN);
    // A script's POST: no cookie, no Origin, yet allowed.
    let group = send(
        &h.app,
        with_token(
            Method::POST,
            "/api/admin/groups",
            &token,
            Some(r#"{"name":"Bots"}"#),
        ),
    )
    .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    // Never the account's own affairs, never pages.
    let main = send(
        &h.app,
        with_token(
            Method::POST,
            "/api/me/main",
            &token,
            Some(r#"{"character_id":196379789}"#),
        ),
    )
    .await;
    assert_eq!(main.status, StatusCode::FORBIDDEN);
    let page_by_token = send(
        &h.app,
        with_token(Method::GET, "/dashboard/access-tokens", &token, None),
    )
    .await;
    assert_ne!(page_by_token.status, StatusCode::OK);
    let tokens_api = send(&h.app, with_token(Method::GET, "/api/tokens", &token, None)).await;
    assert_ne!(tokens_api.status, StatusCode::OK);

    // Expired, then revoked: nothing.
    sqlx::query("UPDATE core.personal_tokens SET created_at = now() - interval '2 days', expires_at = now() - interval '1 day'")
        .execute(&h.db)
        .await
        .unwrap();
    let expired = send(&h.app, with_token(Method::GET, "/api/me", &token, None)).await;
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    let id: i64 = sqlx::query_scalar("SELECT id FROM core.personal_tokens")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let revoked = send(
        &h.app,
        form(&format!("/dashboard/access-tokens/{id}/revoke"), "", &owner),
    )
    .await;
    assert_eq!(revoked.location(), "/dashboard/access-tokens");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action LIKE 'access_token.%'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audited, 2);
    let log: String = sqlx::query_scalar(
        "SELECT details::text FROM core.audit_log WHERE action = 'access_token.create'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(!log.contains(&token), "the token never reaches the log");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_token_never_outgrows_its_account(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    // Not a scope the pilot holds.
    let refused = make_token(&h, &pilot, "name=Sneaky&days=30&scopes=admin.audit").await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    for (body, why) in [
        ("name=&days=30&scopes=account%3Aread", "Name the token"),
        ("name=X&days=400&scopes=account%3Aread", "1 to 365 days"),
        ("name=X&days=30", "Choose what"),
    ] {
        let res = make_token(&h, &pilot, body).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(res.body.contains(why), "{body}: {}", res.body);
    }
    let made = make_token(&h, &pilot, "name=Me&days=30&scopes=account%3Aread").await;
    let token = token_from(&made.body);
    assert_eq!(
        send(&h.app, with_token(Method::GET, "/api/me", &token, None))
            .await
            .status,
        StatusCode::OK
    );
    // Deactivated: the token stops too.
    let account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{account}/deactivate"),
            &owner,
            "",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    assert_eq!(
        send(&h.app, with_token(Method::GET, "/api/me", &token, None))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Garbage is simply unauthorized.
    assert_eq!(
        send(
            &h.app,
            with_token(Method::GET, "/api/me", "tether_pat_nope", None)
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_token_can_hand_out_only_what_it_carries(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    // A group that grants admin.permissions, made in the browser.
    let admins = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Admins"}"#),
    )
    .await;
    let admins: serde_json::Value = serde_json::from_str(&admins.body).unwrap();
    let admins = admins["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"admin.permissions","group_id":{admins}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);

    // The owner's bot token may manage groups, nothing else.
    let made = make_token(&h, &owner, "name=Groups+bot&days=30&scopes=admin.groups").await;
    let token = token_from(&made.body);
    // Adding someone to a group that grants what the token lacks: refused,
    // though the owner's account holds everything.
    let res = send(
        &h.app,
        with_token(
            Method::POST,
            &format!("/api/admin/groups/{admins}/members"),
            &token,
            Some(&format!(r#"{{"account_id":{pilot_account}}}"#)),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(
        me(&h, &pilot).await["groups"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Within reach, yes, and the audit log names the token.
    let plain = send(
        &h.app,
        with_token(
            Method::POST,
            "/api/admin/groups",
            &token,
            Some(r#"{"name":"Miners"}"#),
        ),
    )
    .await;
    assert_eq!(plain.status, StatusCode::CREATED, "{}", plain.body);
    let via: Option<i64> = sqlx::query_scalar(
        "SELECT (details->>'via_token')::bigint FROM core.audit_log WHERE action = 'group.create' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(via.is_some(), "the audit names the token");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reactivating_never_brings_tokens_back(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let made = make_token(&h, &pilot, "name=Me&days=30&scopes=account%3Aread").await;
    let token = token_from(&made.body);
    for action in ["deactivate", "reactivate"] {
        let res = send(
            &h.app,
            post_json(
                &format!("/api/admin/accounts/{account}/{action}"),
                &owner,
                "",
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{action}: {}", res.body);
    }
    assert_eq!(
        send(&h.app, with_token(Method::GET, "/api/me", &token, None))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}
