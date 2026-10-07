//! The Sessions page: where an account is signed in, Sign out per session
//! and everywhere else, and an admin signing someone out everywhere (sudo
//! mode). Each audited; access tokens never manage sessions.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";
const FIREFOX: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0";

/// A plain sign-in from a browser saying `user_agent`.
async fn log_in_from(h: &Harness, character: &str, user_agent: &str) -> String {
    let (state, browser) = start_login(h, "/").await;
    let res = send(
        &h.app,
        Request::get(format!("/auth/callback?code=ok:{character}&state={state}"))
            .header(header::COOKIE, format!("{LOGIN}={browser}"))
            .header(header::USER_AGENT, user_agent)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

/// The id of the session behind a cookie.
async fn id_of(h: &Harness, session: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM core.sessions WHERE token_hash = sha256($1::bytea)")
        .bind(session.as_bytes())
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn alive(h: &Harness, session: &str) -> bool {
    send(&h.app, get("/api/me", &[(SESSION, session)]))
        .await
        .status
        == StatusCode::OK
}

async fn audited(h: &Harness, action: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('actor', actor_account_id, 'target', target, 'details', details) \
         FROM core.audit_log WHERE action = $1 ORDER BY id",
    )
    .bind(action)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_page_lists_this_accounts_sessions_and_marks_this_one(db: PgPool) {
    let h = harness(db, true).await;
    log_in_owner(&h, CHRIBBA).await;
    let laptop = log_in_from(&h, CHRIBBA, FIREFOX).await;
    let phone = log_in_as(&h, CHRIBBA, None).await;
    let other = log_in_as(&h, GIGX, None).await;

    let res = page(&h, "/sessions", &laptop).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // Three of Chribba's (the owner's first login too), none of gigX's.
    let rows = res.body.matches("<tr>").count() - 1;
    assert_eq!(rows, 3, "{}", res.body);
    // A coarse label only, never the header's text.
    assert!(res.body.contains("Firefox on Windows"));
    assert!(!res.body.contains("rv:131.0"));
    let stored: Option<String> =
        sqlx::query_scalar("SELECT device FROM core.sessions WHERE token_hash = sha256($1::bytea)")
            .bind(laptop.as_bytes())
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(stored.as_deref(), Some("Firefox on Windows"));
    assert!(res.body.contains("Unknown browser"));
    assert_eq!(res.body.matches("This browser").count(), 1);
    // Every other session has its button; this one doesn't.
    let laptop_id = id_of(&h, &laptop).await;
    let phone_id = id_of(&h, &phone).await;
    let other_id = id_of(&h, &other).await;
    assert!(
        !res.body
            .contains(&format!(r#"action="/sessions/{laptop_id}/sign-out""#))
    );
    assert!(
        res.body
            .contains(&format!(r#"action="/sessions/{phone_id}/sign-out""#))
    );
    assert!(
        !res.body
            .contains(&format!(r#"action="/sessions/{other_id}/sign-out""#))
    );
    assert!(res.body.contains("Sign out everywhere else"));
    // Nothing of the session's key reaches the page.
    assert!(!res.body.contains(&laptop));

    // In the account menu, marked while open.
    assert!(
        res.body
            .contains(r#"href="/sessions" class="user-menu-item" aria-current="page""#)
    );

    // Alone, there's nobody else to sign out.
    let res = page(&h, "/sessions", &other).await;
    assert_eq!(res.body.matches("<tr>").count() - 1, 1);
    assert!(!res.body.contains("Sign out everywhere else"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sign_out_ends_one_session_of_ones_own(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let laptop = log_in_as(&h, CHRIBBA, None).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let laptop_id = id_of(&h, &laptop).await;
    let pilot_id = id_of(&h, &pilot).await;
    let owner_id = id_of(&h, &owner).await;

    // Someone else's: as if there were none, and it stays.
    let res = send(
        &h.app,
        form(&format!("/sessions/{pilot_id}/sign-out"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert!(alive(&h, &pilot).await);

    // This browser's own: that's Log out.
    let res = send(
        &h.app,
        form(&format!("/sessions/{owner_id}/sign-out"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("Log out"));
    assert!(alive(&h, &owner).await);

    let res = send(
        &h.app,
        form(&format!("/sessions/{laptop_id}/sign-out"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/sessions");
    assert!(!alive(&h, &laptop).await);
    assert!(alive(&h, &owner).await);
    let entries = audited(&h, "session.sign_out").await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["details"]["session_id"], laptop_id);

    // Gone already.
    let res = send(
        &h.app,
        form(&format!("/sessions/{laptop_id}/sign-out"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sign_out_everywhere_else_keeps_only_this_browser(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let laptop = log_in_as(&h, CHRIBBA, None).await;
    let phone = log_in_as(&h, CHRIBBA, None).await;
    let pilot = log_in_as(&h, GIGX, None).await;

    let res = send(&h.app, form("/sessions/sign-out-others", "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/sessions");
    assert!(alive(&h, &owner).await);
    assert!(!alive(&h, &laptop).await);
    assert!(!alive(&h, &phone).await);
    // Another account's are untouched.
    assert!(alive(&h, &pilot).await);
    let entries = audited(&h, "session.sign_out_others").await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["details"]["sessions"], 2);

    // A boosted post stays on the page with a toast.
    let res = send(
        &h.app,
        boosted(form("/sessions/sign-out-others", "", &owner), "/sessions"),
    )
    .await;
    let (message, _) = toast(&res).expect("a toast");
    assert_eq!(message, "Signed out of 0 sessions.");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn cross_site_and_signed_out_posts_change_nothing(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let laptop = log_in_as(&h, CHRIBBA, None).await;

    // Another site's form, with the cookie riding along.
    let res = send(
        &h.app,
        Request::post("/sessions/sign-out-others")
            .header(header::ORIGIN, "https://evil.example")
            .header(header::COOKIE, format!("{SESSION}={owner}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(alive(&h, &laptop).await);

    // Signed out: to log in, nothing read.
    let res = send(&h.app, get("/sessions", &[])).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/login");
    let res = send(
        &h.app,
        Request::post("/sessions/sign-out-others")
            .header(header::ORIGIN, SITE)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.location(), "/login");
    assert!(alive(&h, &laptop).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn access_tokens_never_manage_sessions(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let laptop = log_in_as(&h, CHRIBBA, None).await;
    let made = send(
        &h.app,
        form(
            "/dashboard/access-tokens",
            "name=Bot&days=30&scopes=account%3Aread&scopes=admin.users",
            &owner,
        ),
    )
    .await;
    let start = made.body.find("tether_pat_").unwrap();
    let token: String = made.body[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    for (method, uri) in [
        ("GET", "/sessions".to_owned()),
        ("POST", "/sessions/sign-out-others".to_owned()),
        (
            "POST",
            format!("/sessions/{}/sign-out", id_of(&h, &laptop).await),
        ),
    ] {
        let res = send(
            &h.app,
            Request::builder()
                .method(method)
                .uri(&uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    assert!(alive(&h, &laptop).await);
}

/// The session last logged in with EVE `minutes` ago (sudo mode).
async fn age(h: &Harness, session: &str, minutes: i32) {
    sqlx::query(
        "UPDATE core.sessions SET reauthenticated_at = now() - make_interval(mins => $2) \
         WHERE token_hash = sha256($1::bytea)",
    )
    .bind(session.as_bytes())
    .bind(minutes)
    .execute(&h.db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_sign_a_user_out_everywhere_in_sudo_mode(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_too = log_in_as(&h, GIGX, None).await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let user_page = format!("/admin/users/{pilot_account}");
    let sign_out = format!("{user_page}/sign-out");

    let res = page(&h, &user_page, &owner).await;
    assert!(
        res.body
            .contains("signed in on <span class=\"num\">2</span> browsers")
    );
    assert!(res.body.contains(&format!(r#"action="{sign_out}""#)));

    // Not for those without admin.users.
    let res = send(&h.app, form(&sign_out, "", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(alive(&h, &pilot).await);

    // A stale session confirms it's them first: nothing changes.
    age(&h, &owner, 16).await;
    let mut req = form(&sign_out, "", &owner);
    req.headers_mut().insert(
        header::REFERER,
        format!("{SITE}{user_page}").parse().unwrap(),
    );
    let res = send(&h.app, req).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        res.location()
            .starts_with("/reauthenticate?action=sign_out_user"),
        "{}",
        res.location()
    );
    assert!(alive(&h, &pilot).await);

    age(&h, &owner, 1).await;
    let res = send(&h.app, form(&sign_out, "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), user_page);
    assert!(!alive(&h, &pilot).await);
    assert!(!alive(&h, &pilot_too).await);
    assert!(alive(&h, &owner).await);
    let entries = audited(&h, "session.sign_out_all").await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["target"], format!("account:{pilot_account}"));
    assert_eq!(entries[0]["details"]["sessions"], 2);
    // Nobody signed in: no button.
    let res = page(&h, &user_page, &owner).await;
    assert!(!res.body.contains(&format!(r#"action="{sign_out}""#)));

    // Not one's own account (that's the Sessions page).
    let owner_account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{owner_account}/sign-out"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(alive(&h, &owner).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_user_admin_cant_sign_out_someone_holding_more(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let owner_account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    // gigX may administer users, and nothing more.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/permissions"),
            "permission=admin.users",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{owner_account}/sign-out"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("which you don"), "{}", res.body);
    assert!(alive(&h, &owner).await);
}
