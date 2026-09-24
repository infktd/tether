#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use serde_json::Value;
use sqlx::PgPool;

fn authed(method: &str, uri: &str, token: &str, body: Option<&str>) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
        .unwrap()
}

async fn call(h: &Harness, method: &str, uri: &str, token: &str, body: Option<&str>) -> Res {
    send(&h.app, authed(method, uri, token, body)).await
}

fn json(res: &Res) -> Value {
    serde_json::from_str(&res.body).unwrap_or_else(|_| panic!("not JSON: {}", res.body))
}

/// Owner (first login) and an ordinary pilot.
async fn owner_and_pilot(h: &Harness) -> (String, String) {
    let owner = log_in_as(h, "90000001:Owner", None).await;
    let pilot = log_in_as(h, "90000002:Pilot", None).await;
    (owner, pilot)
}

async fn create_group(h: &Harness, token: &str, name: &str, policy: &str) -> i64 {
    let body = format!(r#"{{"name":"{name}","join_policy":"{policy}"}}"#);
    let res = call(h, "POST", "/api/admin/groups", token, Some(&body)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    json(&res)["id"].as_i64().unwrap()
}

async fn my_groups(h: &Harness, token: &str) -> Vec<Value> {
    me(h, token).await["groups"].as_array().unwrap().clone()
}

async fn audit_actions(h: &Harness, owner: &str) -> Vec<(String, Option<String>)> {
    let res = call(h, "GET", "/api/admin/audit", owner, None).await;
    assert_eq!(res.status, StatusCode::OK);
    json(&res)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap().to_owned(),
                e["actor_name"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_admins_manage_groups(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;

    let res = call(
        &h,
        "POST",
        "/api/admin/groups",
        &pilot,
        Some(r#"{"name":"X","join_policy":"open"}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    create_group(&h, &owner, "Miners", "open").await;
    let dup = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"Miners","join_policy":"open"}"#),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    let bad = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"Y","join_policy":"secret"}"#),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn open_groups_are_joined_and_left_freely(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Miners", "open").await;

    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(json(&res)["status"], "member");
    assert_eq!(my_groups(&h, &pilot).await, vec!["Miners"]);

    let res = call(&h, "POST", &format!("/api/groups/{id}/leave"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(my_groups(&h, &pilot).await.is_empty());

    let actions = audit_actions(&h, &owner).await;
    assert_eq!(actions[0], ("group.leave".into(), Some("Pilot".into())));
    assert_eq!(actions[1], ("group.join".into(), Some("Pilot".into())));
    assert_eq!(actions[2], ("group.create".into(), Some("Owner".into())));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn request_groups_need_approval(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let third = log_in_as(&h, "90000003:Third", None).await;
    let id = create_group(&h, &owner, "Capitals", "request").await;

    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(json(&res)["status"], "requested");
    call(&h, "POST", &format!("/api/groups/{id}/join"), &third, None).await;
    assert!(my_groups(&h, &pilot).await.is_empty());

    let listed = call(&h, "GET", "/api/groups", &pilot, None).await;
    assert_eq!(json(&listed)[0]["has_requested"], true);

    let requests = call(
        &h,
        "GET",
        &format!("/api/admin/groups/{id}/requests"),
        &owner,
        None,
    )
    .await;
    let requests = json(&requests);
    assert_eq!(requests.as_array().unwrap().len(), 2);
    let pilot_account = requests[0]["account_id"].as_i64().unwrap();
    let third_account = requests[1]["account_id"].as_i64().unwrap();

    let approve = format!("/api/admin/groups/{id}/requests/{pilot_account}/approve");
    assert_eq!(
        call(&h, "POST", &approve, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(my_groups(&h, &pilot).await, vec!["Capitals"]);
    // A request can only be approved once.
    assert_eq!(
        call(&h, "POST", &approve, &owner, None).await.status,
        StatusCode::NOT_FOUND
    );

    let deny = format!("/api/admin/groups/{id}/requests/{third_account}/deny");
    assert_eq!(
        call(&h, "POST", &deny, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(my_groups(&h, &third).await.is_empty());

    let actions: Vec<String> = audit_actions(&h, &owner)
        .await
        .into_iter()
        .map(|a| a.0)
        .collect();
    assert_eq!(
        &actions[..4],
        [
            "group.request.deny",
            "group.request.approve",
            "group.request",
            "group.request"
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn assigned_groups_are_admin_only(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Officers", "assigned").await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();

    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    let body = format!(r#"{{"account_id":{pilot_account}}}"#);
    let add = call(
        &h,
        "POST",
        &format!("/api/admin/groups/{id}/members"),
        &owner,
        Some(&body),
    )
    .await;
    assert_eq!(add.status, StatusCode::NO_CONTENT);
    assert_eq!(my_groups(&h, &pilot).await, vec!["Officers"]);

    let missing = call(
        &h,
        "POST",
        &format!("/api/admin/groups/{id}/members"),
        &owner,
        Some(r#"{"account_id":999}"#),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let remove = format!("/api/admin/groups/{id}/members/{pilot_account}");
    assert_eq!(
        call(&h, "DELETE", &remove, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(my_groups(&h, &pilot).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permissions_granted_to_groups_and_tiers_take_effect(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let officers = create_group(&h, &owner, "Officers", "assigned").await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();

    assert_eq!(
        call(&h, "GET", "/api/admin/audit", &pilot, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    let body = format!(r#"{{"permission":"admin.audit","group_id":{officers}}}"#);
    let grant = call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&body),
    )
    .await;
    assert_eq!(grant.status, StatusCode::CREATED, "{}", grant.body);
    let grant_id = json(&grant)["id"].as_i64().unwrap();
    let dup = call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&body),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    // Not yet in the group.
    assert_eq!(
        call(&h, "GET", "/api/admin/audit", &pilot, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let member = format!(r#"{{"account_id":{pilot_account}}}"#);
    call(
        &h,
        "POST",
        &format!("/api/admin/groups/{officers}/members"),
        &owner,
        Some(&member),
    )
    .await;
    assert_eq!(
        call(&h, "GET", "/api/admin/audit", &pilot, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        me(&h, &pilot).await["permissions"],
        serde_json::json!(["admin.audit"])
    );

    // Revoking removes it again.
    let revoke = format!("/api/admin/permissions/grants/{grant_id}");
    assert_eq!(
        call(&h, "DELETE", &revoke, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&h, "GET", "/api/admin/audit", &pilot, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    // Grants to the pilot's tier (guest here) apply too.
    let body = r#"{"permission":"admin.groups","tier":"guest"}"#;
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(body),
    )
    .await;
    assert_eq!(
        me(&h, &pilot).await["permissions"],
        serde_json::json!(["admin.groups"])
    );

    let actions: Vec<String> = audit_actions(&h, &owner)
        .await
        .into_iter()
        .map(|a| a.0)
        .collect();
    assert!(actions.contains(&"permission.grant".to_owned()));
    assert!(actions.contains(&"permission.revoke".to_owned()));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn invalid_grants_are_rejected(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let grant = |body: &'static str| {
        let (h, owner) = (&h, owner.clone());
        async move {
            call(
                h,
                "POST",
                "/api/admin/permissions/grants",
                &owner,
                Some(body),
            )
            .await
            .status
        }
    };

    assert_eq!(
        grant(r#"{"permission":"admin.everything","tier":"member"}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit"}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit","tier":"member","group_id":1}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit","tier":"admiral"}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit","group_id":999}"#).await,
        StatusCode::NOT_FOUND
    );

    let res = call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &pilot,
        Some(r#"{"permission":"admin.audit","tier":"guest"}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let listed = call(&h, "GET", "/api/admin/permissions", &owner, None).await;
    assert_eq!(json(&listed)["grants"], serde_json::json!([]));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deleting_a_group_is_audited_with_its_name(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Temporary", "open").await;

    assert_eq!(
        call(
            &h,
            "DELETE",
            &format!("/api/admin/groups/{id}"),
            &owner,
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &h,
            "DELETE",
            &format!("/api/admin/groups/{id}"),
            &owner,
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    let res = call(&h, "GET", "/api/admin/audit?limit=1", &owner, None).await;
    let entry = &json(&res)[0];
    assert_eq!(entry["action"], "group.delete");
    assert_eq!(entry["details"]["name"], "Temporary");
    assert_eq!(entry["target"], format!("group:{id}"));
}
