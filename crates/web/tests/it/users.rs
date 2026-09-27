//! Users (AA's admin site Users): find accounts by any character, see
//! everything about one, deactivate and reactivate it, grant it
//! permissions of its own, and make it a superuser.

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
        "the rail links it"
    );
    let landing = page(&h, "/admin", &pilot).await;
    assert!(
        landing.body.contains(r#"href="/admin/users""#),
        "{}",
        landing.body
    );

    // A second account holding fleetpings.basic_access, which the pilot doesn't.
    let other: i64 =
        sqlx::query_scalar("INSERT INTO core.accounts (state_id) VALUES ($1) RETURNING id")
            .bind(MEMBER_STATE)
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO core.permission_grants (permission, state_id) VALUES ('fleetpings.basic_access', $1)",
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
    assert!(res.body.contains("fleetpings.basic_access"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reactivating_counts_the_groups_the_account_rejoins(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    // gigX is Member, so compliant accounts join the Compliant group.
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    // Registered with Member's scopes, so compliant.
    let pilot = log_in_as(&h, GIGX, Some(&pilot)).await;
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
            &format!(r#"{{"permission":"fleetpings.basic_access","group_id":{compliant}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    assert!(
        me(&h, &pilot).await["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "fleetpings.basic_access"),
        "compliant, so it holds fleetpings.basic_access"
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

    // Back in, it would rejoin Compliant and hold fleetpings.basic_access, which the
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
    assert!(res.body.contains("fleetpings.basic_access"), "{}", res.body);
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

async fn is_superuser(h: &Harness, token: &str) -> bool {
    me(h, token).await["is_owner"].as_bool().unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn superusers_make_and_unmake_superusers(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let owner_account = account_of(&h, &owner).await;
    let pilot_account = account_of(&h, &pilot).await;

    let one = page(&h, &format!("/admin/users/{pilot_account}"), &owner).await;
    assert!(one.body.contains("Make superuser"), "{}", one.body);
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/superuser"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
    assert!(is_superuser(&h, &pilot).await, "any number, as AA's");
    assert!(is_superuser(&h, &owner).await);

    // The new superuser can unmake the first...
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{owner_account}/superuser/revoke"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{owner_account}"));
    assert!(!is_superuser(&h, &owner).await);
    // ...but not themselves, the last one.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/superuser/revoke"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(is_superuser(&h, &pilot).await);

    // A users admin who isn't a superuser can't make one.
    grant_via_group(&h, &pilot, "User Admins", "admin.users", owner_account).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{owner_account}/superuser"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(!is_superuser(&h, &owner).await);

    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM core.audit_log WHERE action LIKE 'account.superuser%' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        audited,
        ["account.superuser_grant", "account.superuser_revoke"]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permissions_granted_to_a_single_user(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let admin = log_in_as(&h, "443630591:The Mittani", None).await;
    let admin_account = account_of(&h, &admin).await;

    // A superuser grants it on the user's page.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/permissions"),
            "permission=admin.audit",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
    let audit = send(&h.app, get("/api/admin/audit", &[(SESSION, &pilot)])).await;
    assert_eq!(audit.status, StatusCode::OK, "{}", audit.body);
    // Listed on Permissions and in the Permissions Audit ("via user").
    let listed = page(&h, "/admin/permissions", &owner).await;
    assert!(
        listed
            .body
            .contains(&format!(r#"href="/admin/users/{pilot_account}""#)),
        "{}",
        listed.body
    );
    let counts: serde_json::Value = serde_json::from_str(
        &send(&h.app, get("/api/admin/permissions", &[(SESSION, &owner)]))
            .await
            .body,
    )
    .unwrap();
    let grant = counts["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["account_id"] == pilot_account)
        .unwrap()
        .clone();
    assert_eq!(grant["permission"], "admin.audit");
    let holders = page(&h, "/admin/permissions/audit/admin.audit", &owner).await;
    assert!(holders.body.contains(">User<"), "{}", holders.body);

    // Nobody grants themselves.
    let admin_self = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(
                r#"{{"permission":"admin.audit","account_id":{}}}"#,
                account_of(&h, &owner).await
            ),
        ),
    )
    .await;
    assert_eq!(
        admin_self.status,
        StatusCode::FORBIDDEN,
        "{}",
        admin_self.body
    );

    // An admin grants only what they hold.
    grant_via_group(
        &h,
        &owner,
        "Permission Admins",
        "admin.permissions",
        admin_account,
    )
    .await;
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &admin,
            &format!(r#"{{"permission":"fleetpings.basic_access","account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    // Nor revoke one they don't hold.
    let grant_id = grant["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/permissions/{grant_id}/revoke"),
            "",
            &admin,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // A superuser revokes it.
    let res = send(
        &h.app,
        form(
            &format!("/admin/users/{pilot_account}/permissions/{grant_id}/revoke"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), format!("/admin/users/{pilot_account}"));
    let audit = send(&h.app, get("/api/admin/audit", &[(SESSION, &pilot)])).await;
    assert_eq!(audit.status, StatusCode::FORBIDDEN);
}
