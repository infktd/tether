//! Permissions Audit (AA's permissions tool): every permission, how widely
//! it's held, and who holds it through what.

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn lists_who_holds_a_permission_and_through_what(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;

    // Not an auditor yet.
    let denied = page(&h, "/admin/permissions/audit", &pilot).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    // A group grants fleet.ping; the pilot joins it.
    let group = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"FCs"}"#),
    )
    .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"fleet.ping","group_id":{group}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{group}/members"),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);

    let list = page(&h, "/admin/permissions/audit", &owner).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.body
            .contains(r#"href="/admin/permissions/audit/fleet.ping""#),
        "{}",
        list.body
    );
    let held = tether_db::permissions_audit::counts(&h.db, "fleet.ping")
        .await
        .unwrap();
    // The group, and the owner plus the pilot.
    assert_eq!((held.groups, held.accounts), (1, 2));

    let one = page(&h, "/admin/permissions/audit/fleet.ping", &owner).await;
    assert_eq!(one.status, StatusCode::OK);
    assert!(one.body.contains("Group: FCs"), "{}", one.body);
    assert!(one.body.contains(">Owner<"), "{}", one.body);

    // Deactivated accounts hold nothing.
    sqlx::query("UPDATE core.accounts SET active = false WHERE id = $1")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    let one = page(&h, "/admin/permissions/audit/fleet.ping", &owner).await;
    assert!(!one.body.contains("Group: FCs"), "{}", one.body);

    let missing = page(&h, "/admin/permissions/audit/no.such", &owner).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_audit_permission_opens_it(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let group = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Auditors"}"#),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"permissions_tool.audit_permissions","group_id":{group}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{group}/members"),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    let landing = page(&h, "/admin", &pilot).await;
    assert_eq!(landing.status, StatusCode::OK, "{}", landing.body);
    assert!(landing.body.contains(r#"href="/admin/permissions/audit""#));
    let list = page(&h, "/admin/permissions/audit", &pilot).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(
        list.body.contains(r#"href="/admin/permissions/audit""#),
        "the rail links it"
    );
}
