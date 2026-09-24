#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

fn json(res: &Res) -> Value {
    serde_json::from_str(&res.body).unwrap_or_else(|_| panic!("not JSON: {}", res.body))
}

async fn status(h: &Harness, cookies: &[(&str, &str)]) -> Value {
    let res = send(&h.app, get("/api/setup", cookies)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    json(&res)
}

/// POST with JSON from our own origin, carrying the given cookies.
fn post(uri: &str, body: &str, cookies: &[(&str, &str)], origin: &str) -> Request<Body> {
    let cookie: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Request::post(uri)
        .header(header::ORIGIN, origin)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie.join("; "))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn audit_actions(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM core.audit_log ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_wizard_from_fresh_install_to_complete(db: PgPool) {
    let h = harness(db, false).await;

    let s = status(&h, &[]).await;
    assert_eq!(s["state"], "needs_sso");
    assert_eq!(s["callback_url"], "https://tether.test/auth/callback");
    assert_eq!(s["unlocked"], false);

    // Step 1: the setup token.
    let wrong = send(
        &h.app,
        post("/api/setup/unlock", r#"{"token":"guess"}"#, &[], SITE),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let no_token = send(
        &h.app,
        post(
            "/api/setup/sso",
            r#"{"client_id":"0123456789abcdef"}"#,
            &[],
            SITE,
        ),
    )
    .await;
    assert_eq!(no_token.status, StatusCode::UNAUTHORIZED);

    let setup = unlock(&h).await;
    assert_eq!(status(&h, &[(SETUP, &setup)]).await["unlocked"], true);

    // Step 2: the SSO client id.
    let bad = send(
        &h.app,
        post(
            "/api/setup/sso",
            r#"{"client_id":"nope"}"#,
            &[(SETUP, &setup)],
            SITE,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let ok = send(
        &h.app,
        post(
            "/api/setup/sso",
            r#"{"client_id":"0123456789abcdef0123456789abcdef"}"#,
            &[(SETUP, &setup)],
            SITE,
        ),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(
        json(&ok)["callback_url"],
        "https://tether.test/auth/callback"
    );
    assert_eq!(status(&h, &[]).await["state"], "needs_owner");

    // Step 3: log in from the unlocked browser; that account is the owner.
    let (state, browser) = start_login(&h, "/").await;
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:196379789:Chribba&state={state}"),
            &[(LOGIN, &browser), (SETUP, &setup)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.set_cookie(SETUP).unwrap().contains("Max-Age=0"));
    let owner = res.cookie_value(SESSION);
    assert_eq!(me(&h, &owner).await["is_owner"], true);

    // The token step is closed for good, and the old setup cookie is dead.
    let s = status(&h, &[(SESSION, &owner), (SETUP, &setup)]).await;
    assert_eq!(s["state"], "needs_alliance");
    assert_eq!(s["unlocked"], false);
    let late = send(
        &h.app,
        post(
            "/api/setup/unlock",
            &format!(r#"{{"token":"{SETUP_TOKEN}"}}"#),
            &[],
            SITE,
        ),
    )
    .await;
    assert_eq!(late.status, StatusCode::GONE);
    let stale = send(
        &h.app,
        post(
            "/api/setup/sso",
            r#"{"client_id":"0123456789abcdef"}"#,
            &[(SETUP, &setup)],
            SITE,
        ),
    )
    .await;
    assert_eq!(stale.status, StatusCode::UNAUTHORIZED);

    // Step 4: the owner's own alliance is suggested; choose it.
    let suggested = &s["suggested"];
    assert_eq!(
        *suggested,
        json!({"id": 159826257, "name": "Otherworld Empire", "kind": "alliance"})
    );
    let set = send(
        &h.app,
        post_json(
            "/api/admin/tiers",
            &owner,
            r#"{"entity_id":159826257,"tier":"member"}"#,
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.body);
    assert_eq!(status(&h, &[]).await["state"], "complete");

    assert_eq!(
        audit_actions(&h.db).await,
        ["setup.unlock", "setup.sso", "setup.owner", "tier.rule.set"]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn logging_in_without_the_setup_session_does_not_claim_ownership(db: PgPool) {
    let h = harness(db, true).await;
    let early = log_in_as(&h, "90000001:Early Bird", None).await;

    assert_eq!(me(&h, &early).await["is_owner"], false);
    assert_eq!(status(&h, &[]).await["state"], "needs_owner");

    // The real admin, from the unlocked browser, still can.
    let admin = log_in_owner(&h, "90000002:Admin").await;
    assert_eq!(me(&h, &admin).await["is_owner"], true);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unlock_is_rate_limited_per_ip(db: PgPool) {
    let h = harness(db, false).await;
    let attempt = |ip: &'static str, token: &'static str| {
        let app = h.app.clone();
        async move {
            let req = Request::post("/api/setup/unlock")
                .header(header::ORIGIN, SITE)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
                .unwrap();
            app.oneshot(req).await.unwrap()
        }
    };

    for _ in 0..5 {
        assert_eq!(
            attempt("203.0.113.7", "guess").await.status(),
            StatusCode::FORBIDDEN
        );
    }
    let limited = attempt("203.0.113.7", SETUP_TOKEN).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key(header::RETRY_AFTER));
    // Other clients are unaffected.
    assert_eq!(
        attempt("198.51.100.1", SETUP_TOKEN).await.status(),
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tier_rules_are_admin_only_validated_and_re_evaluate_everyone(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    assert_eq!(me(&h, &owner).await["tier"], "guest");

    let forbidden = send(
        &h.app,
        post_json(
            "/api/admin/tiers",
            &pilot,
            r#"{"entity_id":159826257,"tier":"member"}"#,
        ),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let bad_tier = send(
        &h.app,
        post_json(
            "/api/admin/tiers",
            &owner,
            r#"{"entity_id":159826257,"tier":"guest"}"#,
        ),
    )
    .await;
    assert_eq!(bad_tier.status, StatusCode::BAD_REQUEST);
    let unknown = send(
        &h.app,
        post_json(
            "/api/admin/tiers",
            &owner,
            r#"{"entity_id":5,"tier":"member"}"#,
        ),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let resolved = send(
        &h.app,
        post_json(
            "/api/admin/tiers/resolve",
            &owner,
            r#"{"names":["Goonswarm Federation","GoonWaffe"]}"#,
        ),
    )
    .await;
    let resolved = json(&resolved);
    assert_eq!(
        resolved["alliances"][0],
        json!({"id": 1354830081, "name": "Goonswarm Federation"})
    );
    assert_eq!(
        resolved["corporations"][0],
        json!({"id": 667531913, "name": "GoonWaffe"})
    );

    let set = send(
        &h.app,
        post_json(
            "/api/admin/tiers",
            &owner,
            r#"{"entity_id":159826257,"tier":"member"}"#,
        ),
    )
    .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT);
    let rules = send(&h.app, get("/api/admin/tiers", &[(SESSION, &owner)])).await;
    assert_eq!(
        json(&rules),
        json!([{"entity_id": 159826257, "kind": "alliance", "tier": "member", "name": "Otherworld Empire"}])
    );

    // The change queued a re-evaluation of every account; run it.
    let mut registry = tether_jobs::Registry::new();
    tether_web::tiers::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    let config = tether_jobs::WorkerConfig::default();
    while tether_jobs::run_once(&h.db, &registry, &config)
        .await
        .unwrap()
        != tether_jobs::Outcome::Idle
    {}
    assert_eq!(me(&h, &owner).await["tier"], "member");
    assert_eq!(me(&h, &pilot).await["tier"], "guest");

    let remove = send(
        &h.app,
        Request::delete("/api/admin/tiers/159826257")
            .header(header::ORIGIN, SITE)
            .header(header::COOKIE, format!("{SESSION}={owner}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(remove.status, StatusCode::NO_CONTENT);
    while tether_jobs::run_once(&h.db, &registry, &config)
        .await
        .unwrap()
        != tether_jobs::Outcome::Idle
    {}
    assert_eq!(me(&h, &owner).await["tier"], "guest");
}

/// The callback check fetches the public URL for real, so serve the app on
/// a local port and point PUBLIC_URL at it.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn callback_check_reaches_this_instance(db: PgPool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site = format!("http://{}", listener.local_addr().unwrap());
    let esi = wiremock::MockServer::start().await;
    let h = harness_full(db, false, esi, &site).await;
    tokio::spawn(axum::serve(listener, h.app.clone()).into_future());

    let setup = {
        let res = send(
            &h.app,
            post(
                "/api/setup/unlock",
                &format!(r#"{{"token":"{SETUP_TOKEN}"}}"#),
                &[],
                &site,
            ),
        )
        .await;
        res.cookie_value(SETUP)
    };
    let res = send(
        &h.app,
        post("/api/setup/callback-check", "", &[(SETUP, &setup)], &site),
    )
    .await;

    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let out = json(&res);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["url"], site);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn callback_check_reports_an_unreachable_url(db: PgPool) {
    let site = "http://127.0.0.1:1";
    let esi = wiremock::MockServer::start().await;
    let h = harness_full(db, false, esi, site).await;
    let unlock = send(
        &h.app,
        post(
            "/api/setup/unlock",
            &format!(r#"{{"token":"{SETUP_TOKEN}"}}"#),
            &[],
            site,
        ),
    )
    .await;
    let setup = unlock.cookie_value(SETUP);

    let res = send(
        &h.app,
        post("/api/setup/callback-check", "", &[(SETUP, &setup)], site),
    )
    .await;

    let out = json(&res);
    assert_eq!(out["ok"], false);
    assert!(
        out["detail"]
            .as_str()
            .unwrap()
            .starts_with("Could not reach"),
        "{out}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn probe_only_echoes_safe_nonces(db: PgPool) {
    let h = harness(db, false).await;
    let ok = send(&h.app, get("/api/setup/probe?nonce=abc123", &[])).await;
    assert_eq!(ok.body, "tether-probe:abc123");
    let bad = send(&h.app, get("/api/setup/probe?nonce=%3Cscript%3E", &[])).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}
