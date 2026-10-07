//! Signed out, every form endpoint sends you to log in before it looks
//! at the form: an empty or missing body is never a 415 or a 422.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;

/// Every signed-in form route, with sample path ids.
const ROUTES: &[&str] = &[
    "/admin/autogroups/1/delete",
    "/tokens/1/delete",
    "/notifications/1/delete",
    "/notifications/read-all",
    "/notifications/delete-read",
    "/corpstats/1/update",
    "/groups/1/join",
    "/groups/1/leave",
    "/groups/1/retract",
    "/group-management/1/requests/1/accept",
    "/group-management/1/requests/1/reject",
    "/group-management/1/members/1/remove",
    "/admin/groups/1/leaders/1/remove",
    "/admin/groups/1/leader-groups/2/remove",
    "/pings/preview",
    "/admin/pings/settings",
    "/admin/pings/options",
    "/admin/pings/options/1/delete",
    "/admin/pings/restrictions",
    "/admin/pings/restrictions/1/remove",
    "/admin/settings",
    "/admin/system/site-name",
    "/admin/menu/sections",
    "/admin/menu/folders",
    "/admin/menu/links",
    "/admin/menu/change",
    "/admin/menu/move",
    "/admin/menu/delete",
    "/admin/menu/reset",
    "/blacklist",
    "/blacklist/1/remove",
    "/blacklist/notes",
    "/blacklist/notes/1/delete",
    "/blacklist/notes/1/edit",
    "/blacklist/notes/1/comments",
    "/admin/groups/1/smart",
    "/admin/groups/1/smart/filters",
    "/admin/groups/1/smart/filters/1/delete",
    "/admin/groups/1/smart/filters/1/grace",
    "/admin/groups/1/smart/filters/combine",
    "/securegroups/1/join",
    "/securegroups/1/leave",
    "/securegroups/audit/1/check",
    "/securegroups/audit/1/members/1/remove",
    "/admin/users/1/deactivate",
    "/admin/users/1/reactivate",
    "/admin/users/1/sign-out",
    "/sessions/sign-out-others",
    "/sessions/1/sign-out",
    "/admin/users/1/superuser",
    "/admin/users/1/superuser/revoke",
    "/admin/users/1/permissions",
    "/admin/users/1/permissions/1/revoke",
    "/admin/states/1/public",
    "/admin/discord/mappings/1/remove",
    "/services/discord/link",
    "/services/discord/unlink",
    "/admin/discord/channels/1/remove",
    "/admin/system/updates/check",
    "/admin/jobs/1/retry",
    "/admin/system/schedules/affiliation.sync/run",
    "/admin/plugins/example.hello/schedules/sync/run",
    "/admin/plugin-uploads/1/approve",
    "/admin/plugin-uploads/1/discard",
    "/admin/plugins/example.hello/enable",
    "/admin/plugins/example.hello/disable",
    "/admin/plugins/example.hello/update",
    "/admin/plugins/example.hello/source",
    "/admin/plugins/example.hello/rollback",
    "/admin/plugins/example.hello/uninstall",
    "/admin/plugin-github",
    "/register/start",
    "/apps/example.hello/owners/add",
    "/apps/example.hello/owners/1/withdraw",
    "/apps/example.hello/owners/1/remove",
    "/admin/plugins/example.hello/channels/1/remove",
    "/reauthenticate",
];

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn signed_out_posts_go_to_login_first(db: PgPool) {
    let h = harness(db, true).await;
    let mut wrong = Vec::new();
    for uri in ROUTES {
        // No body and no content type, as a bare form post or a probe.
        let bare = Request::post(*uri)
            .header(header::ORIGIN, SITE)
            .body(Body::empty())
            .unwrap();
        let res = send(&h.app, bare).await;
        if res.status != StatusCode::SEE_OTHER || res.location() != "/login" {
            wrong.push(format!("{uri}: {}", res.status));
        }
        // A post is never where a login lands: nothing is replayed.
        if res.set_cookie(NEXT).is_some() {
            wrong.push(format!("{uri}: remembered as a destination"));
        }
    }
    assert!(wrong.is_empty(), "not sent to log in: {wrong:#?}");
}

const NEXT: &str = "__Host-tether_next";

/// Logs in from a signed-out browser holding `next` (its destination
/// handle, if any) with the owner's character; returns where it lands.
async fn land(h: &Harness, login_uri: &str, next: Option<&str>) -> String {
    let cookies: Vec<(&str, &str)> = next.map(|n| (NEXT, n)).into_iter().collect();
    let res = send(&h.app, get(login_uri, &cookies)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    if next.is_some() {
        // The handle is spent either way.
        assert!(res.set_cookie(NEXT).unwrap().contains("Max-Age=0"));
    }
    let state = query_param(res.location(), "state").to_owned();
    let browser = res.cookie_value(LOGIN);
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:196379789:Chribba&state={state}"),
            &[(LOGIN, &browser)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.location().to_owned()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_deep_link_signed_out_lands_there_after_login(db: PgPool) {
    let h = harness(db, true).await;
    log_in_owner(&h, "196379789:Chribba").await;

    // Still sent to /login, with a handle on the page kept server-side.
    let res = send(&h.app, get("/admin/users?page=2", &[])).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/login");
    let cookie = res.set_cookie(NEXT).unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("Secure"));
    assert!(!cookie.contains("admin"), "only a handle: {cookie}");
    let next = res.cookie_value(NEXT);
    let stored: String = sqlx::query_scalar("SELECT path FROM core.login_destinations")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(stored, "/admin/users?page=2");

    // Another page from the same browser replaces it, under the same handle.
    let res = send(&h.app, get("/admin/audit", &[(NEXT, &next)])).await;
    assert_eq!(res.location(), "/login");
    assert_eq!(res.cookie_value(NEXT), next);

    assert_eq!(land(&h, "/auth/login", Some(&next)).await, "/admin/audit");
    // Used once.
    assert_eq!(land(&h, "/auth/login", Some(&next)).await, "/");
    // A link that names its destination (setup's) keeps it.
    let res = send(&h.app, get("/admin/users", &[])).await;
    let next = res.cookie_value(NEXT);
    assert_eq!(
        land(&h, "/auth/login?return_to=/setup", Some(&next)).await,
        "/setup"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_page_visits_are_remembered(db: PgPool) {
    let h = harness(db, true).await;
    log_in_owner(&h, "196379789:Chribba").await;
    let visit = |uri: &str, headers: &[(&str, &str)]| {
        let mut req = Request::get(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.body(Body::empty()).unwrap()
    };
    for (uri, headers) in [
        // htmx fragments, event streams and scripts' fetches.
        ("/admin/users", &[("hx-request", "true")][..]),
        (
            "/notifications/stream",
            &[("accept", "text/event-stream")][..],
        ),
        ("/admin/users", &[("sec-fetch-dest", "empty")][..]),
        ("/admin/users", &[("accept", "application/json")][..]),
        // Pages that aren't a way into logging in, or the home page.
        ("/", &[][..]),
        ("/login", &[][..]),
        ("/admin/users?next=https://evil.example", &[][..]),
    ] {
        let res = send(&h.app, visit(uri, headers)).await;
        assert!(res.set_cookie(NEXT).is_none(), "{uri} {headers:?}");
    }
    // A browser's navigation is.
    let res = send(
        &h.app,
        visit(
            "/admin/users",
            &[
                ("sec-fetch-dest", "document"),
                ("accept", "text/html,application/xhtml+xml,*/*;q=0.8"),
            ],
        ),
    )
    .await;
    assert!(res.set_cookie(NEXT).is_some());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_login_never_lands_off_site(db: PgPool) {
    let h = harness(db, true).await;
    log_in_owner(&h, "196379789:Chribba").await;
    for hostile in [
        "https://evil.example",
        "//evil.example",
        "///evil.example",
        "/%5Cevil.example",
        "%2F%2Fevil.example",
        "/\\evil.example",
        "javascript:alert(1)",
        "/login",
        "/auth/logout",
    ] {
        let uri = format!("/auth/login?return_to={}", hostile.replace('\\', "%5C"));
        assert_eq!(land(&h, &uri, None).await, "/", "{hostile}");
    }
    // Even a stored destination is checked again before it's used.
    let handle = "ab".repeat(32);
    for hostile in ["//evil.example", "https://evil.example", "/a\\b"] {
        sqlx::query(
            "INSERT INTO core.login_destinations (browser_hash, path, expires_at) \
             VALUES (sha256($1::bytea), $2, now() + interval '1 minute')",
        )
        .bind(handle.as_bytes())
        .bind(hostile)
        .execute(&h.db)
        .await
        .unwrap();
        assert_eq!(
            land(&h, "/auth/login", Some(&handle)).await,
            "/",
            "{hostile}"
        );
    }
    // And an expired one is ignored.
    sqlx::query(
        "INSERT INTO core.login_destinations (browser_hash, path, expires_at) \
         VALUES (sha256($1::bytea), '/admin/users', now() - interval '1 minute')",
    )
    .bind(handle.as_bytes())
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(land(&h, "/auth/login", Some(&handle)).await, "/");
}

/// Every path the router serves, read from its source so a new route is
/// covered the day it's added: each `.route(` and the path after it, its
/// parameters filled with samples.
fn every_route() -> Vec<String> {
    let source = include_str!("../../src/lib.rs");
    let mut paths = Vec::new();
    // One router, its routes named by literal paths: anything else would
    // slip past this list.
    assert!(
        !source.contains(".nest("),
        "nested routers aren't read here"
    );
    assert_eq!(source.matches("Router::new()").count(), 1);
    for (at, _) in source.match_indices(".route(") {
        let rest = source[at + ".route(".len()..].trim_start();
        assert!(
            rest.starts_with('"'),
            "a route not named by a literal path: {}",
            &rest[..rest.len().min(80)]
        );
        let rest = &rest[1..];
        let Some(close) = rest.find('"') else {
            continue;
        };
        let path = &rest[..close];
        if !path.starts_with('/') {
            continue;
        }
        let mut filled = String::new();
        let mut inside = false;
        for c in path.chars() {
            match c {
                '{' => inside = true,
                '}' => {
                    inside = false;
                    filled.push('1');
                }
                _ if !inside => filled.push(c),
                _ => {}
            }
        }
        paths.push(filled);
    }
    paths.sort();
    paths.dedup();
    paths
}

/// What a signed-out browser may reach once Tether is set up: the way in
/// (logging in and EVE's and Discord's callbacks, which check their own
/// state), logging out, what the login page draws with, the container's
/// health checks, and the metrics endpoint (its own bearer token). Each
/// with the answer it gives without a session; anything else must send to
/// log in (or 401 for the API, or 405 for a method the route doesn't
/// take).
const THE_WAY_IN: &[(&str, &str, u16)] = &[
    ("GET", "/login", 200),
    ("GET", "/auth/login", 303),
    ("GET", "/auth/callback", 400),
    ("GET", "/discord/callback", 400),
    ("POST", "/auth/logout", 303),
    ("GET", "/health", 200),
    ("GET", "/ready", 200),
    // Off unless METRICS_ENABLED (then only with METRICS_TOKEN).
    ("GET", "/metrics", 404),
    ("GET", "/theme.css", 200),
    // An asset that doesn't exist.
    ("GET", "/static/1", 404),
    // The old name of the Dashboard, which then asks to log in.
    ("GET", "/profile", 308),
    // Echoes a nonce, so `tether doctor` can check the public URL (this
    // one sends none).
    ("GET", "/api/setup/probe", 400),
];

/// Routes only development builds have: not there at all in this one.
const DEV_ONLY: &[&str] = &["/dev/login", "/dev/login/1", "/docs"];

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_route_needs_a_login_but_the_way_in(db: PgPool) {
    let h = harness(db, true).await;
    // Set up: an owner exists. Its session isn't sent below.
    log_in_owner(&h, "196379789:Chribba").await;
    let routes = every_route();
    assert!(routes.len() > 100, "{routes:?}");
    let mut wrong = Vec::new();
    for path in routes {
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .header(header::ORIGIN, SITE)
                .body(Body::empty())
                .unwrap();
            let res = send(&h.app, req).await;
            let status = res.status.as_u16();
            let to = res
                .headers
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let fine = if DEV_ONLY.contains(&path.as_str()) {
                // Not found, or a post sent to log in first.
                status == 404 || (status == 303 && to == "/login")
            } else {
                match THE_WAY_IN
                    .iter()
                    .find(|(m, p, _)| *m == method && *p == path)
                {
                    Some((_, _, expected)) => status == *expected,
                    None => (status == 303 && to == "/login") || status == 401 || status == 405,
                }
            };
            if !fine {
                wrong.push(format!("{method} {path}: {status} {to}"));
            }
        }
    }
    assert!(wrong.is_empty(), "open signed out: {wrong:#?}");

    // The probe answers anyone: `tether doctor` checks the public URL so.
    let res = send(&h.app, get("/api/setup/probe?nonce=abc123", &[])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}
