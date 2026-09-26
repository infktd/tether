//! Users (AA's admin site Users): find accounts by any character, see
//! everything about one, deactivate and reactivate it.

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

/// A group granting `permission`, with `account` in it.
async fn grant_via_group(h: &Harness, owner: &str, name: &str, permission: &str, account: i64) {
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            owner,
            &format!(r#"{{"name":"{name}"}}"#),
        ),
    )
    .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            owner,
            &format!(r#"{{"permission":"{permission}","group_id":{group}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{group}/members"),
            owner,
            &format!(r#"{{"account_id":{account}}}"#),
        ),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn finds_accounts_by_any_character(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;

    let denied = page(&h, "/admin/users", &pilot).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // An alt with a wildcard in its name.
    sqlx::query(
        "INSERT INTO core.characters (id, account_id, name) VALUES (90000001, $1, 'Alt_100%')",
    )
    .bind(pilot_account)
    .execute(&h.db)
    .await
    .unwrap();

    let all = page(&h, "/admin/users", &owner).await;
    assert_eq!(all.status, StatusCode::OK);
    assert!(
        all.body
            .contains(&format!(r#"href="/admin/users/{pilot_account}""#))
    );

    let by_alt = page(&h, "/admin/users?q=alt_100%25", &owner).await;
    assert!(by_alt.body.contains("found by Alt_100%"), "{}", by_alt.body);
    assert!(
        by_alt
            .body
            .contains(&format!(r#"href="/admin/users/{pilot_account}""#))
    );
    // `_` and `%` are literal: this matches nobody.
    let literal = page(&h, "/admin/users?q=alt%25100", &owner).await;
    assert!(literal.body.contains("Nobody matches."), "{}", literal.body);
    // By character id.
    let by_id = page(&h, "/admin/users?q=90000001", &owner).await;
    assert!(
        by_id
            .body
            .contains(&format!(r#"href="/admin/users/{pilot_account}""#))
    );

    let inactive = page(&h, "/admin/users?status=inactive", &owner).await;
    assert!(
        inactive.body.contains("Nobody matches."),
        "{}",
        inactive.body
    );

    // The account's page.
    let one = page(&h, &format!("/admin/users/{pilot_account}"), &owner).await;
    assert_eq!(one.status, StatusCode::OK);
    assert!(one.body.contains("Alt_100%"), "{}", one.body);
    assert!(one.body.contains(">Main<"), "{}", one.body);
    assert!(one.body.contains("Deactivate"), "{}", one.body);
    let missing = page(&h, "/admin/users/999999", &owner).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deactivate_and_reactivate(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let owner_account = account_of(&h, &owner).await;

    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/deactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
    let one = page(&h, &format!("/admin/users/{pilot_account}"), &owner).await;
    assert!(one.body.contains("Deactivated"), "{}", one.body);
    assert!(one.body.contains("Reactivate"), "{}", one.body);
    let inactive = page(&h, "/admin/users?status=inactive", &owner).await;
    assert!(
        inactive
            .body
            .contains(&format!(r#"href="/admin/users/{pilot_account}""#))
    );

    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/reactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
    let active: bool = sqlx::query_scalar("SELECT active FROM core.accounts WHERE id = $1")
        .bind(pilot_account)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert!(active);

    // Never the owner.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{owner_account}/deactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains(r#"role="alert""#), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_users_admin_cant_deactivate_someone_holding_more(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    grant_via_group(&h, &owner, "User Admins", "admin.users", pilot_account).await;

    let list = page(&h, "/admin/users", &pilot).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(
        list.body.contains(r#"href="/admin/users""#),
        "the sidebar links it"
    );
    let landing = page(&h, "/admin", &pilot).await;
    assert_eq!(landing.location(), "/admin/users");

    // A second account holding fleet.ping, which the pilot doesn't.
    let other: i64 =
        sqlx::query_scalar("INSERT INTO core.accounts (state_id) VALUES ($1) RETURNING id")
            .bind(MEMBER_STATE)
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO core.permission_grants (permission, state_id) VALUES ('fleet.ping', $1)",
    )
    .bind(MEMBER_STATE)
    .execute(&h.db)
    .await
    .unwrap();
    let res = send(
        &h.app,
        form(&format!("/admin/users/{other}/deactivate"), "", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("fleet.ping"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reactivating_counts_the_groups_the_account_rejoins(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    // gigX is Member, so compliant accounts join the Compliant group.
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let admin = log_in_as(&h, "443630591:The Mittani", None).await;
    let admin_account = account_of(&h, &admin).await;
    grant_via_group(&h, &owner, "User Admins", "admin.users", admin_account).await;
    // Everything Member grants, too: only the Compliant group's grant is
    // missing.
    sqlx::query(
        "INSERT INTO core.permission_grants (permission, group_id)
         SELECT p.permission, g.id FROM core.permission_grants p, core.groups g
         WHERE p.state_id = $1 AND g.name = 'User Admins'
         ON CONFLICT DO NOTHING",
    )
    .bind(MEMBER_STATE)
    .execute(&h.db)
    .await
    .unwrap();

    let compliant: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE name = 'Compliant'")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"fleet.ping","group_id":{compliant}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    assert!(
        me(&h, &pilot).await["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "fleet.ping"),
        "compliant, so it holds fleet.ping"
    );

    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/deactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));

    // Back in, it would rejoin Compliant and hold fleet.ping, which the
    // users admin doesn't: refused, and nothing changes.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/reactivate"),
            "",
            &admin,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("fleet.ping"), "{}", res.body);
    let active: bool = sqlx::query_scalar("SELECT active FROM core.accounts WHERE id = $1")
        .bind(pilot_account)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert!(!active);

    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/reactivate"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
}
