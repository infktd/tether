#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use sqlx::PgPool;

fn form(uri: &str, body: &str, cookies: &[(&str, &str)]) -> Request<Body> {
    let cookie: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Request::post(uri)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, cookie.join("; "))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn htmx(mut req: Request<Body>) -> Request<Body> {
    req.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    req
}

/// Every absolute URL a page points the browser at must be CCP's image
/// server or, for the setup instructions, CCP's developer site.
fn assert_only_allowed_external_urls(html: &str) {
    for (i, _) in html.match_indices("http") {
        let rest = &html[i..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            continue;
        }
        let url: String = rest
            .chars()
            .take_while(|c| !matches!(c, '"' | '\'' | '<' | ' '))
            .collect();
        let allowed = url.starts_with("https://images.evetech.net/")
            || url.starts_with("https://developers.eveonline.com/")
            || url.starts_with("https://tether.test/");
        assert!(allowed, "unexpected external URL in page: {url}");
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn home_sends_visitors_where_they_belong(db: PgPool) {
    let h = harness(db, true).await;
    assert_eq!(send(&h.app, get("/", &[])).await.location(), "/setup");

    let owner = log_in_owner(&h, "196379789:Chribba").await;
    assert_eq!(send(&h.app, get("/", &[])).await.location(), "/login");
    assert_eq!(
        send(&h.app, get("/", &[(SESSION, &owner)]))
            .await
            .location(),
        "/profile"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn login_page(db: PgPool) {
    let h = harness(db, true).await;
    let res = send(&h.app, get("/login", &[])).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains(r#"href="/auth/login""#));
    assert!(res.body.contains("Log in with EVE Online"));
    assert_only_allowed_external_urls(&res.body);

    let token = log_in_owner(&h, "196379789:Chribba").await;
    assert_eq!(
        send(&h.app, get("/login", &[(SESSION, &token)]))
            .await
            .location(),
        "/profile"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn profile_shows_characters_tier_and_permissions(db: PgPool) {
    let h = harness(db, true).await;
    assert_eq!(
        send(&h.app, get("/profile", &[])).await.location(),
        "/login"
    );

    let token = log_in_owner(&h, "196379789:Chribba").await;
    let token = log_in_as(&h, "443630591:The Mittani", Some(&token)).await;
    let res = send(&h.app, get("/profile", &[(SESSION, &token)])).await;

    assert_eq!(res.status, StatusCode::OK);
    let html = &res.body;
    assert!(html.contains("<h1 class=\"page-title\">Profile</h1>"));
    assert!(html.contains("https://images.evetech.net/characters/196379789/portrait?size=64"));
    assert!(html.contains("The Mittani"));
    assert!(html.contains(r#"data-tier="guest""#));
    assert!(html.contains("admin.tiers"), "owner sees their permissions");
    assert!(html.contains(r#"hx-post="/profile/main""#));
    assert!(html.contains(r#"aria-current="page""#));
    assert_only_allowed_external_urls(html);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn make_main_returns_the_characters_fragment(db: PgPool) {
    let h = harness(db, true).await;
    let token = log_in_owner(&h, "196379789:Chribba").await;
    let token = log_in_as(&h, "443630591:The Mittani", Some(&token)).await;

    let res = send(
        &h.app,
        htmx(form(
            "/profile/main",
            "character_id=443630591",
            &[(SESSION, &token)],
        )),
    )
    .await;

    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body
            .trim_start()
            .starts_with(r#"<section class="card data-card" id="characters">"#),
        "{}",
        res.body
    );
    assert!(!res.body.contains("<html"), "a fragment, not a page");
    assert_eq!(me(&h, &token).await["main"]["id"], 443630591);

    // Without htmx, back to the page.
    let plain = send(
        &h.app,
        form(
            "/profile/main",
            "character_id=196379789",
            &[(SESSION, &token)],
        ),
    )
    .await;
    assert_eq!(plain.location(), "/profile");

    // A foreign character is refused in the fragment.
    let foreign = send(
        &h.app,
        htmx(form(
            "/profile/main",
            "character_id=1",
            &[(SESSION, &token)],
        )),
    )
    .await;
    assert!(foreign.body.contains("on your account"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_wizard_pages_from_token_to_complete(db: PgPool) {
    let h = harness(db, false).await;

    let page = send(&h.app, get("/setup", &[])).await;
    assert!(page.body.contains(r#"action="/setup/unlock""#));
    assert!(page.body.contains(r#"aria-current="step""#));

    let wrong = send(&h.app, form("/setup/unlock", "token=guess", &[])).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert!(wrong.body.contains("That setup token is not correct."));

    let ok = send(
        &h.app,
        form("/setup/unlock", &format!("token={SETUP_TOKEN}"), &[]),
    )
    .await;
    assert_eq!(ok.location(), "/setup");
    let setup = ok.cookie_value(SETUP);

    let page = send(&h.app, get("/setup", &[(SETUP, &setup)])).await;
    assert!(page.body.contains("https://tether.test/auth/callback"));
    assert!(page.body.contains(r#"action="/setup/sso""#));
    assert_only_allowed_external_urls(&page.body);

    let bad = send(
        &h.app,
        form("/setup/sso", "client_id=nope", &[(SETUP, &setup)]),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert!(bad.body.contains("client ID"));
    let saved = send(
        &h.app,
        form(
            "/setup/sso",
            "client_id=0123456789abcdef0123",
            &[(SETUP, &setup)],
        ),
    )
    .await;
    assert_eq!(saved.location(), "/setup");

    let page = send(&h.app, get("/setup", &[(SETUP, &setup)])).await;
    assert!(page.body.contains(r#"href="/auth/login?return_to=/setup""#));

    // Owner login from this browser.
    let (state, browser) = start_login(&h, "/setup").await;
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:196379789:Chribba&state={state}"),
            &[(LOGIN, &browser), (SETUP, &setup)],
        ),
    )
    .await;
    let owner = res.cookie_value(SESSION);

    let page = send(&h.app, get("/setup", &[(SESSION, &owner)])).await;
    assert!(
        page.body.contains("Otherworld Empire"),
        "owner's alliance is suggested"
    );

    let found = send(
        &h.app,
        htmx(form(
            "/setup/alliance/search",
            "name=Goonswarm+Federation",
            &[(SESSION, &owner)],
        )),
    )
    .await;
    assert!(
        found.body.contains("Goonswarm Federation"),
        "{}",
        found.body
    );

    let chosen = send(
        &h.app,
        form(
            "/setup/alliance",
            "entity_id=159826257",
            &[(SESSION, &owner)],
        ),
    )
    .await;
    assert_eq!(chosen.location(), "/setup");
    let page = send(&h.app, get("/setup", &[(SESSION, &owner)])).await;
    assert!(page.body.contains("Setup is complete"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn callback_check_fragment_explains_a_missing_setup_session(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, htmx(form("/setup/check", "", &[]))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(
        res.body.contains("Enter the setup token first."),
        "{}",
        res.body
    );
    assert!(!res.body.contains("<html"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn static_assets_are_embedded_and_cacheable(db: PgPool) {
    let h = harness(db, false).await;
    for (path, content_type) in [
        ("/static/app.css", "text/css; charset=utf-8"),
        ("/static/htmx.min.js", "text/javascript; charset=utf-8"),
        ("/static/fonts/Geist-Variable.woff2", "font/woff2"),
        ("/static/fonts/GeistMono-Variable.woff2", "font/woff2"),
    ] {
        let res = send(&h.app, get(path, &[])).await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        assert_eq!(res.headers[header::CONTENT_TYPE], content_type, "{path}");
        let etag = res.headers[header::ETAG].to_str().unwrap().to_owned();
        let again = send(
            &h.app,
            Request::get(path)
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{path}");
    }
    let css = send(&h.app, get("/static/app.css", &[])).await;
    assert!(
        css.body.contains("--accent-surface"),
        "DESIGN.md tokens are in the stylesheet"
    );
    assert_eq!(
        send(&h.app, get("/static/nope.js", &[])).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn security_headers_on_pages_errors_and_api(db: PgPool) {
    let h = harness(db, false).await;
    for uri in ["/login", "/no-such-page", "/api/setup"] {
        let res = send(&h.app, get(uri, &[])).await;
        let csp = res.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(csp.contains("script-src 'self'"), "{uri}");
        assert!(
            csp.contains("img-src 'self' https://images.evetech.net"),
            "{uri}"
        );
        assert_eq!(res.headers[header::X_FRAME_OPTIONS], "DENY", "{uri}");
        assert_eq!(
            res.headers[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff",
            "{uri}"
        );
        assert!(
            res.headers.contains_key(header::STRICT_TRANSPORT_SECURITY),
            "{uri}"
        );
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unknown_paths_get_the_404_page(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, get("/no-such-page", &[])).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("<html"));
    assert!(res.body.contains("nothing at this address"));
}
