//! What's new: the popup after an update, once, with what concerns each
//! pilot, and the page with every update.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;

fn popup_opened(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/whats-new/seen")
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header(header::ORIGIN, SITE)
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap()
}

async fn forget_seen(h: &Harness) {
    sqlx::query("UPDATE core.accounts SET whats_new_seen = 0")
        .execute(&h.db)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_popup_comes_once_with_what_concerns_each(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner").await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    // New pilots start up to date: no history.
    for token in [&owner, &pilot] {
        let home = page(&h, "/dashboard", token).await;
        assert_eq!(home.status, StatusCode::OK, "{}", home.body);
        assert!(!home.body.contains(r#"id="whats-new""#), "{}", home.body);
    }
    // An update they haven't seen: the popup, everyone's notes for all,
    // Administration's only for admins.
    forget_seen(&h).await;
    let theirs = page(&h, "/dashboard", &pilot).await.body;
    assert!(theirs.contains(r#"id="whats-new""#), "{theirs}");
    assert!(theirs.contains("popup like this one now opens"), "{theirs}");
    assert!(!theirs.contains("<h3>Administration</h3>"), "{theirs}");
    let admins = page(&h, "/dashboard", &owner).await.body;
    assert!(admins.contains("<h3>Administration</h3>"), "{admins}");
    // Opening it marks it read (live.js says so): it doesn't come back.
    let opened = send(&h.app, popup_opened(&pilot)).await;
    assert_eq!(opened.status, StatusCode::NO_CONTENT, "{}", opened.body);
    let again = page(&h, "/dashboard", &pilot).await.body;
    assert!(!again.contains(r#"id="whats-new""#), "{again}");
    // The owner hasn't opened theirs yet.
    assert!(
        page(&h, "/notifications", &owner)
            .await
            .body
            .contains(r#"id="whats-new""#)
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_update_is_on_its_page(db: PgPool) {
    let h = harness(db, true).await;
    let pilot = log_in_as(&h, "90000002:Pilot", None).await;
    forget_seen(&h).await;
    let res = page(&h, "/whats-new", &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("2026-10-07"), "{}", res.body);
    // The page says it all, so no popup over it, and reading it counts.
    assert!(!res.body.contains(r#"id="whats-new""#), "{}", res.body);
    let home = page(&h, "/dashboard", &pilot).await.body;
    assert!(!home.contains(r#"id="whats-new""#), "{home}");
    assert!(home.contains(r#"href="/whats-new""#), "{home}");
    // Signed out: no page.
    let anon = send(&h.app, get("/whats-new", &[])).await;
    assert_ne!(anon.status, StatusCode::OK);
}
