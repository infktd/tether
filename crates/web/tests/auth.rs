#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use sqlx::PgPool;
use tether_core::Secret;
use tether_db::settings;
use tether_esi::sso::{PendingLogin, Sso, SsoConfig, SsoError, SsoFuture, SsoIdentity};
use tether_web::{AppState, Site, router};
use tower::ServiceExt;

const SITE: &str = "https://tether.test";

/// Stands in for CCP. `begin` issues a state and PKCE verifier; `finish`
/// accepts codes of the form `ok:<character_id>:<name>` and checks that the
/// verifier it receives is one it issued.
#[derive(Default)]
struct FakeSso {
    issued: Mutex<HashMap<String, String>>,
    seen_verifiers: Mutex<Vec<String>>,
}

impl Sso for FakeSso {
    fn begin(&self, config: &SsoConfig) -> Result<PendingLogin, SsoError> {
        let mut issued = self.issued.lock().unwrap();
        let state = format!("state-{}", issued.len());
        let verifier = format!("verifier-{}", issued.len());
        issued.insert(state.clone(), verifier.clone());
        Ok(PendingLogin {
            authorize_url: format!(
                "https://login.test/authorize?client_id={}&redirect_uri={}&state={state}",
                config.client_id, config.redirect_uri
            ),
            state,
            pkce_verifier: Secret::new(verifier),
        })
    }

    fn finish<'a>(
        &'a self,
        _config: &'a SsoConfig,
        code: String,
        pkce_verifier: Secret<String>,
    ) -> SsoFuture<'a> {
        Box::pin(async move {
            let verifier = pkce_verifier.expose().clone();
            let known = self.issued.lock().unwrap().values().any(|v| *v == verifier);
            self.seen_verifiers.lock().unwrap().push(verifier);
            if !known {
                return Err(SsoError::Exchange("unknown PKCE verifier".into()));
            }
            let mut parts = code.splitn(3, ':');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("ok"), Some(id), Some(name)) => Ok(SsoIdentity {
                    character_id: id.parse().unwrap(),
                    character_name: name.to_owned(),
                }),
                _ => Err(SsoError::Exchange("invalid_grant".into())),
            }
        })
    }
}

struct Harness {
    app: Router,
    db: PgPool,
    sso: Arc<FakeSso>,
}

async fn harness(db: PgPool, configured: bool) -> Harness {
    if configured {
        settings::set(&db, settings::SSO_CLIENT_ID, "client-123".into())
            .await
            .unwrap();
    }
    let sso = Arc::new(FakeSso::default());
    let app = router(AppState {
        db: db.clone(),
        sso: sso.clone(),
        site: Arc::new(Site::new(SITE)),
    });
    Harness { app, db, sso }
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Res {
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
    }

    /// The raw Set-Cookie header for `name`.
    fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .find(|c| c.starts_with(&format!("{name}=")))
    }

    fn cookie_value(&self, name: &str) -> String {
        let raw = self.set_cookie(name).unwrap();
        raw[name.len() + 1..].split(';').next().unwrap().to_owned()
    }
}

async fn send(app: &Router, request: Request<Body>) -> Res {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 16).await.unwrap();
    Res {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

fn get(uri: &str, cookies: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::get(uri);
    if !cookies.is_empty() {
        let header: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        req = req.header(header::COOKIE, header.join("; "));
    }
    req.body(Body::empty()).unwrap()
}

fn query_param<'a>(url: &'a str, key: &str) -> &'a str {
    url.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .unwrap()
}

const LOGIN: &str = "__Host-tether_login";
const SESSION: &str = "__Host-tether_session";

/// Runs /auth/login and returns (state, login cookie value).
async fn start_login(h: &Harness, return_to: &str) -> (String, String) {
    let res = send(
        &h.app,
        get(&format!("/auth/login?return_to={return_to}"), &[]),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let state = query_param(res.location(), "state").to_owned();
    (state, res.cookie_value(LOGIN))
}

async fn session_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM core.sessions")
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn login_is_unavailable_until_sso_is_configured(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, get("/auth/login", &[])).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn login_redirects_to_sso_with_a_bound_browser_cookie(db: PgPool) {
    let h = harness(db, true).await;
    let res = send(&h.app, get("/auth/login", &[])).await;

    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.location().starts_with("https://login.test/authorize"));
    assert!(res.location().contains("client_id=client-123"));
    assert!(
        res.location()
            .contains("redirect_uri=https://tether.test/auth/callback")
    );
    let cookie = res.set_cookie(LOGIN).unwrap();
    for attr in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=600",
    ] {
        assert!(cookie.contains(attr), "{cookie} lacks {attr}");
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn full_login_creates_a_session(db: PgPool) {
    let h = harness(db, true).await;
    let (state, browser) = start_login(&h, "/profile").await;

    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:90000001:Jita%20Trader&state={state}"),
            &[(LOGIN, &browser)],
        ),
    )
    .await;

    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/profile");
    let cookie = res.set_cookie(SESSION).unwrap();
    for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
        assert!(cookie.contains(attr), "{cookie} lacks {attr}");
    }
    // The login cookie is cleared.
    assert!(res.set_cookie(LOGIN).unwrap().contains("Max-Age=0"));
    // PKCE: the verifier issued at login reached the token exchange.
    assert_eq!(*h.sso.seen_verifiers.lock().unwrap(), vec!["verifier-0"]);

    let token = res.cookie_value(SESSION);
    let me = send(&h.app, get("/api/me", &[(SESSION, &token)])).await;
    assert_eq!(me.status, StatusCode::OK);
    let me: serde_json::Value = serde_json::from_str(&me.body).unwrap();
    assert_eq!(me["main"]["id"], 90000001);
    assert_eq!(me["main"]["name"], "Jita Trader");
    assert_eq!(me["is_owner"], true);

    // Only a hash of the token is stored.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM core.sessions")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(stored, tether_core::hash_token(&token));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn callback_rejects_wrong_state_foreign_browser_and_replay(db: PgPool) {
    let h = harness(db, true).await;
    let (state, browser) = start_login(&h, "/").await;
    let callback = |state: &str| format!("/auth/callback?code=ok:1:A&state={state}");

    let wrong_state = send(&h.app, get(&callback("forged"), &[(LOGIN, &browser)])).await;
    assert_eq!(wrong_state.status, StatusCode::BAD_REQUEST);

    // Login CSRF: someone else's browser presenting a valid state.
    let no_cookie = send(&h.app, get(&callback(&state), &[])).await;
    assert_eq!(no_cookie.status, StatusCode::BAD_REQUEST);
    let other_browser = send(&h.app, get(&callback(&state), &[(LOGIN, "attacker")])).await;
    assert_eq!(other_browser.status, StatusCode::BAD_REQUEST);

    // The real browser still succeeds, but only once.
    let ok = send(&h.app, get(&callback(&state), &[(LOGIN, &browser)])).await;
    assert_eq!(ok.status, StatusCode::SEE_OTHER);
    let replay = send(&h.app, get(&callback(&state), &[(LOGIN, &browser)])).await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST);
    assert_eq!(session_count(&h.db).await, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn expired_login_attempt_is_rejected(db: PgPool) {
    let h = harness(db, true).await;
    let (state, browser) = start_login(&h, "/").await;
    sqlx::query("UPDATE core.login_attempts SET expires_at = now() - interval '1 second'")
        .execute(&h.db)
        .await
        .unwrap();

    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:1:A&state={state}"),
            &[(LOGIN, &browser)],
        ),
    )
    .await;

    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn failed_exchange_is_a_bad_gateway_and_consumes_the_attempt(db: PgPool) {
    let h = harness(db, true).await;
    let (state, browser) = start_login(&h, "/").await;

    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=garbage&state={state}"),
            &[(LOGIN, &browser)],
        ),
    )
    .await;

    assert_eq!(res.status, StatusCode::BAD_GATEWAY);
    assert_eq!(session_count(&h.db).await, 0);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.login_attempts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sso_error_parameter_is_reported(db: PgPool) {
    let h = harness(db, true).await;
    let res = send(&h.app, get("/auth/callback?error=access_denied", &[])).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("cancelled"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn open_redirect_return_to_falls_back_to_root(db: PgPool) {
    let h = harness(db, true).await;
    let (state, browser) = start_login(&h, "//evil.example").await;
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:1:A&state={state}"),
            &[(LOGIN, &browser)],
        ),
    )
    .await;
    assert_eq!(res.location(), "/");
}

async fn log_in(h: &Harness, existing_session: Option<&str>) -> String {
    log_in_as(h, "90000001:Pilot", existing_session).await
}

/// Logs in with `character` ("<id>:<name>"), optionally while signed in.
async fn log_in_as(h: &Harness, character: &str, existing_session: Option<&str>) -> String {
    let res = callback_as(h, character, existing_session).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

async fn callback_as(h: &Harness, character: &str, existing_session: Option<&str>) -> Res {
    let (state, browser) = start_login(h, "/").await;
    let mut cookies = vec![(LOGIN, browser.as_str())];
    if let Some(s) = existing_session {
        cookies.push((SESSION, s));
    }
    send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &cookies,
        ),
    )
    .await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn logging_in_again_rotates_the_session(db: PgPool) {
    let h = harness(db, true).await;
    let first = log_in(&h, None).await;
    let second = log_in(&h, Some(&first)).await;

    assert_ne!(first, second);
    assert_eq!(session_count(&h.db).await, 1);
    let old = send(&h.app, get("/api/me", &[(SESSION, &first)])).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = send(&h.app, get("/api/me", &[(SESSION, &second)])).await;
    assert_eq!(new.status, StatusCode::OK);
}

fn post(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::post(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.body(Body::empty()).unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn logout_requires_same_origin(db: PgPool) {
    let h = harness(db, true).await;
    let token = log_in(&h, None).await;
    let cookie = format!("{SESSION}={token}");

    for headers in [
        vec![("cookie", cookie.as_str())],
        vec![
            ("cookie", cookie.as_str()),
            ("origin", "https://evil.example"),
        ],
        vec![
            ("cookie", cookie.as_str()),
            ("sec-fetch-site", "cross-site"),
        ],
    ] {
        let res = send(&h.app, post("/auth/logout", &headers)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{headers:?}");
    }
    assert_eq!(session_count(&h.db).await, 1);

    let res = send(
        &h.app,
        post("/auth/logout", &[("cookie", &cookie), ("origin", SITE)]),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(res.set_cookie(SESSION).unwrap().contains("Max-Age=0"));
    assert_eq!(session_count(&h.db).await, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn same_origin_fetch_without_origin_header_is_allowed(db: PgPool) {
    let h = harness(db, true).await;
    let res = send(
        &h.app,
        post("/auth/logout", &[("sec-fetch-site", "same-origin")]),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn me_requires_a_live_session(db: PgPool) {
    let h = harness(db, true).await;
    assert_eq!(
        send(&h.app, get("/api/me", &[])).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&h.app, get("/api/me", &[(SESSION, "not-a-session")]))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    let token = log_in(&h, None).await;
    sqlx::query("UPDATE core.sessions SET expires_at = now() - interval '1 second'")
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        send(&h.app, get("/api/me", &[(SESSION, &token)]))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

async fn me(h: &Harness, token: &str) -> serde_json::Value {
    let res = send(&h.app, get("/api/me", &[(SESSION, token)])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    serde_json::from_str(&res.body).unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn logging_in_with_another_character_while_signed_in_adds_an_alt(db: PgPool) {
    let h = harness(db, true).await;
    let main = log_in_as(&h, "90000001:Main Pilot", None).await;
    let after_alt = log_in_as(&h, "90000002:Alt Pilot", Some(&main)).await;

    insta::assert_json_snapshot!("me_with_alt", me(&h, &after_alt).await);

    // Logging in later with just the alt reaches the same account.
    let via_alt = log_in_as(&h, "90000002:Alt Pilot", None).await;
    assert_eq!(me(&h, &via_alt).await["main"]["id"], 90000001);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn character_linked_elsewhere_is_refused_and_session_kept(db: PgPool) {
    let h = harness(db, true).await;
    log_in_as(&h, "90000001:Someone Else", None).await;
    let mine = log_in_as(&h, "90000002:Mine", None).await;

    let res = callback_as(&h, "90000001:Someone Else", Some(&mine)).await;

    assert_eq!(res.status, StatusCode::CONFLICT);
    assert!(res.set_cookie(SESSION).is_none());
    let me = me(&h, &mine).await;
    assert_eq!(me["characters"].as_array().unwrap().len(), 1);
    assert_eq!(me["is_owner"], false);
}

fn post_json(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn main_can_be_switched_to_an_own_character(db: PgPool) {
    let h = harness(db, true).await;
    let token = log_in_as(&h, "90000001:Main", None).await;
    let token = log_in_as(&h, "90000002:Alt", Some(&token)).await;
    log_in_as(&h, "90000003:Stranger", None).await;

    let res = send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":90000002}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    assert_eq!(me(&h, &token).await["main"]["id"], 90000002);

    let res = send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":90000003}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(me(&h, &token).await["main"]["id"], 90000002);
}
