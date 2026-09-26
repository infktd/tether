#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)] // test code

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};
use tether_core::Secret;
use tether_db::settings;
use tether_esi::Esi;

use tether_core::crypto::EncryptionKey;
use tether_discord::{Discord, Endpoints};
use tether_esi::sso::{
    PendingLogin, RefreshFuture, Sso, SsoConfig, SsoError, SsoFuture, SsoIdentity, SsoTokens,
};
use tether_esi::vault::TokenVault;
use tether_web::{AppState, Site, router};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

pub const SITE: &str = "https://tether.test";

/// The built-in states' ids in a fresh database (migration 0021 inserts
/// them in this order).
pub const MEMBER_STATE: i64 = 1;
pub const BLUE_STATE: i64 = 2;
pub const GUEST_STATE: i64 = 3;

/// Makes a built-in state cover an alliance, corporation or character.
pub async fn cover(
    db: &sqlx::PgPool,
    state: tether_core::states::Builtin,
    kind: tether_core::states::EntityKind,
    entity_id: i64,
) {
    let target = tether_db::states::builtin(db, state)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::add_entity(
        db,
        target.id,
        kind,
        entity_id,
        &format!("entity {entity_id}"),
    )
    .await
    .unwrap();
}

/// The account's state's name, from `/api/me`.
pub async fn state_of(h: &Harness, token: &str) -> String {
    me(h, token).await["state"].as_str().unwrap().to_owned()
}

/// Stands in for CCP. `begin` issues a state and PKCE verifier; `finish`
/// accepts codes of the form `ok:<character_id>:<name>` and checks that the
/// verifier it receives is one it issued.
pub struct FakeSso {
    issued: Mutex<HashMap<String, String>>,
    pub seen_verifiers: Mutex<Vec<String>>,
    /// Lifetime of issued access tokens (zero: already due for refresh).
    pub token_ttl: Mutex<Duration>,
    pub refresh_outcome: Mutex<RefreshOutcome>,
    pub refresh_calls: AtomicUsize,
    pub refresh_tokens_seen: Mutex<Vec<String>>,
    /// Owner hash per character id; default `owner-<id>`. Change one to
    /// simulate the character moving to another EVE account.
    pub owner_hashes: Mutex<HashMap<i64, String>>,
    /// Scopes every login grants, besides those it asked for.
    pub granted_scopes: Mutex<Vec<String>>,
    /// Scopes each login asked for, by PKCE verifier.
    pub requested: Mutex<HashMap<String, Vec<String>>>,
    /// What the latest login asked for.
    pub last_requested: Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// New access token and a rotated refresh token.
    Rotate,
    /// New access token, same refresh token.
    Keep,
    Revoked,
    Unavailable,
}

impl Default for FakeSso {
    fn default() -> Self {
        Self {
            issued: Mutex::default(),
            seen_verifiers: Mutex::default(),
            token_ttl: Mutex::new(Duration::from_secs(20 * 60)),
            refresh_outcome: Mutex::new(RefreshOutcome::Rotate),
            refresh_calls: AtomicUsize::new(0),
            refresh_tokens_seen: Mutex::default(),
            owner_hashes: Mutex::default(),
            granted_scopes: Mutex::default(),
            requested: Mutex::default(),
            last_requested: Mutex::default(),
        }
    }
}

impl FakeSso {
    fn tokens(&self, access: String, refresh: Option<String>) -> SsoTokens {
        SsoTokens {
            access_token: Secret::new(access),
            refresh_token: refresh.map(Secret::new),
            expires_at: Some(SystemTime::now() + *self.token_ttl.lock().unwrap()),
            owner_hash: None,
        }
    }
}

impl Sso for FakeSso {
    fn begin(&self, config: &SsoConfig, scopes: &[String]) -> Result<PendingLogin, SsoError> {
        let mut issued = self.issued.lock().unwrap();
        let state = format!("state-{}", issued.len());
        let verifier = format!("verifier-{}", issued.len());
        issued.insert(state.clone(), verifier.clone());
        self.requested
            .lock()
            .unwrap()
            .insert(verifier.clone(), scopes.to_vec());
        *self.last_requested.lock().unwrap() = scopes.to_vec();
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
            self.seen_verifiers.lock().unwrap().push(verifier.clone());
            if !known {
                return Err(SsoError::Exchange("unknown PKCE verifier".into()));
            }
            let mut parts = code.splitn(3, ':');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("ok"), Some(id), Some(name)) => Ok(SsoIdentity {
                    character_id: id.parse().unwrap(),
                    character_name: name.to_owned(),
                    owner_hash: self
                        .owner_hashes
                        .lock()
                        .unwrap()
                        .get(&id.parse::<i64>().unwrap())
                        .cloned()
                        .unwrap_or_else(|| format!("owner-{id}")),
                    // What the login asked for, plus anything a test adds.
                    scopes: {
                        let mut scopes = self
                            .requested
                            .lock()
                            .unwrap()
                            .get(&verifier)
                            .cloned()
                            .unwrap_or_default();
                        scopes.extend(self.granted_scopes.lock().unwrap().iter().cloned());
                        scopes
                    },
                    tokens: self.tokens(
                        format!("access-{id}-login"),
                        Some(format!("refresh-{id}-1")),
                    ),
                }),
                _ => Err(SsoError::Exchange("invalid_grant".into())),
            }
        })
    }

    fn refresh<'a>(
        &'a self,
        _config: &'a SsoConfig,
        refresh_token: Secret<String>,
    ) -> RefreshFuture<'a> {
        Box::pin(async move {
            let n = self.refresh_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let seen = refresh_token.expose().clone();
            self.refresh_tokens_seen.lock().unwrap().push(seen.clone());
            // Let concurrent callers pile up behind the single refresh.
            tokio::time::sleep(Duration::from_millis(20)).await;
            let outcome = *self.refresh_outcome.lock().unwrap();
            // Refresh tokens are `refresh-<character id>-<n>`: the refreshed
            // token carries that character's current owner hash.
            let owner_hash = seen
                .split('-')
                .nth(1)
                .and_then(|id| id.parse::<i64>().ok())
                .map(|id| {
                    self.owner_hashes
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("owner-{id}"))
                });
            let with_owner = |mut t: SsoTokens| {
                t.owner_hash = owner_hash.clone();
                t
            };
            match outcome {
                RefreshOutcome::Rotate => {
                    let base = seen
                        .rsplit_once('-')
                        .map_or(seen.as_str(), |(b, _)| b)
                        .to_owned();
                    Ok(with_owner(self.tokens(
                        format!("access-refreshed-{n}"),
                        Some(format!("{base}-{}", n + 1)),
                    )))
                }
                RefreshOutcome::Keep => Ok(with_owner(
                    self.tokens(format!("access-refreshed-{n}"), None),
                )),
                RefreshOutcome::Revoked => Err(SsoError::Revoked("invalid_grant".into())),
                RefreshOutcome::Unavailable => Err(SsoError::Unavailable("timeout".into())),
            }
        })
    }
}

pub struct Harness {
    pub app: Router,
    pub db: PgPool,
    pub esi: Esi,
    pub sso: Arc<FakeSso>,
    pub vault: Arc<TokenVault>,
    /// Mock ESI; serves recorded fixtures.
    pub esi_server: MockServer,
    /// Mock Discord; tests mount what they need.
    pub discord_server: MockServer,
    pub discord: Arc<Discord>,
    pub key: EncryptionKey,
    pub plugins: Arc<tether_web::plugins::Plugins>,
}

/// Serves `tests/fixtures/esi/characters_affiliation.json`, filtered to the
/// ids in the request body like the real endpoint.
pub struct AffiliationFixture(Vec<serde_json::Value>);

impl AffiliationFixture {
    pub fn load() -> Self {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/esi/characters_affiliation.json"
        );
        Self(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
    }
}

impl Respond for AffiliationFixture {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let ids: Vec<i64> = serde_json::from_slice(&request.body).unwrap();
        let items: Vec<&serde_json::Value> = self
            .0
            .iter()
            .filter(|v| ids.contains(&v["character_id"].as_i64().unwrap()))
            .collect();
        ResponseTemplate::new(200).set_body_json(items)
    }
}

pub fn test_key() -> EncryptionKey {
    EncryptionKey::from_hex(&Secret::new(
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f".to_owned(),
    ))
    .unwrap()
}

pub const SETUP_TOKEN: &str = "test-setup-token-0123456789abcdef";

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../../tests/fixtures/esi/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

/// universe/ids and universe/names, served from recorded fixtures.
pub async fn mount_universe(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("universe_ids.json"), "application/json"),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("universe_names.json"), "application/json"),
        )
        .mount(server)
        .await;
}

pub async fn mount_affiliations(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(AffiliationFixture::load())
        .mount(server)
        .await;
}

pub async fn harness(db: PgPool, configured: bool) -> Harness {
    let esi_server = MockServer::start().await;
    mount_affiliations(&esi_server).await;
    mount_universe(&esi_server).await;
    harness_with_esi(db, configured, esi_server).await
}

pub async fn harness_with_esi(db: PgPool, configured: bool, esi_server: MockServer) -> Harness {
    harness_full(db, configured, esi_server, SITE).await
}

pub async fn harness_full(
    db: PgPool,
    configured: bool,
    esi_server: MockServer,
    site: &str,
) -> Harness {
    harness_parts(db, configured, esi_server, site, None).await
}

/// With snapshots before plugin migrations, as the server runs.
pub async fn harness_with_snapshots(
    db: PgPool,
    snapshots: Arc<tether_snapshots::Snapshots>,
) -> Harness {
    let esi_server = MockServer::start().await;
    mount_affiliations(&esi_server).await;
    mount_universe(&esi_server).await;
    harness_parts(db, true, esi_server, SITE, Some(snapshots)).await
}

async fn harness_parts(
    db: PgPool,
    configured: bool,
    esi_server: MockServer,
    site: &str,
    snapshots: Option<Arc<tether_snapshots::Snapshots>>,
) -> Harness {
    if configured {
        settings::set(&db, settings::SSO_CLIENT_ID, "client-123".into())
            .await
            .unwrap();
    }
    let sso = Arc::new(FakeSso::default());
    let esi = Esi::new("tether tests", Some(&esi_server.uri())).unwrap();
    let vault = Arc::new(TokenVault::new(
        db.clone(),
        test_key(),
        sso.clone(),
        format!("{site}/auth/callback"),
    ));
    let discord_server = MockServer::start().await;
    let discord = Arc::new(
        Discord::new(
            Endpoints::local(discord_server.address().to_string()),
            tether_net::Outbound::new(
                tether_net::Allowlist::production()
                    .with_local(&discord_server.address().to_string()),
                "tether tests",
                std::time::Duration::from_secs(10),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let plugins = tether_web::plugins::Plugins::new(
        tether_plugins::host::Host::new(Arc::new(tether_plugins::Runtime::new().unwrap())).unwrap(),
        tether_web::plugin_services::Deps {
            db: db.clone(),
            esi: esi.clone(),
            vault: vault.clone(),
            discord: discord.clone(),
            key: test_key(),
            public_url: site.to_owned(),
            snapshots,
        },
    );
    let app = router(AppState {
        key: test_key(),
        discord: discord.clone(),
        db: db.clone(),
        esi: esi.clone(),
        vault: vault.clone(),
        sso: sso.clone(),
        site: Arc::new(Site::new(site)),
        setup_token: Arc::new(Secret::new(SETUP_TOKEN.to_owned())),
        limits: Arc::default(),
        plugins: plugins.clone(),
        notices: tether_web::notifications::Notices::start(db.clone()),
    });
    Harness {
        app,
        db,
        esi,
        sso,
        vault,
        esi_server,
        discord_server,
        discord,
        key: test_key(),
        plugins,
    }
}

pub struct Res {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Res {
    pub fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
    }

    /// The raw Set-Cookie header for `name`.
    pub fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .find(|c| c.starts_with(&format!("{name}=")))
    }

    pub fn cookie_value(&self, name: &str) -> String {
        let raw = self.set_cookie(name).unwrap();
        raw[name.len() + 1..].split(';').next().unwrap().to_owned()
    }
}

pub async fn send(app: &Router, request: Request<Body>) -> Res {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Res {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

pub fn get(uri: &str, cookies: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::get(uri);
    if !cookies.is_empty() {
        let header: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        req = req.header(header::COOKIE, header.join("; "));
    }
    req.body(Body::empty()).unwrap()
}

pub fn query_param<'a>(url: &'a str, key: &str) -> &'a str {
    url.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .unwrap()
}

pub const LOGIN: &str = "__Host-tether_login";
pub const SESSION: &str = "__Host-tether_session";

/// Runs /auth/login and returns (state, login cookie value).
pub async fn start_login(h: &Harness, return_to: &str) -> (String, String) {
    let res = send(
        &h.app,
        get(&format!("/auth/login?return_to={return_to}"), &[]),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let state = query_param(res.location(), "state").to_owned();
    (state, res.cookie_value(LOGIN))
}

pub async fn session_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM core.sessions")
        .fetch_one(db)
        .await
        .unwrap()
}

pub async fn log_in(h: &Harness, existing_session: Option<&str>) -> String {
    log_in_as(h, "90000001:Pilot", existing_session).await
}

/// Logs in with `character` ("<id>:<name>"), optionally while signed in.
pub async fn log_in_as(h: &Harness, character: &str, existing_session: Option<&str>) -> String {
    let res = callback_as(h, character, existing_session).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

/// A login's SSO round trip. With a session, it's Add Character (as in
/// AA, a plain login never links an alt); without, a plain sign-in.
pub async fn callback_as(h: &Harness, character: &str, existing_session: Option<&str>) -> Res {
    let (state, browser) = match existing_session {
        Some(session) => start_add_character(h, session).await,
        None => start_login(h, "/").await,
    };
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

/// Starts Add Character for a signed-in account: (oauth state, login
/// cookie).
pub async fn start_add_character(h: &Harness, session: &str) -> (String, String) {
    let res = send(
        &h.app,
        Request::post("/register/start")
            .header(header::ORIGIN, SITE)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, format!("{SESSION}={session}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let state = query_param(res.location(), "state").to_owned();
    (state, res.cookie_value(LOGIN))
}

/// A plain sign-in while already signed in (switches accounts, or is
/// refused for an alt).
pub async fn sign_in_while_signed_in(h: &Harness, character: &str, session: &str) -> Res {
    let (state, browser) = start_login(h, "/").await;
    send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, browser.as_str()), (SESSION, session)],
        ),
    )
    .await
}

pub fn post(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut req = Request::post(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.body(Body::empty()).unwrap()
}

pub async fn me(h: &Harness, token: &str) -> serde_json::Value {
    let res = send(&h.app, get("/api/me", &[(SESSION, token)])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    serde_json::from_str(&res.body).unwrap()
}

pub fn post_json(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

pub const SETUP: &str = "__Host-tether_setup";

/// Unlocks the wizard with the setup token; returns the setup cookie value.
pub async fn unlock(h: &Harness) -> String {
    let res = send(
        &h.app,
        Request::post("/api/setup/unlock")
            .header(header::ORIGIN, SITE)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(format!(r#"{{"token":"{SETUP_TOKEN}"}}"#)))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    res.cookie_value(SETUP)
}

/// Logs in from an unlocked browser, which claims ownership.
pub async fn log_in_owner(h: &Harness, character: &str) -> String {
    let setup = unlock(h).await;
    let (state, browser) = start_login(h, "/").await;
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, &browser), (SETUP, &setup)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

// ---- plugins -----------------------------------------------------------------

/// Builds a wasm32-wasip2 workspace crate into its own target directory
/// (so it doesn't wait on the lock of the build running the tests) and
/// returns the component.
pub fn build_guest(package: &str) -> Vec<u8> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target = root.join("target/test-guests");
    let output = std::process::Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "build",
            "-p",
            package,
            "--target",
            "wasm32-wasip2",
            "--release",
        ])
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .expect("running cargo");
    assert!(
        output.status.success(),
        "building {package}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let file = format!("wasm32-wasip2/release/{}.wasm", package.replace('-', "_"));
    std::fs::read(target.join(file)).unwrap()
}

pub const BOUNDARY: &str = "tether-test-boundary";

pub fn multipart(fields: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, data) in fields {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"; \
                 filename=\"{name}.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

pub fn upload_request(token: &str, body: Vec<u8>) -> Request<Body> {
    Request::post("/admin/plugins")
        .header(header::ORIGIN, SITE)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .body(Body::from(body))
        .unwrap()
}

pub async fn upload(h: &Harness, token: &str, package: &[u8], signature: &str) -> Res {
    let body = multipart(&[("package", package), ("signature", signature.as_bytes())]);
    send(&h.app, upload_request(token, body)).await
}

pub fn form(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

pub async fn page(h: &Harness, uri: &str, token: &str) -> Res {
    send(&h.app, get(uri, &[(SESSION, token)])).await
}

/// Uploads and approves a signed package; returns where approving went.
pub async fn install_package(h: &Harness, token: &str, package: &[u8], signature: &str) -> String {
    let res = upload(h, token, package, signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review = res.location().to_owned();
    let res = send(&h.app, form(&format!("{review}/approve"), "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.location().to_owned()
}

/// Runs the storage probe guest (`tether-plugins-test-guest-storage`) and
/// returns what it reports. Through `submit` (writes and jobs allowed) or,
/// with `as_page`, a page render (read-only).
pub async fn run_probe(
    h: &Harness,
    id: &str,
    path: &str,
    query: Vec<(String, String)>,
    as_page: bool,
) -> String {
    use tether_plugins::host::{Request as PageRequest, Section, Submission, SubmitResult};
    let plugin = h.plugins.get(id).expect("the plugin is running");
    let request = PageRequest {
        path: path.to_owned(),
        query,
    };
    let page = if as_page {
        h.plugins
            .host()
            .render(&plugin, request, &Default::default())
            .await
            .unwrap()
            .page
    } else {
        let submitted = h
            .plugins
            .host()
            .submit(
                &plugin,
                Submission {
                    request,
                    form: "probe".to_owned(),
                    values: Vec::new(),
                },
                &Default::default(),
            )
            .await
            .unwrap();
        match submitted.result {
            SubmitResult::Page(page) => page,
            SubmitResult::Redirect(to) => panic!("redirect to {to}"),
        }
    };
    match &page.sections[0] {
        Section::Text(text) => text.clone(),
        other => panic!("{other:?}"),
    }
}

// ---- Discord, set up for plugins ------------------------------------------------

pub const DISCORD_GUILD: &str = "222222222222222222";
pub const DISCORD_PING_CHANNEL: &str = "600000000000000001";
pub const DISCORD_MEMBER_ROLE: &str = "500000000000000003";

fn discord_fixture(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/../../tests/fixtures/discord/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Discord set up (bot, server, one ping channel, the Member role mapped)
/// by `owner`, with the mock bot answering what that takes.
pub async fn discord_ready(h: &Harness, owner: &str) {
    let ok = |name: &str| ResponseTemplate::new(200).set_body_json(discord_fixture(name));
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .respond_with(ok("bot_user"))
        .mount(&h.discord_server)
        .await;
    for (route, name) in [
        (format!("/api/v10/guilds/{DISCORD_GUILD}"), "guild"),
        (format!("/api/v10/guilds/{DISCORD_GUILD}/roles"), "roles"),
        (
            format!("/api/v10/guilds/{DISCORD_GUILD}/members/111111111111111111"),
            "bot_member",
        ),
        (
            format!("/api/v10/guilds/{DISCORD_GUILD}/channels"),
            "channels",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ok(name))
            .mount(&h.discord_server)
            .await;
    }
    let settings = "application_id=111111111111111111&guild_id=222222222222222222\
                    &client_secret=client-secret-value&bot_token=bot-token-value";
    for (uri, body) in [
        ("/admin/discord", settings.to_owned()),
        (
            "/admin/discord/channels",
            format!("channel_id={DISCORD_PING_CHANNEL}"),
        ),
        (
            "/admin/discord/mappings",
            format!("role_id={DISCORD_MEMBER_ROLE}&grantee=state%3A{MEMBER_STATE}"),
        ),
    ] {
        let res = send(&h.app, form(uri, &body, owner)).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{uri}: {}", res.body);
    }
}
