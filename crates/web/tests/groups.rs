#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use serde_json::Value;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};

const PILOT: i64 = 90000002;
const THIRD: i64 = 90000003;

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
    let owner = log_in_owner(h, "90000001:Owner").await;
    let pilot = log_in_as(h, "90000002:Pilot", None).await;
    (owner, pilot)
}

/// Makes these characters Members (who may request groups).
async fn members(db: &PgPool, ids: &[i64]) {
    for id in ids {
        cover(db, Builtin::Member, EntityKind::Character, *id).await;
    }
}

/// `kind`: `internal` (AA's default), `open`, `requestable`, `hidden`
/// (Open but unlisted) or `public` (requestable without request_groups).
async fn create_group(h: &Harness, token: &str, name: &str, kind: &str) -> i64 {
    let flags = match kind {
        "internal" => "",
        "open" => r#","internal":false,"hidden":false,"open":true"#,
        "requestable" => r#","internal":false,"hidden":false"#,
        "hidden" => r#","internal":false,"open":true"#,
        "public" => r#","internal":false,"hidden":false,"public":true"#,
        other => panic!("unknown kind {other}"),
    };
    let body = format!(r#"{{"name":"{name}"{flags}}}"#);
    let res = call(h, "POST", "/api/admin/groups", token, Some(&body)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    json(&res)["id"].as_i64().unwrap()
}

/// The account's groups, leaving out the Compliant group (a compliance
/// group every compliant Member is in).
async fn my_groups(h: &Harness, token: &str) -> Vec<Value> {
    me(h, token).await["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| *g != "Compliant")
        .cloned()
        .collect()
}

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

async fn listed(h: &Harness, token: &str) -> Vec<Value> {
    let res = call(h, "GET", "/api/groups", token, None).await;
    assert_eq!(res.status, StatusCode::OK);
    json(&res)
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["name"] != "Compliant")
        .cloned()
        .collect()
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

/// The group's Audit Log as `(type, action, requestor, actor)`, newest
/// first.
async fn group_log(h: &Harness, token: &str, id: i64) -> Vec<(String, String, String, String)> {
    let res = call(
        h,
        "GET",
        &format!("/api/group-management/groups/{id}/audit-log"),
        token,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    json(&res)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let s = |k: &str| e[k].as_str().unwrap_or_default().to_owned();
            (
                s("request_type"),
                s("action"),
                s("requestor_main"),
                s("actor_name"),
            )
        })
        .collect()
}

fn entry(
    kind: &str,
    action: &str,
    requestor: &str,
    actor: &str,
) -> (String, String, String, String) {
    (kind.into(), action.into(), requestor.into(), actor.into())
}

async fn settings(h: &Harness, token: &str, id: i64, body: &str) -> Res {
    call(
        h,
        "PUT",
        &format!("/api/admin/groups/{id}"),
        token,
        Some(body),
    )
    .await
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
        Some(r#"{"name":"X"}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    create_group(&h, &owner, "Miners", "open").await;
    let dup = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"Miners"}"#),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    // Reserved names are refused, ignoring case, and a taken name can't be
    // reserved.
    let reserve = |body: &'static str| {
        call(
            &h,
            "POST",
            "/api/admin/reserved-group-names",
            &owner,
            Some(body),
        )
    };
    assert_eq!(
        reserve(r#"{"name":"Directors","reason":"CEO only"}"#)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        reserve(r#"{"name":"miners","reason":"taken"}"#)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    let refused = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"DIRECTORS"}"#),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(refused.body.contains("reserved"));
    let names = call(&h, "GET", "/api/admin/reserved-group-names", &owner, None).await;
    assert_eq!(json(&names)[0]["name"], "directors");
    assert_eq!(
        call(
            &h,
            "DELETE",
            "/api/admin/reserved-group-names/Directors",
            &owner,
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    create_group(&h, &owner, "Directors", "internal").await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn open_groups_are_joined_and_left_at_once(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Miners", "open").await;

    assert_eq!(listed(&h, &pilot).await[0]["kind"], "open");
    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(json(&res)["status"], "member");
    assert_eq!(my_groups(&h, &pilot).await, vec!["Miners"]);
    let again = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    let res = call(&h, "POST", &format!("/api/groups/{id}/leave"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(json(&res)["status"], "left");
    assert!(my_groups(&h, &pilot).await.is_empty());

    let actions = audit_actions(&h, &owner).await;
    assert_eq!(actions[0], ("group.leave".into(), Some("Pilot".into())));
    assert_eq!(actions[1], ("group.join".into(), Some("Pilot".into())));
    assert_eq!(actions[2], ("group.create".into(), Some("Owner".into())));
    // In the group's Audit Log, as their own actor (AA).
    assert_eq!(
        group_log(&h, &owner, id).await,
        [
            entry("leave", "accept", "Pilot", "Pilot"),
            entry("join", "accept", "Pilot", "Pilot")
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn requestable_groups_need_their_leaders(db: PgPool) {
    members(&db, &[PILOT, THIRD]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let third = log_in_as(&h, "90000003:Third", None).await;
    let id = create_group(&h, &owner, "Capitals", "requestable").await;
    let (pilot_account, third_account) =
        (account_of(&h, &pilot).await, account_of(&h, &third).await);

    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(json(&res)["status"], "requested");
    assert_eq!(listed(&h, &pilot).await[0]["pending"], "join");
    let twice = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(twice.status, StatusCode::CONFLICT);
    // Retracting a join request isn't logged.
    assert_eq!(
        call(
            &h,
            "POST",
            &format!("/api/groups/{id}/retract"),
            &pilot,
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    call(&h, "POST", &format!("/api/groups/{id}/join"), &third, None).await;
    assert!(my_groups(&h, &pilot).await.is_empty());

    let requests = call(&h, "GET", "/api/group-management/requests", &owner, None).await;
    assert_eq!(json(&requests).as_array().unwrap().len(), 2);
    // Plain pilots manage nothing.
    let none = call(&h, "GET", "/api/group-management/requests", &pilot, None).await;
    assert_eq!(json(&none), serde_json::json!([]));

    let accept = format!("/api/group-management/groups/{id}/requests/{pilot_account}/accept");
    assert_eq!(
        call(&h, "POST", &accept, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(my_groups(&h, &pilot).await, vec!["Capitals"]);
    assert_eq!(
        call(&h, "POST", &accept, &owner, None).await.status,
        StatusCode::NOT_FOUND
    );
    let reject = format!("/api/group-management/groups/{id}/requests/{third_account}/reject");
    assert_eq!(
        call(&h, "POST", &reject, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(my_groups(&h, &third).await.is_empty());

    // Leaving needs approval too (auto-leave is off by default), and a
    // leave request can't be withdrawn.
    let res = call(&h, "POST", &format!("/api/groups/{id}/leave"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(my_groups(&h, &pilot).await, vec!["Capitals"]);
    assert_eq!(
        call(
            &h,
            "POST",
            &format!("/api/groups/{id}/retract"),
            &pilot,
            None
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&h, "POST", &accept, &owner, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(my_groups(&h, &pilot).await.is_empty());

    assert_eq!(
        group_log(&h, &owner, id).await,
        [
            entry("leave", "accept", "Pilot", "Owner"),
            entry("join", "reject", "Third", "Owner"),
            entry("join", "accept", "Pilot", "Owner"),
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn auto_leave_lets_members_go_without_approval(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Capitals", "requestable").await;
    let pilot_account = account_of(&h, &pilot).await;
    call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    call(
        &h,
        "POST",
        &format!("/api/group-management/groups/{id}/requests/{pilot_account}/accept"),
        &owner,
        None,
    )
    .await;

    let saved = send(
        &h.app,
        form("/admin/groups/settings", "auto_leave=on", &owner),
    )
    .await;
    assert_eq!(saved.location(), "/admin/groups");
    let res = call(&h, "POST", &format!("/api/groups/{id}/leave"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(my_groups(&h, &pilot).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn requesting_needs_request_groups_unless_public(db: PgPool) {
    let h = harness(db, true).await;
    // A Guest: no request_groups (AA grants it to Member).
    let (owner, guest) = owner_and_pilot(&h).await;
    let requestable = create_group(&h, &owner, "Capitals", "requestable").await;
    let public = create_group(&h, &owner, "Newbros", "public").await;

    let seen: Vec<Value> = listed(&h, &guest)
        .await
        .into_iter()
        .map(|g| g["name"].clone())
        .collect();
    assert_eq!(seen, ["Newbros"]);
    let res = call(
        &h,
        "POST",
        &format!("/api/groups/{requestable}/join"),
        &guest,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = call(
        &h,
        "POST",
        &format!("/api/groups/{public}/join"),
        &guest,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn internal_groups_are_admin_only(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Officers", "internal").await;
    let pilot_account = account_of(&h, &pilot).await;

    assert!(listed(&h, &pilot).await.is_empty());
    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

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
    // Shown to its members, who can't leave it.
    assert_eq!(listed(&h, &pilot).await[0]["kind"], "internal");
    let leave = call(&h, "POST", &format!("/api/groups/{id}/leave"), &pilot, None).await;
    assert_eq!(leave.status, StatusCode::FORBIDDEN);
    // Outsiders can't tell Internal groups exist, compliance groups
    // included.
    let compliant: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE compliance")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let guest = log_in_as(&h, "90000003:Third", None).await;
    for group in [id, compliant] {
        let res = call(
            &h,
            "POST",
            &format!("/api/groups/{group}/leave"),
            &guest,
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{group}: {}", res.body);
    }
    // Group Management doesn't cover Internal groups.
    let gm = call(
        &h,
        "GET",
        &format!("/api/group-management/groups/{id}/members"),
        &owner,
        None,
    )
    .await;
    assert_eq!(gm.status, StatusCode::NOT_FOUND);

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
async fn hidden_groups_join_through_their_link(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Scouts", "hidden").await;
    let internal = create_group(&h, &owner, "Officers", "internal").await;

    assert!(listed(&h, &pilot).await.is_empty());
    let link = page(&h, &format!("/groups/{id}"), &pilot).await;
    assert_eq!(link.status, StatusCode::OK);
    assert!(link.body.contains("Join Scouts"));
    assert_eq!(
        page(&h, &format!("/groups/{internal}"), &pilot)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(my_groups(&h, &pilot).await, vec!["Scouts"]);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn allowed_states_limit_joining_and_remove_the_rest(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let guest = log_in_as(&h, "90000003:Third", None).await;
    let id = create_group(&h, &owner, "Miners", "open").await;
    let open = r#""internal":false,"hidden":false,"open":true,"public":false,"restricted":false"#;
    let res = settings(
        &h,
        &owner,
        id,
        &format!(r#"{{{open},"states":[{MEMBER_STATE}]}}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);

    // Guests don't see it or get in.
    assert!(listed(&h, &guest).await.is_empty());
    let res = call(&h, "POST", &format!("/api/groups/{id}/join"), &guest, None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(my_groups(&h, &pilot).await, vec!["Miners"]);

    // Saving states that exclude a member removes them.
    let res = settings(
        &h,
        &owner,
        id,
        &format!(r#"{{{open},"states":[{BLUE_STATE}]}}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(my_groups(&h, &pilot).await.is_empty());

    // So does a state change: the pilot rejoins, then stops being Member.
    settings(
        &h,
        &owner,
        id,
        &format!(r#"{{{open},"states":[{MEMBER_STATE}]}}"#),
    )
    .await;
    call(&h, "POST", &format!("/api/groups/{id}/join"), &pilot, None).await;
    assert_eq!(my_groups(&h, &pilot).await, vec!["Miners"]);
    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, PILOT)
        .await
        .unwrap();
    tether_web::states::evaluate_account(
        &h.db,
        tether_db::accounts::AccountId(account_of(&h, &pilot).await),
    )
    .await
    .unwrap();
    assert_eq!(state_of(&h, &pilot).await, "Guest");
    assert!(my_groups(&h, &pilot).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn group_leaders_manage_only_their_groups(db: PgPool) {
    members(&db, &[PILOT, THIRD]).await;
    let h = harness(db, true).await;
    let (owner, leader) = owner_and_pilot(&h).await;
    let third = log_in_as(&h, "90000003:Third", None).await;
    let led = create_group(&h, &owner, "Capitals", "requestable").await;
    let other = create_group(&h, &owner, "Supers", "requestable").await;
    let (leader_account, third_account) =
        (account_of(&h, &leader).await, account_of(&h, &third).await);

    assert_eq!(
        call(
            &h,
            "PUT",
            &format!("/api/admin/groups/{led}/leaders/{leader_account}"),
            &leader,
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &h,
            "PUT",
            &format!("/api/admin/groups/{led}/leaders/{leader_account}"),
            &owner,
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    call(&h, "POST", &format!("/api/groups/{led}/join"), &third, None).await;
    call(
        &h,
        "POST",
        &format!("/api/groups/{other}/join"),
        &third,
        None,
    )
    .await;

    // The leader sees only their group's request, and the nav shows it.
    let requests = json(&call(&h, "GET", "/api/group-management/requests", &leader, None).await);
    assert_eq!(requests.as_array().unwrap().len(), 1);
    assert_eq!(requests[0]["group_name"], "Capitals");
    assert!(
        page(&h, "/dashboard", &leader)
            .await
            .body
            .contains(r#"href="/group-management""#)
    );
    assert!(
        !page(&h, "/dashboard", &third)
            .await
            .body
            .contains(r#"href="/group-management""#)
    );
    let elsewhere = call(
        &h,
        "POST",
        &format!("/api/group-management/groups/{other}/requests/{third_account}/accept"),
        &leader,
        None,
    )
    .await;
    assert_eq!(elsewhere.status, StatusCode::NOT_FOUND);

    // A leader must hold what the group grants to let anyone in.
    let body = format!(r#"{{"permission":"admin.audit","group_id":{led}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&body),
    )
    .await;
    let accept = format!("/api/group-management/groups/{led}/requests/{third_account}/accept");
    let refused = call(&h, "POST", &accept, &leader, None).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.body.contains("admin.audit"));
    assert!(my_groups(&h, &third).await.is_empty());

    // Leader groups: members of Officers lead Supers.
    let officers = create_group(&h, &owner, "Officers", "internal").await;
    call(
        &h,
        "POST",
        &format!("/api/admin/groups/{officers}/members"),
        &owner,
        Some(&format!(r#"{{"account_id":{leader_account}}}"#)),
    )
    .await;
    assert_eq!(
        call(
            &h,
            "PUT",
            &format!("/api/admin/groups/{other}/leader-groups/{officers}"),
            &owner,
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let accepted = call(
        &h,
        "POST",
        &format!("/api/group-management/groups/{other}/requests/{third_account}/accept"),
        &leader,
        None,
    )
    .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT, "{}", accepted.body);
    assert_eq!(my_groups(&h, &third).await, vec!["Supers"]);
    assert_eq!(
        group_log(&h, &leader, other).await,
        [entry("join", "accept", "Third", "Pilot")]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn restricted_groups_are_the_owners_alone(db: PgPool) {
    members(&db, &[PILOT, THIRD]).await;
    let h = harness(db, true).await;
    let (owner, admin) = owner_and_pilot(&h).await;
    let third = log_in_as(&h, "90000003:Third", None).await;
    let third_account = account_of(&h, &third).await;
    // Members manage groups here.
    let grant = format!(r#"{{"permission":"admin.groups","state_id":{MEMBER_STATE}}}"#);
    let res = call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&grant),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);

    let res = call(
        &h,
        "POST",
        "/api/admin/groups",
        &admin,
        Some(r#"{"name":"Council","restricted":true}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"Council","restricted":true}"#),
    )
    .await;
    let council = json(&res)["id"].as_i64().unwrap();
    let body = format!(r#"{{"account_id":{third_account}}}"#);
    let path = format!("/api/admin/groups/{council}/members");
    assert_eq!(
        call(&h, "POST", &path, &admin, Some(&body)).await.status,
        StatusCode::FORBIDDEN
    );
    let unrestrict =
        r#"{"internal":true,"hidden":true,"open":false,"public":false,"restricted":false}"#;
    assert_eq!(
        settings(&h, &admin, council, unrestrict).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&h, "POST", &path, &owner, Some(&body)).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sensitive_groups_cannot_become_open(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let id = create_group(&h, &owner, "Auditors", "requestable").await;
    let body = format!(r#"{{"permission":"admin.audit","group_id":{id}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&body),
    )
    .await;
    let res = settings(
        &h,
        &owner,
        id,
        r#"{"internal":false,"hidden":false,"open":true,"public":false,"restricted":false}"#,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("admin.audit"));
    // And a compliance group must be Internal.
    let res = settings(
        &h,
        &owner,
        id,
        r#"{"internal":false,"hidden":false,"open":false,"public":false,"restricted":false,"compliance":true}"#,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permissions_granted_to_groups_and_states_take_effect(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let officers = create_group(&h, &owner, "Officers", "internal").await;
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

    // Admin permissions can't go to Guest: anyone who logs in with EVE is.
    let body = r#"{"permission":"admin.groups","state_id":3}"#;
    let refused = call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(body),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(
        refused
            .body
            .contains("anyone who logs in with EVE is Guest")
    );
    assert_eq!(me(&h, &pilot).await["permissions"], serde_json::json!([]));

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
        grant(r#"{"permission":"admin.everything","state_id":1}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit"}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit","state_id":1,"group_id":1}"#).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        grant(r#"{"permission":"admin.audit","state_id":999}"#).await,
        StatusCode::NOT_FOUND
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
        Some(r#"{"permission":"admin.audit","state_id":3}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // Only the default grant: request_groups for Member.
    let listed = call(&h, "GET", "/api/admin/permissions", &owner, None).await;
    let grants = json(&listed)["grants"].clone();
    assert_eq!(grants.as_array().unwrap().len(), 1, "{grants}");
    assert_eq!(grants[0]["permission"], "request_groups");
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

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn restricted_groups_stay_the_owners_even_when_open(db: PgPool) {
    members(&db, &[PILOT, THIRD]).await;
    let h = harness(db, true).await;
    let (owner, admin) = owner_and_pilot(&h).await;
    let third = log_in_as(&h, "90000003:Third", None).await;
    let grant = format!(r#"{{"permission":"admin.groups","state_id":{MEMBER_STATE}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&grant),
    )
    .await;
    let res = call(
        &h,
        "POST",
        "/api/admin/groups",
        &owner,
        Some(r#"{"name":"Council","restricted":true}"#),
    )
    .await;
    let council = json(&res)["id"].as_i64().unwrap();

    // An admin can't open it while leaving it Restricted either.
    let open = r#"{"internal":false,"hidden":false,"open":true,"public":true,"restricted":true}"#;
    assert_eq!(
        settings(&h, &admin, council, open).await.status,
        StatusCode::FORBIDDEN
    );
    // Nor appoint its leaders.
    let admin_account = account_of(&h, &admin).await;
    assert_eq!(
        call(
            &h,
            "PUT",
            &format!("/api/admin/groups/{council}/leaders/{admin_account}"),
            &admin,
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // Opened by the owner, joining it is still a request.
    assert_eq!(
        settings(&h, &owner, council, open).await.status,
        StatusCode::NO_CONTENT
    );
    let res = call(
        &h,
        "POST",
        &format!("/api/groups/{council}/join"),
        &third,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert!(my_groups(&h, &third).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn opening_a_group_needs_its_permissions(db: PgPool) {
    cover(&db, Builtin::Blue, EntityKind::Character, PILOT).await;
    let h = harness(db, true).await;
    let (owner, admin) = owner_and_pilot(&h).await;
    // A Blue admin: manages groups, but can't request them.
    let grant = format!(r#"{{"permission":"admin.groups","state_id":{BLUE_STATE}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&grant),
    )
    .await;
    let id = create_group(&h, &owner, "Requesters", "requestable").await;
    let grant = format!(r#"{{"permission":"request_groups","group_id":{id}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&grant),
    )
    .await;

    let res = settings(
        &h,
        &admin,
        id,
        r#"{"internal":false,"hidden":false,"open":true,"public":true,"restricted":false}"#,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("request_groups"), "{}", res.body);

    // Nor can they widen who may walk into one that's already Open.
    let open = r#""internal":false,"hidden":false,"open":true,"public":true,"restricted":false"#;
    let res = settings(
        &h,
        &owner,
        id,
        &format!(r#"{{{open},"states":[{MEMBER_STATE}]}}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let res = settings(
        &h,
        &admin,
        id,
        &format!(r#"{{{open},"states":[{MEMBER_STATE},{BLUE_STATE}]}}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    // Or put themselves in a leader group whose led group grants it.
    let officers = create_group(&h, &owner, "Officers", "internal").await;
    let led = create_group(&h, &owner, "Capitals", "requestable").await;
    let grant = format!(r#"{{"permission":"request_groups","group_id":{led}}}"#);
    call(
        &h,
        "POST",
        "/api/admin/permissions/grants",
        &owner,
        Some(&grant),
    )
    .await;
    call(
        &h,
        "PUT",
        &format!("/api/admin/groups/{led}/leader-groups/{officers}"),
        &owner,
        None,
    )
    .await;
    let me_body = format!(r#"{{"account_id":{}}}"#, account_of(&h, &admin).await);
    let res = call(
        &h,
        "POST",
        &format!("/api/admin/groups/{officers}/members"),
        &admin,
        Some(&me_body),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("request_groups"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn leader_groups_are_never_open_or_compliance(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let led = create_group(&h, &owner, "Capitals", "requestable").await;
    let open = create_group(&h, &owner, "Anyone", "open").await;
    let officers = create_group(&h, &owner, "Officers", "internal").await;
    let add = |leader: i64| {
        let (h, owner) = (&h, owner.clone());
        async move {
            call(
                h,
                "PUT",
                &format!("/api/admin/groups/{led}/leader-groups/{leader}"),
                &owner,
                None,
            )
            .await
            .status
        }
    };
    assert_eq!(add(open).await, StatusCode::BAD_REQUEST);
    assert_eq!(add(officers).await, StatusCode::NO_CONTENT);
    // A group that leads others can't be opened or made a compliance group.
    let res = settings(
        &h,
        &owner,
        officers,
        r#"{"internal":false,"hidden":false,"open":true,"public":false,"restricted":false}"#,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = settings(
        &h,
        &owner,
        officers,
        r#"{"internal":true,"hidden":true,"open":false,"public":false,"restricted":false,"compliance":true}"#,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn leaders_who_become_guest_stop_leading(db: PgPool) {
    members(&db, &[PILOT]).await;
    let h = harness(db, true).await;
    let (owner, leader) = owner_and_pilot(&h).await;
    let led = create_group(&h, &owner, "Capitals", "requestable").await;
    let leader_account = account_of(&h, &leader).await;
    call(
        &h,
        "PUT",
        &format!("/api/admin/groups/{led}/leaders/{leader_account}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(
        page(&h, "/group-management", &leader).await.status,
        StatusCode::OK
    );

    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, PILOT)
        .await
        .unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(leader_account))
        .await
        .unwrap();
    assert_eq!(
        page(&h, "/group-management", &leader).await.status,
        StatusCode::FORBIDDEN
    );
    let members = call(
        &h,
        "GET",
        &format!("/api/group-management/groups/{led}/members"),
        &leader,
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::NOT_FOUND);
}
