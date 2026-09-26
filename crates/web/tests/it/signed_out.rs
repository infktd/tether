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
    "/admin/system/theme",
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
    "/admin/groups/1/smart",
    "/admin/groups/1/smart/filters",
    "/admin/groups/1/smart/filters/1/delete",
    "/admin/users/1/deactivate",
    "/admin/users/1/reactivate",
    "/admin/discord/mappings/1/remove",
    "/services/discord/link",
    "/services/discord/unlink",
    "/admin/discord/channels/1/remove",
    "/admin/system/updates/check",
    "/admin/jobs/1/retry",
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
    "/profile/corp-stats/offer",
    "/profile/corp-stats/1/withdraw",
    "/compliance/sources/1/approve",
    "/compliance/sources/1/remove",
    "/profile/plugins/example.hello/offer",
    "/profile/plugins/example.hello/offer/1/withdraw",
    "/admin/plugins/example.hello/sources/1/approve",
    "/admin/plugins/example.hello/sources/1/remove",
    "/admin/plugins/example.hello/channels/1/remove",
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
