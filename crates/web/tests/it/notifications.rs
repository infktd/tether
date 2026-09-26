use std::pin::Pin;
use std::time::Duration;

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures_core::Stream;
use serde_json::Value;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_db::accounts::AccountId;
use tether_db::notifications::{Level, notify};
use tower::ServiceExt;

const CHRIBBA: &str = "196379789:Chribba"; // alliance 159826257
const PILOT: i64 = 90000002;

fn api(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE)
        .body(Body::empty())
        .unwrap()
}

async fn list(h: &Harness, token: &str) -> Vec<Value> {
    let res = send(&h.app, api("GET", "/api/notifications", token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    serde_json::from_str::<Value>(&res.body)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

async fn titles(h: &Harness, token: &str) -> Vec<String> {
    list(h, token)
        .await
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_owned())
        .collect()
}

async fn account_of(h: &Harness, token: &str) -> AccountId {
    AccountId(me(h, token).await["account_id"].as_i64().unwrap())
}

async fn send_one(h: &Harness, account: AccountId, title: &str) {
    let mut conn = h.db.acquire().await.unwrap();
    notify(&mut conn, account, Level::Info, title, Some("Body"))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_the_recipient_sees_opens_and_deletes(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    let pilot_account = account_of(&h, &pilot).await;
    send_one(&h, pilot_account, "Hello").await;
    let id = list(&h, &pilot).await[0]["id"].as_i64().unwrap();

    assert!(titles(&h, &owner).await.iter().all(|t| t != "Hello"));
    let theirs = send(
        &h.app,
        api("POST", &format!("/api/notifications/{id}/open"), &owner),
    )
    .await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(
        page(&h, &format!("/notifications/{id}"), &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // The page and the bell show it unread; opening it marks it read.
    let listed = page(&h, "/notifications", &pilot).await;
    assert!(listed.body.contains("Hello"));
    assert!(listed.body.contains("1 unread"), "{}", listed.body);
    let opened = page(&h, &format!("/notifications/{id}"), &pilot).await;
    assert_eq!(opened.status, StatusCode::OK);
    assert!(opened.body.contains("Body"));
    assert!(!opened.body.contains("1 unread"));
    let unread = send(&h.app, api("GET", "/api/notifications/unread", &pilot)).await;
    assert_eq!(unread.body, r#"{"unread":0}"#);

    // Mark all read, delete all read, delete one.
    send_one(&h, pilot_account, "Second").await;
    let res = send(&h.app, form("/notifications/read-all", "", &pilot)).await;
    assert_eq!(res.location(), "/notifications");
    assert!(list(&h, &pilot).await.iter().all(|n| n["read"] == true));
    send_one(&h, pilot_account, "Third").await;
    let res = send(&h.app, form("/notifications/delete-read", "", &pilot)).await;
    assert_eq!(res.location(), "/notifications");
    assert_eq!(titles(&h, &pilot).await, ["Third"]);
    let third = list(&h, &pilot).await[0]["id"].as_i64().unwrap();
    assert_eq!(
        send(
            &h.app,
            api("DELETE", &format!("/api/notifications/{third}"), &owner)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &h.app,
            api("DELETE", &format!("/api/notifications/{third}"), &pilot)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert!(list(&h, &pilot).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn state_changes_are_notified_in_aas_words(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let _owner = log_in_owner(&h, "90000001:Owner").await;
    let member = log_in_as(&h, CHRIBBA, None).await;
    let n = list(&h, &member).await;
    let state = n
        .iter()
        .find(|n| n["title"] == "State changed to: Member")
        .unwrap_or_else(|| panic!("{n:?}"));
    assert_eq!(state["message"], "Your user's state is now: Member");
    assert_eq!(state["level"], "info");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn group_decisions_and_opt_in_requests_are_notified(db: PgPool) {
    // Leaders count only while they aren't Guest.
    cover(&db, Builtin::Member, EntityKind::Character, 90000001).await;
    cover(&db, Builtin::Member, EntityKind::Character, PILOT).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let owner_account = account_of(&h, &owner).await;
    let res = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Capitals","internal":false,"hidden":false}"#,
        ),
    )
    .await;
    let id = serde_json::from_str::<Value>(&res.body).unwrap()["id"]
        .as_i64()
        .unwrap();
    send(
        &h.app,
        api(
            "PUT",
            &format!("/api/admin/groups/{id}/leaders/{}", owner_account.0),
            &owner,
        ),
    )
    .await;

    // Off by default: leaders hear nothing of requests.
    send(
        &h.app,
        api("POST", &format!("/api/groups/{id}/join"), &pilot),
    )
    .await;
    assert!(
        !titles(&h, &owner)
            .await
            .iter()
            .any(|t| t.starts_with("Group Management"))
    );
    send(
        &h.app,
        api(
            "POST",
            &format!(
                "/api/group-management/groups/{id}/requests/{}/accept",
                pilot_account.0
            ),
            &owner,
        ),
    )
    .await;
    let accepted = list(&h, &pilot).await;
    assert_eq!(accepted[0]["title"], "Group Application Accepted");
    assert_eq!(
        accepted[0]["message"],
        "Your application to Capitals has been accepted."
    );
    assert_eq!(accepted[0]["level"], "success");

    // With the setting on, a leave request reaches the leader.
    send(
        &h.app,
        form("/admin/groups/settings", "notify_requests=on", &owner),
    )
    .await;
    send(
        &h.app,
        api("POST", &format!("/api/groups/{id}/leave"), &pilot),
    )
    .await;
    let owner_n = list(&h, &owner).await;
    assert_eq!(
        owner_n[0]["title"],
        "Group Management: Leave request for Capitals"
    );
    assert_eq!(owner_n[0]["message"], "Pilot wants to leave Capitals.");
    send(
        &h.app,
        api(
            "POST",
            &format!(
                "/api/group-management/groups/{id}/requests/{}/reject",
                pilot_account.0
            ),
            &owner,
        ),
    )
    .await;
    let rejected = &list(&h, &pilot).await[0];
    assert_eq!(rejected["title"], "Group Leave Request Rejected");
    assert_eq!(rejected["level"], "danger");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn lost_characters_are_notified(db: PgPool) {
    let h = harness(db, true).await;
    let _owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    let lost = tether_db::accounts::Lost {
        character_id: PILOT,
        character_name: "Pilot".into(),
        from: account_of(&h, &pilot).await,
        was_main: true,
        owner_lost: false,
        reason: "sold",
    };
    tether_web::notifications::character_lost(&h.db, &lost)
        .await
        .unwrap();
    let n = &list(&h, &pilot).await[0];
    assert_eq!(n["title"], "Character Pilot lost");
    assert_eq!(n["level"], "warning");
    let message = n["message"].as_str().unwrap();
    assert!(message.contains("another EVE account"), "{message}");
    assert!(message.contains("Change Main"), "{message}");
}

/// The next server-sent event's text, or `None` after `wait`.
async fn next_event(
    body: &mut Pin<Box<axum::body::BodyDataStream>>,
    wait: Duration,
) -> Option<String> {
    loop {
        let frame =
            tokio::time::timeout(wait, std::future::poll_fn(|cx| body.as_mut().poll_next(cx)))
                .await
                .ok()??
                .unwrap();
        let text = String::from_utf8_lossy(&frame).into_owned();
        // Skip keep-alive comments.
        if text.contains("event: unread") {
            return Some(text);
        }
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_unread_count_is_live(db: PgPool) {
    let h = harness(db, true).await;
    let _owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    let account = account_of(&h, &pilot).await;
    tether_db::notifications::mark_all_read(&h.db, account)
        .await
        .unwrap();

    // Signed out: refused.
    let res = send(&h.app, get("/notifications/stream", &[])).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let res = h
        .app
        .clone()
        .oneshot(get("/notifications/stream", &[(SESSION, &pilot)]))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    // Unbuffered through nginx.
    assert_eq!(res.headers()["x-accel-buffering"], "no");
    let mut body = Box::pin(res.into_body().into_data_stream());
    let first = next_event(&mut body, Duration::from_secs(5)).await.unwrap();
    assert!(first.contains(r#"id="notification-bell""#), "{first}");
    assert!(!first.contains("unread\""), "{first}");

    // A notification from anywhere (here, straight into the database)
    // arrives through Postgres.
    send_one(&h, account, "Live").await;
    let second = next_event(&mut body, Duration::from_secs(10))
        .await
        .expect("no event after a new notification");
    assert!(second.contains("1 unread"), "{second}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn streams_are_capped_and_end_with_the_session(db: PgPool) {
    let h = harness(db, true).await;
    let _owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    let account = account_of(&h, &pilot).await;

    let mut open = Vec::new();
    for _ in 0..tether_web::notifications::MAX_STREAMS {
        let res = h
            .app
            .clone()
            .oneshot(get("/notifications/stream", &[(SESSION, &pilot)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let mut body = Box::pin(res.into_body().into_data_stream());
        next_event(&mut body, Duration::from_secs(5)).await.unwrap();
        open.push(body);
    }
    // A ninth ends the oldest.
    let res = h
        .app
        .clone()
        .oneshot(get("/notifications/stream", &[(SESSION, &pilot)]))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut newest = Box::pin(res.into_body().into_data_stream());
    next_event(&mut newest, Duration::from_secs(5))
        .await
        .unwrap();
    send_one(&h, account, "Ninth").await;
    assert_eq!(
        next_event(&mut open[0], Duration::from_secs(10)).await,
        None
    );
    assert!(
        next_event(&mut open[1], Duration::from_secs(10))
            .await
            .is_some()
    );

    // Logging out ends the rest at their next change.
    let res = send(&h.app, form("/auth/logout", "", &pilot)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    send_one(&h, account, "After logout").await;
    assert_eq!(next_event(&mut newest, Duration::from_secs(10)).await, None);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_repeated_request_notice_waits_unread_once(db: PgPool) {
    let h = harness(db, true).await;
    let pilot = log_in_owner(&h, "90000001:Owner").await;
    let account = account_of(&h, &pilot).await;
    let mut conn = h.db.acquire().await.unwrap();
    for _ in 0..3 {
        tether_db::notifications::notify_once(&mut conn, account, Level::Info, "Asked", None)
            .await
            .unwrap();
    }
    assert_eq!(
        titles(&h, &pilot)
            .await
            .iter()
            .filter(|t| *t == "Asked")
            .count(),
        1
    );
}
