use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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
        "/dashboard"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn login_page(db: PgPool) {
    let h = harness(db, true).await;
    let res = send(&h.app, get("/login", &[])).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains(r#"href="/auth/login""#));
    assert!(res.body.contains("Log in with EVE Online"));
    assert!(res.body.contains("Admins log in here too"));
    assert_only_allowed_external_urls(&res.body);
    // CCP's proprietary notice (its Developer License Agreement, 7.1).
    const NOTICE: &str = "© 2014 CCP hf. All rights reserved. &quot;EVE&quot;, &quot;EVE Online&quot;, \
                          &quot;CCP&quot;, and all related logos and images are trademarks or \
                          registered trademarks of CCP hf.";
    assert!(res.body.contains(NOTICE), "{}", res.body);

    let token = log_in_owner(&h, "196379789:Chribba").await;
    assert_eq!(
        send(&h.app, get("/login", &[(SESSION, &token)]))
            .await
            .location(),
        "/dashboard"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_dashboard_shows_characters_and_state(db: PgPool) {
    let h = harness(db, true).await;
    assert_eq!(
        send(&h.app, get("/dashboard", &[])).await.location(),
        "/login"
    );

    let token = log_in_owner(&h, "196379789:Chribba").await;
    let token = log_in_as(&h, "443630591:The Mittani", Some(&token)).await;
    let res = send(&h.app, get("/dashboard", &[(SESSION, &token)])).await;

    assert_eq!(res.status, StatusCode::OK);
    let html = &res.body;
    assert!(html.contains("<h1 class=\"page-title\">Dashboard</h1>"));
    assert!(html.contains("https://images.evetech.net/characters/196379789/portrait?size=64"));
    assert!(html.contains("The Mittani"));
    // As AA: the main is marked once, other characters carry no "Alt"
    // label, and can be made the main from their row.
    assert_eq!(html.matches("</svg> Main</span>").count(), 1, "{html}");
    assert!(!html.contains(">Alt<"));
    // The state and groups under the title; the permissions are on
    // Groups, behind a disclosure, not a wall.
    assert!(html.contains(r#"class="page-membership""#), "{html}");
    assert!(html.contains(r#"data-state="guest""#));
    assert!(!html.contains("admin.states"), "{html}");
    let groups = send(&h.app, get("/groups", &[(SESSION, &token)])).await;
    assert!(
        groups
            .body
            .contains(r#"<details class="card card-disclosure">"#),
        "{}",
        groups.body
    );
    assert!(
        groups.body.contains("admin.states"),
        "owner sees their permissions"
    );
    assert!(html.contains(r#"action="/profile/main""#));
    assert!(html.contains(">Make main<"), "{html}");
    assert!(html.contains("Change Main with EVE login"), "{html}");
    // One way to add a character, and no scope wall: scopes are on Token
    // Management.
    assert_eq!(html.matches("Add character").count(), 1, "{html}");
    assert!(!html.contains("scopes granted"), "{html}");
    assert!(
        !html.contains("Viewing as"),
        "no watermark on the Dashboard"
    );
    assert!(html.contains(r#"aria-current="page""#));
    assert_only_allowed_external_urls(html);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn make_main_stays_on_the_dashboard(db: PgPool) {
    let h = harness(db, true).await;
    let token = log_in_owner(&h, "196379789:Chribba").await;
    let token = log_in_as(&h, "443630591:The Mittani", Some(&token)).await;
    let affiliation_reads = || async {
        h.esi_server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/characters/affiliation")
            .count()
    };
    let before = affiliation_reads().await;

    // From the Dashboard (a boosted form): the Dashboard again, in place,
    // with a toast.
    let res = send(
        &h.app,
        boosted(
            form(
                "/profile/main",
                "character_id=443630591",
                &[(SESSION, &token)],
            ),
            "/dashboard",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = hx_location(&res).expect("HX-Location");
    assert_eq!(to["path"], "/dashboard");
    assert_eq!(to["swap"], "innerHTML show:none");
    assert_eq!(to["push"], "false");
    assert_eq!(
        toast(&res),
        Some((
            "The Mittani is your main now.".to_owned(),
            "done".to_owned()
        ))
    );
    assert_eq!(me(&h, &token).await["main"]["id"], 443630591);
    // The new main's corporation was read from ESI at once (there's room).
    assert_eq!(affiliation_reads().await, before + 1);

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
    assert_eq!(plain.location(), "/dashboard");

    // A foreign character is refused in a toast; nothing moves.
    let foreign = send(
        &h.app,
        boosted(
            form("/profile/main", "character_id=1", &[(SESSION, &token)]),
            "/dashboard",
        ),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NO_CONTENT);
    assert!(foreign.headers.get("hx-location").is_none());
    let (message, tone) = toast(&foreign).expect("a toast");
    assert!(message.contains("on your account"), "{message}");
    assert_eq!(tone, "problem");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_boosted_login_start_keeps_its_cookie(db: PgPool) {
    let h = harness(db, true).await;
    let token = log_in_owner(&h, "196379789:Chribba").await;

    // Register (a boosted form): off to EVE's login in full, with the
    // cookie its callback checks.
    let res = send(
        &h.app,
        boosted(
            form("/register/start", "", &[(SESSION, &token)]),
            "/register",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = res.headers["hx-redirect"].to_str().unwrap();
    assert!(to.starts_with("https://login.test/authorize"), "{to}");
    assert!(!res.cookie_value(LOGIN).is_empty());
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
    // The scopes to enable on the EVE application, Member's own among
    // them: an application without them can't register anyone.
    assert!(
        page.body.contains("Authentication &amp; API Access"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("esi-corporations.read_corporation_membership.v1"),
        "{}",
        page.body
    );
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
    // Onwards: the owner to Administration.
    assert!(page.body.contains(r#"href="/admin""#), "{}", page.body);
    // And names the site, prefilled with the main's alliance.
    assert!(page.body.contains("Name this site"), "{}", page.body);
    assert!(
        page.body.contains(r#"value="Otherworld Empire""#),
        "{}",
        page.body
    );
    // Signed out, set up: to the same EVE login everyone uses.
    let page = send(&h.app, get("/setup", &[])).await;
    assert_eq!(page.location(), "/login");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_login_without_the_setup_token_is_told_how_to_become_owner(db: PgPool) {
    let h = harness(db, true).await;
    // Logged in from a browser that never entered the setup token (or
    // whose setup session ended): a plain account, not the owner.
    let token = log_in_as(&h, "196379789:Chribba", None).await;
    assert_eq!(me(&h, &token).await["is_owner"], false);
    let page = send(&h.app, get("/setup", &[(SESSION, &token)])).await;
    assert!(page.body.contains("Enter the setup token"));
    assert!(
        page.body.contains("this instance has no superuser yet"),
        "{}",
        page.body
    );

    // With the token entered, logging in again claims it.
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    assert_eq!(me(&h, &owner).await["is_owner"], true);
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
        ("/static/fonts/Archivo-Variable.woff2", "font/woff2"),
        ("/static/fonts/IBMPlexMono-Variable.woff2", "font/woff2"),
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
        // Forms that post and then 303 to EVE SSO (Add Character, Change
        // Main with EVE login): browsers check the redirect too.
        assert!(
            csp.contains("form-action 'self' https://login.eveonline.com"),
            "{uri}"
        );
        assert!(
            csp.contains("img-src 'self' data: https://images.evetech.net"),
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
        // Never in search results (an instance may carry its alliance's
        // name), on pages, errors and the API alike.
        assert_eq!(
            res.headers["x-robots-tag"], "noindex, nofollow, noarchive",
            "{uri}"
        );
    }
    let login = send(&h.app, get("/login", &[])).await.body;
    assert!(login.contains(r#"<meta name="robots" content="noindex, nofollow, noarchive">"#));
    // Pages never go into the browser's storage (htmx's history cache is
    // off; Back and Forward use the tab's memory, assets/live.js), and
    // Back never reloads the whole page.
    assert!(
        login.contains(r#""historyCacheSize": 0, "refreshOnHistoryMiss": false"#),
        "{login}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unknown_paths_get_the_404_page(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, get("/no-such-page", &[])).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("<html"));
    assert!(res.body.contains("nothing at this address"));
}
