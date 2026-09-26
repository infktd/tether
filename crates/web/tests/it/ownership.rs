//! Login and ownership, Alliance Auth style: only the main signs in; Add
//! Character links (and moves) characters; the ownership check takes away
//! sold characters and characters whose token died; a lost main clears
//! instead of promoting an alt; deactivated accounts can't sign in.

use std::time::Duration;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};

const CHRIBBA: &str = "196379789:Chribba";
const CHRIBBA_ID: i64 = 196379789;
const MITTANI: &str = "443630591:The Mittani";
const MITTANI_ID: i64 = 443630591;
const NOT_MAIN: &str = "Please log in with the main character associated with this account.";

async fn member_harness(db: PgPool) -> Harness {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    // Access tokens are due for refresh at once, so checks really refresh.
    *h.sso.token_ttl.lock().unwrap() = Duration::ZERO;
    h
}

/// Pretends every revocation happened two days ago (past the grace).
async fn age_revocations(db: &PgPool) {
    sqlx::query("UPDATE core.character_tokens SET revoked_at = now() - interval '2 days' WHERE state = 'revoked'")
        .execute(db)
        .await
        .unwrap();
}

async fn ownership_lost(db: &PgPool) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'character.ownership_lost' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_the_main_signs_in(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(
        me(&h, &owner).await["characters"].as_array().unwrap().len(),
        2
    );

    // The alt alone: refused, with AA's message, and no session.
    let res = callback_as(&h, MITTANI, None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains(NOT_MAIN), "{}", res.body);
    // A plain login with it while signed in doesn't link or switch either.
    let res = sign_in_while_signed_in(&h, MITTANI, &owner).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // The main signs in.
    assert!(!log_in_as(&h, CHRIBBA, None).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn add_character_moves_a_character_from_another_account(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let other = log_in_as(&h, MITTANI, None).await;
    assert_eq!(me(&h, &other).await["main"]["name"], "The Mittani");

    // Chribba adds The Mittani (a fresh SSO login proves control): it moves.
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(
        me(&h, &owner).await["characters"].as_array().unwrap().len(),
        2
    );
    // The other account lost its main: Guest, with no main.
    let left = me(&h, &other).await;
    assert!(left["main"].is_null(), "{left}");
    assert_eq!(left["state"], "Guest");
    let lost = ownership_lost(&h.db).await;
    assert_eq!(lost[0]["reason"], "moved");
    assert_eq!(lost[0]["was_main"], true);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_dead_main_token_clears_the_main_until_its_owner_returns(db: PgPool) {
    let h = member_harness(db).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(state_of(&h, &token).await, "Member");

    // Chribba revokes Tether on EVE's site; the ownership check notices,
    // but gives a day's grace before taking the character away.
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Revoked;
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!((checked.checked, checked.lost), (1, 0));
    assert_eq!(me(&h, &token).await["main"]["id"], CHRIBBA_ID);
    age_revocations(&h.db).await;
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(checked.lost, 1);
    let after = me(&h, &token).await;
    assert!(after["main"].is_null(), "{after}");
    assert_eq!(after["state"], "Guest");
    let dashboard = page(&h, "/dashboard", &token).await.body;
    assert!(
        dashboard.contains("Your account has no main character"),
        "{dashboard}"
    );
    assert_eq!(ownership_lost(&h.db).await[0]["reason"], "token");

    // Signing in again with the character brings it back as the main.
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Rotate;
    let back = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(me(&h, &back).await["main"]["name"], "Chribba");
    assert_eq!(state_of(&h, &back).await, "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sold_alt_is_caught_on_refresh(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;

    // The Mittani is sold to another EVE account.
    h.sso
        .owner_hashes
        .lock()
        .unwrap()
        .insert(MITTANI_ID, "someone-else".into());
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(checked.lost, 1);
    let account = me(&h, &owner).await;
    assert_eq!(account["characters"].as_array().unwrap().len(), 1);
    assert_eq!(account["main"]["id"], CHRIBBA_ID);
    assert_eq!(state_of(&h, &owner).await, "Member");
    assert_eq!(ownership_lost(&h.db).await[0]["reason"], "sold");
    // Checked: not refreshed again for 4 hours.
    let again = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(again.checked, 0);
}

async fn change_main(h: &Harness, session: &str, character_id: i64) -> Res {
    send(
        &h.app,
        post_json(
            "/api/me/main",
            session,
            &format!(r#"{{"character_id":{character_id}}}"#),
        ),
    )
    .await
}

/// Change Main through EVE SSO (AA's "add new token" on Change Main):
/// logs in as `character` from the signed-in browser.
async fn change_main_by_login(h: &Harness, character: &str, session: &str) -> Res {
    let res = send(
        &h.app,
        axum::http::Request::post("/profile/main/login")
            .header(axum::http::header::ORIGIN, SITE)
            .header(axum::http::header::COOKIE, format!("{SESSION}={session}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let state = query_param(res.location(), "state").to_owned();
    let browser = res.cookie_value(LOGIN);
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

async fn mains_set(db: &PgPool) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'account.main_set' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_moves_the_state_and_is_audited(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(state_of(&h, &owner).await, "Member");

    // From the Dashboard (htmx): the page reloads, as the sidebar, banner
    // and state follow the main.
    let res = send(
        &h.app,
        axum::http::Request::post("/profile/main")
            .header(axum::http::header::ORIGIN, SITE)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(axum::http::header::COOKIE, format!("{SESSION}={owner}"))
            .header("HX-Request", "true")
            .body(axum::body::Body::from(format!("character_id={MITTANI_ID}")))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.headers["HX-Refresh"], "true");
    assert_eq!(me(&h, &owner).await["main"]["id"], MITTANI_ID);
    // The Mittani's corporation is an NPC one: Guest.
    assert_eq!(state_of(&h, &owner).await, "Guest");
    let audited = mains_set(&h.db).await;
    let last = audited.last().unwrap();
    assert_eq!(last["character_id"], MITTANI_ID);
    assert_eq!(last["previous"], CHRIBBA_ID);
    assert_eq!(last["how"], "change main");

    // Back again through the API; asking for the main it already is is
    // fine and changes nothing.
    assert_eq!(
        change_main(&h, &owner, CHRIBBA_ID).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(state_of(&h, &owner).await, "Member");
    let count = mains_set(&h.db).await.len();
    assert_eq!(
        change_main(&h, &owner, CHRIBBA_ID).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(mains_set(&h.db).await.len(), count);

    // Never someone else's character.
    let other = log_in_as(&h, "1887431749:gigX", None).await;
    let res = change_main(&h, &owner, 1887431749).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.contains("isn't on your account"), "{}", res.body);
    assert_eq!(me(&h, &other).await["main"]["id"], 1887431749_i64);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_needs_a_working_token(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked' WHERE character_id = $1")
        .bind(MITTANI_ID)
        .execute(&h.db)
        .await
        .unwrap();
    let res = change_main(&h, &owner, MITTANI_ID).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert!(
        res.body.contains("EVE access to The Mittani has ended"),
        "{}",
        res.body
    );
    assert_eq!(me(&h, &owner).await["main"]["id"], CHRIBBA_ID);
    // The Dashboard offers a login with it instead of the direct button.
    let dashboard = page(&h, "/dashboard", &owner).await.body;
    assert!(dashboard.contains("Log in to Change Main"), "{dashboard}");
    // Each attempt may call EVE SSO: limited with Token Management's
    // refreshes (10 a minute per account).
    for _ in 1..10 {
        assert_eq!(
            change_main(&h, &owner, MITTANI_ID).await.status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        change_main(&h, &owner, MITTANI_ID).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Logging in with it proves control again, and makes it the main.
    let res = change_main_by_login(&h, MITTANI, &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let owner = res.cookie_value(SESSION);
    let account = me(&h, &owner).await;
    assert_eq!(account["main"]["id"], MITTANI_ID);
    assert_eq!(account["characters"].as_array().unwrap().len(), 2);
    assert_eq!(state_of(&h, &owner).await, "Guest");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_catches_a_sale_at_once(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;

    // Sold since the last ownership check: the expired token is refreshed
    // first, as AA's `require_valid`, and shows the new owner.
    h.sso
        .owner_hashes
        .lock()
        .unwrap()
        .insert(MITTANI_ID, "someone-else".into());
    let res = change_main(&h, &owner, MITTANI_ID).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert!(
        res.body.contains("moved to another EVE account"),
        "{}",
        res.body
    );
    let account = me(&h, &owner).await;
    assert_eq!(account["main"]["id"], CHRIBBA_ID);
    assert_eq!(account["characters"].as_array().unwrap().len(), 1);
    assert_eq!(ownership_lost(&h.db).await[0]["reason"], "sold");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_works_while_sso_is_down(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    // The stored token state (kept by the ownership check) decides.
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Unavailable;
    assert_eq!(
        change_main(&h, &owner, MITTANI_ID).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(me(&h, &owner).await["main"]["id"], MITTANI_ID);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn change_main_by_login_adds_or_moves_the_character(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;

    // A character new to Tether joins the account as its main.
    let res = change_main_by_login(&h, MITTANI, &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let owner = res.cookie_value(SESSION);
    let account = me(&h, &owner).await;
    assert_eq!(account["main"]["id"], MITTANI_ID);
    assert_eq!(account["characters"].as_array().unwrap().len(), 2);
    assert_eq!(account["is_owner"], true);
    assert_eq!(mains_set(&h.db).await.last().unwrap()["how"], "change main");

    // One on another account moves here (as Add Character does, AA's
    // token rule), and that account loses its main.
    let other = log_in_as(&h, "1887431749:gigX", None).await;
    let res = change_main_by_login(&h, "1887431749:gigX", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let owner = res.cookie_value(SESSION);
    assert_eq!(me(&h, &owner).await["main"]["id"], 1887431749_i64);
    let left = me(&h, &other).await;
    assert!(left["main"].is_null(), "{left}");
    assert_eq!(ownership_lost(&h.db).await[0]["reason"], "moved");

    // Only from the session that started it.
    let stranger = log_in_as(&h, "406944591:mynnna", None).await;
    let started = send(
        &h.app,
        axum::http::Request::post("/profile/main/login")
            .header(axum::http::header::ORIGIN, SITE)
            .header(axum::http::header::COOKIE, format!("{SESSION}={owner}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    let state = query_param(started.location(), "state").to_owned();
    let browser = started.cookie_value(LOGIN);
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{CHRIBBA}&state={state}"),
            &[(LOGIN, browser.as_str()), (SESSION, stranger.as_str())],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(me(&h, &owner).await["main"]["id"], 1887431749_i64);

    // Signed out: to the login page.
    let res = send(
        &h.app,
        axum::http::Request::post("/profile/main/login")
            .header(axum::http::header::ORIGIN, SITE)
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.location(), "/login");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_account_without_a_main_changes_main_to_an_alt(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, MITTANI).await;
    let owner = log_in_as(&h, CHRIBBA, Some(&owner)).await;
    let account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    // Its main was lost (sold, say): Guest, and the banner points here.
    sqlx::query("UPDATE core.accounts SET main_character_id = NULL WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
    assert_eq!(state_of(&h, &owner).await, "Guest");
    let dashboard = page(&h, "/dashboard", &owner).await.body;
    assert!(
        dashboard.contains("Your account has no main character"),
        "{dashboard}"
    );

    assert_eq!(
        change_main(&h, &owner, CHRIBBA_ID).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(me(&h, &owner).await["main"]["id"], CHRIBBA_ID);
    assert_eq!(state_of(&h, &owner).await, "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deactivated_accounts_are_guest_signed_out_and_refused(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    let pilot_id = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let owner_id = me(&h, &owner).await["account_id"].as_i64().unwrap();

    // Admins only; never the owner.
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{owner_id}/deactivate"),
            &pilot,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{owner_id}/deactivate"),
            &owner,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{pilot_id}/deactivate"),
            &owner,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    // Their session is gone, and they can't sign in again.
    let res = send(&h.app, get("/api/me", &[(SESSION, &pilot)])).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = callback_as(&h, "1887431749:gigX", None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("deactivated"), "{}", res.body);
    let state: String = sqlx::query_scalar(
        "SELECT s.name FROM core.accounts a JOIN core.states s ON s.id = a.state_id WHERE a.id = $1",
    )
    .bind(pilot_id)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(state, "Guest");

    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{pilot_id}/reactivate"),
            &owner,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(me(&h, &pilot).await["account_id"], pilot_id);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM core.audit_log WHERE action LIKE 'account.%' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(actions, ["account.deactivate", "account.reactivate"]);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn many_revocations_at_once_trip_the_breaker(db: PgPool) {
    let h = member_harness(db).await;
    let mut sessions = Vec::new();
    for i in 0..7 {
        sessions.push(log_in_as(&h, &format!("{}:Pilot {i}", 90000100 + i), None).await);
    }
    // A wrong client id or CCP suspending the app looks like every token
    // dying: nobody loses anything.
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Revoked;
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!((checked.checked, checked.lost), (7, 0));
    // Nor later: most of the instance's tokens dead at once is never a
    // reason to strip everyone. An admin looks into it.
    age_revocations(&h.db).await;
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(checked.lost, 0);
    for session in &sessions {
        assert!(!me(&h, session).await["main"].is_null());
    }
    // It's recorded for `doctor`; an admin who checked can force it.
    let tripped = tether_db::settings::get(&h.db, tether_web::ownership::BREAKER_SETTING)
        .await
        .unwrap();
    assert!(tripped.is_some());
    let lost = tether_web::ownership::sweep_dead(&h.db, true)
        .await
        .unwrap();
    assert_eq!(lost, 7);
    let cleared = tether_db::settings::get(&h.db, tether_web::ownership::BREAKER_SETTING)
        .await
        .unwrap();
    assert!(cleared.is_none());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_dead_token_never_takes_the_owners_last_character(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    *h.sso.refresh_outcome.lock().unwrap() = RefreshOutcome::Revoked;
    tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    age_revocations(&h.db).await;
    let checked = tether_web::ownership::check(&h.db, &h.vault).await.unwrap();
    assert_eq!(checked.lost, 0);
    let me_now = me(&h, &owner).await;
    assert_eq!(me_now["is_owner"], true);
    assert_eq!(me_now["main"]["id"], CHRIBBA_ID);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deactivation_leaves_groups_and_cant_be_undone_from_a_fresh_account(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;
    let pilot_id = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let officers = tether_db::groups::create(
        &h.db,
        "Officers",
        "",
        tether_core::groups::Flags {
            internal: true,
            hidden: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tether_db::groups::add_member(&h.db, officers, tether_db::accounts::AccountId(pilot_id))
        .await
        .unwrap();

    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{pilot_id}/deactivate"),
            &owner,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let groups: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.group_members WHERE account_id = $1")
            .bind(pilot_id)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(groups, 0);

    // A fresh account can't take the deactivated account's main.
    let fresh = log_in_as(&h, "90000200:Fresh Alpha", None).await;
    let res = callback_as(&h, MITTANI, Some(&fresh)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(
        me(&h, &fresh).await["characters"].as_array().unwrap().len(),
        1
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admin_users_cant_deactivate_someone_holding_more(db: PgPool) {
    use tether_core::states::StateId;
    use tether_db::permissions::{Grantee, grant};
    let h = member_harness(db).await;
    let _owner = log_in_owner(&h, CHRIBBA).await;
    // A helper with admin.users (via Blue) and an auditor with admin.audit.
    let helper = log_in_as(&h, "1887431749:gigX", None).await;
    let auditor = log_in_as(&h, MITTANI, None).await;
    let helper_id = me(&h, &helper).await["account_id"].as_i64().unwrap();
    let auditor_id = me(&h, &auditor).await["account_id"].as_i64().unwrap();
    let group = tether_db::groups::create(
        &h.db,
        "Helpers",
        "",
        tether_core::groups::Flags {
            internal: true,
            hidden: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tether_db::groups::add_member(&h.db, group, tether_db::accounts::AccountId(helper_id))
        .await
        .unwrap();
    grant(&h.db, "admin.users", Grantee::Group(group))
        .await
        .unwrap();
    let auditors = tether_db::groups::create(
        &h.db,
        "Auditors",
        "",
        tether_core::groups::Flags {
            internal: true,
            hidden: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tether_db::groups::add_member(&h.db, auditors, tether_db::accounts::AccountId(auditor_id))
        .await
        .unwrap();
    grant(&h.db, "admin.audit", Grantee::Group(auditors))
        .await
        .unwrap();
    let _ = StateId(0);

    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{auditor_id}/deactivate"),
            &helper,
            "{}",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("admin.audit"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_account_without_a_main_still_opens_its_dashboard_and_loses_group_access(db: PgPool) {
    use tether_db::permissions::{Grantee, grant};
    let h = member_harness(db).await;
    let _owner = log_in_owner(&h, "1887431749:gigX").await;
    let seller = log_in_as(&h, CHRIBBA, None).await;
    let seller = log_in_as(&h, MITTANI, Some(&seller)).await;
    let seller_id = me(&h, &seller).await["account_id"].as_i64().unwrap();
    let group = tether_db::groups::create(
        &h.db,
        "Auditors",
        "",
        tether_core::groups::Flags {
            internal: true,
            hidden: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tether_db::groups::add_member(&h.db, group, tether_db::accounts::AccountId(seller_id))
        .await
        .unwrap();
    grant(&h.db, "admin.audit", Grantee::Group(group))
        .await
        .unwrap();
    assert!(
        me(&h, &seller).await["permissions"]
            .to_string()
            .contains("admin.audit")
    );

    // The main is sold; the alt stays.
    h.sso
        .owner_hashes
        .lock()
        .unwrap()
        .insert(CHRIBBA_ID, "buyer".into());
    log_in_as(&h, CHRIBBA, None).await;

    let left = me(&h, &seller).await;
    assert!(left["main"].is_null());
    assert_eq!(
        left["permissions"],
        serde_json::json!([]),
        "groups pause without a main"
    );
    let dashboard = page(&h, "/dashboard", &seller).await;
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    assert!(
        dashboard
            .body
            .contains("Your account has no main character")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_deactivated_accounts_characters_cant_escape_through_revocation(db: PgPool) {
    let h = member_harness(db).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;
    let pilot_id = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    send(
        &h.app,
        post_json(
            &format!("/api/admin/accounts/{pilot_id}/deactivate"),
            &owner,
            "{}",
        ),
    )
    .await;

    // The pilot revokes their tokens: the sweep leaves a deactivated
    // account's characters alone.
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked', revoked_reason = 'invalid_grant', revoked_at = now() - interval '2 days' WHERE character_id = $1")
        .bind(MITTANI_ID)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        tether_web::ownership::sweep_dead(&h.db, false)
            .await
            .unwrap(),
        0
    );
    // Even if the character were gone, a fresh account can't take it: its
    // last ownership was the deactivated account's.
    sqlx::query("UPDATE core.accounts SET main_character_id = NULL WHERE id = $1")
        .bind(pilot_id)
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM core.characters WHERE id = $1")
        .bind(MITTANI_ID)
        .execute(&h.db)
        .await
        .unwrap();
    let fresh = log_in_as(&h, "90000300:Fresh Alpha", None).await;
    let res = callback_as(&h, MITTANI, Some(&fresh)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
}
