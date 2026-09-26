//! Sudo mode: owner-only and sensitive admin actions need an EVE login
//! with the account's main in the last 15 minutes. Otherwise the browser
//! is sent to confirm it's them, and comes back to submit again.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";
const ALT: &str = "90000002:Chribba Alt";

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

/// The session last logged in with EVE `minutes` ago.
async fn age(h: &Harness, session: &str, minutes: i64) {
    let changed = sqlx::query(
        "UPDATE core.sessions SET reauthenticated_at = now() - make_interval(mins => $2) \
         WHERE token_hash = sha256($1::bytea)",
    )
    .bind(session.as_bytes())
    .bind(minutes as i32)
    .execute(&h.db)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(changed, 1);
}

async fn active(h: &Harness, account: i64) -> bool {
    sqlx::query_scalar("SELECT active FROM core.accounts WHERE id = $1")
        .bind(account)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// A form post from `from` (a page on this site).
fn post_from(uri: &str, body: &str, session: &str, from: &str) -> Request<Body> {
    let mut req = form(uri, body, session);
    req.headers_mut()
        .insert(header::REFERER, format!("{SITE}{from}").parse().unwrap());
    req
}

/// Starts a re-authentication from the confirmation page's form: (oauth
/// state, login cookie).
async fn start_reauth(
    h: &Harness,
    session: &str,
    action: &str,
    return_to: &str,
) -> (String, String) {
    let res = send(
        &h.app,
        form(
            "/reauthenticate",
            &format!(
                "action={action}&return_to={}",
                return_to.replace('/', "%2F")
            ),
            session,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    (
        query_param(res.location(), "state").to_owned(),
        res.cookie_value(LOGIN),
    )
}

async fn finish(h: &Harness, session: &str, state: &str, browser: &str, character: &str) -> Res {
    send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, browser), (SESSION, session)],
        ),
    )
    .await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_stale_session_confirms_its_the_main_then_submits_again(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let user_page = format!("/admin/users/{pilot_account}");
    let deactivate = format!("{user_page}/deactivate");

    // Fresh from logging in: straight through.
    age(&h, &owner, 14).await;
    let res = send(&h.app, post_from(&deactivate, "", &owner, &user_page)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(!active(&h, pilot_account).await);
    let reactivate = format!("{user_page}/reactivate");

    // 16 minutes on: sent to confirm, naming the action and the page, and
    // nothing changed.
    age(&h, &owner, 16).await;
    let res = send(&h.app, post_from(&reactivate, "", &owner, &user_page)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let confirm = res.location().to_owned();
    assert_eq!(
        confirm,
        format!(
            "/reauthenticate?action=account_reactivate&return_to=%2Fadmin%2Fusers%2F{pilot_account}"
        )
    );
    assert!(!active(&h, pilot_account).await);

    let res = page(&h, &confirm, &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Confirm it&#x27;s you") || res.body.contains("Confirm it's you"));
    assert!(res.body.contains("Log in with EVE again to continue"));
    assert!(res.body.contains("Reactivate an account"));
    assert!(res.body.contains("Chribba"), "names the main");
    assert!(res.body.contains(r#"action="/reauthenticate""#));
    assert!(
        res.body
            .contains(&format!(r#"name="return_to" value="{user_page}""#))
    );
    assert!(
        res.body
            .contains(r#"name="action" value="account_reactivate""#)
    );

    // Logging in with the main brings it back to the page, freshly.
    let (state, browser) = start_reauth(&h, &owner, "account_reactivate", &user_page).await;
    let res = finish(&h, &owner, &state, &browser, CHRIBBA).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), user_page);
    let fresh = res.cookie_value(SESSION);
    assert_ne!(fresh, owner, "the session rotates");
    let audited: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'session.reauth'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited["action"], "account_reactivate");
    assert_eq!(audited["character_id"], 196379789);
    // Nothing was replayed: the admin submits again.
    assert!(!active(&h, pilot_account).await);
    let res = send(&h.app, post_from(&reactivate, "", &fresh, &user_page)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(active(&h, pilot_account).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_the_accounts_own_main_confirms(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // Adding a character proves only that character: the time carries
    // over, it isn't renewed.
    age(&h, &owner, 10).await;
    let owner = log_in_as(&h, ALT, Some(&owner)).await;
    let carried: bool = sqlx::query_scalar(
        "SELECT reauthenticated_at < now() - interval '9 minutes' FROM core.sessions \
         WHERE token_hash = sha256($1::bytea)",
    )
    .bind(owner.as_bytes())
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(carried);
    age(&h, &owner, 30).await;
    log_in_as(&h, GIGX, None).await;

    for character in [ALT, GIGX, "90000009:Stranger"] {
        let (state, browser) = start_reauth(&h, &owner, "app_install", "/admin/plugins").await;
        let res = finish(&h, &owner, &state, &browser, character).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{character}");
        assert!(res.body.contains("main"), "{}", res.body);
    }
    // Each refusal is on the record.
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'session.reauth_refused'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(refused, 3);
    // Nothing moved or was created, and the session is as stale as before.
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM core.accounts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(accounts, 2);
    let res = send(
        &h.app,
        form(
            "/dashboard/access-tokens",
            "name=x&days=1&scopes=account%3Aread",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert!(
        res.location()
            .starts_with("/reauthenticate?action=access_token&")
    );

    // Started from another browser's session: refused.
    let other = log_in_as(&h, GIGX, None).await;
    let (state, browser) = start_reauth(&h, &owner, "app_install", "/admin/plugins").await;
    let res = finish(&h, &other, &state, &browser, CHRIBBA).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn htmx_navigates_and_the_api_answers_403(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    age(&h, &owner, 60).await;

    let mut req = post_from(
        &format!("/admin/users/{pilot_account}/deactivate"),
        "",
        &owner,
        "/admin/users",
    );
    req.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let res = send(&h.app, req).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.headers["hx-redirect"],
        "/reauthenticate?action=account_deactivate&return_to=%2Fadmin%2Fusers"
    );

    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{pilot_account}/deactivate"),
            &owner,
            "",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("Confirm it's you"), "{}", res.body);
    assert!(active(&h, pilot_account).await);

    // A Referer from another site, or none, comes back to the Dashboard.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/deactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert!(res.location().ends_with("&return_to=%2Fdashboard"));
    let mut req = form(
        &format!("/admin/users/{pilot_account}/deactivate"),
        "",
        &owner,
    );
    req.headers_mut().insert(
        header::REFERER,
        "https://tether.test.evil.example/admin".parse().unwrap(),
    );
    assert!(
        send(&h.app, req)
            .await
            .location()
            .ends_with("&return_to=%2Fdashboard")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_sensitive_grants_are_gated(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let state_id = async |name: &str| -> i64 {
        sqlx::query_scalar("SELECT id FROM core.states WHERE name = $1")
            .bind(name)
            .fetch_one(&h.db)
            .await
            .unwrap()
    };
    let (member, guest) = (state_id("Member").await, state_id("Guest").await);
    age(&h, &owner, 60).await;
    let grant = |permission: &str, state: i64| {
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"{permission}","state_id":{state}}}"#),
        )
    };
    // Everyday: straight through.
    let res = send(&h.app, grant("discord.access_discord", guest)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    // Sensitive: refused until the owner logs in again.
    let res = send(&h.app, grant("admin.users", member)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let grants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.permission_grants WHERE permission = 'admin.users'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(grants, 0);

    // Revoking one is gated too, and keeps the grant.
    age(&h, &owner, 0).await;
    let res = send(&h.app, grant("admin.users", member)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let id = serde_json::from_str::<serde_json::Value>(&res.body).unwrap()["id"]
        .as_i64()
        .unwrap();
    age(&h, &owner, 60).await;
    let revoke = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/admin/permissions/grants/{id}"))
        .header(header::COOKIE, format!("{SESSION}={owner}"))
        .header(header::ORIGIN, SITE)
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&h.app, revoke).await.status, StatusCode::FORBIDDEN);
    let grants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.permission_grants WHERE permission = 'admin.users'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(grants, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn owner_only_actions_are_gated(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    age(&h, &owner, 60).await;

    // A Restricted group.
    let res = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Council","restricted":true}"#,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    // An ordinary one isn't gated.
    let res = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Miners"}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);

    // Setup, once there's an owner.
    let res = send(
        &h.app,
        post_from(
            "/setup/sso",
            "client_id=0123456789abcdef0123",
            &owner,
            "/setup",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        res.location(),
        "/reauthenticate?action=setup&return_to=%2Fsetup"
    );

    // Apps: uninstalling (checked before anything else about the app).
    let res = send(
        &h.app,
        post_from(
            "/admin/plugins/example.hello/uninstall",
            "confirmation=example.hello",
            &owner,
            "/admin/plugins/example.hello",
        ),
    )
    .await;
    assert_eq!(
        res.location(),
        "/reauthenticate?action=app_uninstall&return_to=%2Fadmin%2Fplugins%2Fexample.hello"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn access_tokens_arent_browsers(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    // Making the token is gated (the owner just logged in).
    let made = send(
        &h.app,
        form(
            "/dashboard/access-tokens",
            "name=Bot&days=1&scopes=admin.users",
            &owner,
        ),
    )
    .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let start = made.body.find("tether_pat_").unwrap();
    let token: String = made.body[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    age(&h, &owner, 60).await;
    // The token carries admin.users explicitly: no login to ask for.
    let res = send(
        &h.app,
        Request::post(format!("/api/admin/accounts/{pilot_account}/deactivate"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
    assert!(!active(&h, pilot_account).await);
    // It can't confirm anything either.
    let res = send(
        &h.app,
        Request::get("/reauthenticate")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_confirmation_page_only_returns_on_site(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // Signed out: to log in first (and back here afterwards).
    let res = send(&h.app, get("/reauthenticate?action=setup", &[])).await;
    assert_eq!(res.location(), "/login");

    for hostile in [
        "//evil.example",
        "https://evil.example",
        "%2F%2Fevil.example",
        "/%5Cevil",
    ] {
        let res = page(
            &h,
            &format!("/reauthenticate?action=nope&return_to={hostile}"),
            &owner,
        )
        .await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(
            res.body.contains(r#"name="return_to" value="/dashboard""#),
            "{hostile}"
        );
        assert!(res.body.contains(r#"name="action" value="""#));
    }
    let (state, browser) = start_reauth(&h, &owner, "nope", "//evil.example").await;
    let res = finish(&h, &owner, &state, &browser, CHRIBBA).await;
    assert_eq!(res.location(), "/dashboard");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_is_gated_for_those_with_powers_only(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, ALT, Some(&owner)).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot = log_in_as(&h, "90000003:gigX Alt", Some(&pilot)).await;
    age(&h, &owner, 60).await;
    age(&h, &pilot, 60).await;

    // The owner: a stolen session mustn't swap in a main of its own, which
    // could then confirm it's them.
    let res = send(
        &h.app,
        post_from(
            "/profile/main",
            "character_id=90000002",
            &owner,
            "/dashboard",
        ),
    )
    .await;
    assert_eq!(
        res.location(),
        "/reauthenticate?action=change_main&return_to=%2Fdashboard"
    );
    assert_eq!(me(&h, &owner).await["main"]["id"], 196379789);
    let res = send(
        &h.app,
        post_from("/profile/main/login", "", &owner, "/dashboard"),
    )
    .await;
    assert!(
        res.location()
            .starts_with("/reauthenticate?action=change_main&"),
        "{}",
        res.location()
    );

    // A pilot without admin powers changes theirs freely.
    let res = send(
        &h.app,
        post_from(
            "/profile/main",
            "character_id=90000003",
            &pilot,
            "/dashboard",
        ),
    )
    .await;
    assert_eq!(res.location(), "/dashboard");
    assert_eq!(me(&h, &pilot).await["main"]["id"], 90000003);
}

/// A stolen, stale owner session can't plant a main of its own: adding a
/// character and deleting the main's token are gated, and a login that
/// takes over a main-less account doesn't count as confirming.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_stale_session_cant_plant_a_main(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, ALT, Some(&owner)).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    age(&h, &owner, 60).await;
    age(&h, &pilot, 60).await;

    let res = send(
        &h.app,
        post_from("/register/start", "", &owner, "/dashboard"),
    )
    .await;
    assert_eq!(
        res.location(),
        "/reauthenticate?action=add_character&return_to=%2Fdashboard"
    );
    let res = send(
        &h.app,
        post_from("/tokens/196379789/delete", "", &owner, "/tokens"),
    )
    .await;
    assert_eq!(
        res.location(),
        "/reauthenticate?action=main_token&return_to=%2Ftokens"
    );
    // An alt's token is everyday work.
    let res = send(
        &h.app,
        post_from("/tokens/90000002/delete", "", &owner, "/tokens"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Token deleted"));
    // And accounts without powers add characters freely.
    let res = send(
        &h.app,
        post_from("/register/start", "", &pilot, "/dashboard"),
    )
    .await;
    assert!(res.location().contains("state="), "{}", res.location());

    // Were the main lost anyway, signing in with the alt makes it the main
    // without a fresh sudo time: one more EVE login, as defence in depth.
    // The barrier is the two gates above (a stale session can't link a
    // character or remove the main's token); once an existing alt is the
    // main, logging in with it again does confirm, as it should.
    sqlx::query("UPDATE core.accounts SET main_character_id = NULL WHERE is_owner")
        .execute(&h.db)
        .await
        .unwrap();
    let session = log_in_as(&h, ALT, None).await;
    let fresh: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT reauthenticated_at FROM core.sessions WHERE token_hash = sha256($1::bytea)",
    )
    .bind(session.as_bytes())
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(fresh, None);
}

/// Letting people into a group that grants a sensitive permission hands it
/// out, as granting it would.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn groups_granting_sensitive_permissions_are_gated(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let mut groups = Vec::new();
    for name in ["Admins", "Miners"] {
        let res = send(
            &h.app,
            post_json(
                "/api/admin/groups",
                &owner,
                &format!(r#"{{"name":"{name}"}}"#),
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
        groups.push(
            serde_json::from_str::<serde_json::Value>(&res.body).unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"admin.users","group_id":{}}}"#, groups[0]),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    age(&h, &owner, 60).await;

    let body = format!(r#"{{"account_id":{pilot_account}}}"#);
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{}/members", groups[0]),
            &owner,
            &body,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{}/members", groups[1]),
            &owner,
            &body,
        ),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
}
