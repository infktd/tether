//! The accent colour (DESIGN.md): amber unless an admin picks another,
//! served as /theme.css after the built stylesheet.

use axum::http::{StatusCode, header};
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_pick_the_accent(db: PgPool) {
    let h = harness(db, true).await;
    // Public: the sign-in page uses it too.
    let css = send(&h.app, get("/theme.css", &[])).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.body.contains("--accent:#f59e0b"), "{}", css.body);
    let etag = css.headers[header::ETAG].to_str().unwrap().to_owned();
    let again = send(
        &h.app,
        axum::http::Request::get("/theme.css")
            .header(header::IF_NONE_MATCH, &etag)
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);

    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let system = page(&h, "/admin/settings", &owner).await.body;
    assert!(
        system.contains(r#"href="/theme.css""#),
        "every page links it"
    );
    assert!(system.contains("Violet"), "{system}");

    let denied = send(&h.app, form("/admin/settings", "accent=%23a78bfa", &pilot)).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let res = save_instance_settings(&h, &owner, &[("accent", "#a78bfa")]).await;
    assert_eq!(res.location(), "/admin/settings", "{}", res.body);
    let css = send(&h.app, get("/theme.css", &[])).await;
    assert!(css.body.contains("--accent:#a78bfa"), "{}", css.body);
    assert_ne!(css.headers[header::ETAG].to_str().unwrap(), etag);

    // A custom colour, checked: too dark to read is refused.
    let dark = save_instance_settings(
        &h,
        &owner,
        &[("accent", "custom"), ("custom_accent", "#1e3a8a")],
    )
    .await;
    assert_eq!(dark.status, StatusCode::BAD_REQUEST);
    assert!(dark.body.contains("too dark"), "{}", dark.body);
    // With script, the page loads again for the new colour.
    let shown = page(&h, "/admin/settings", &owner).await.body;
    let body = form_body(
        &shown,
        "instance-settings",
        &[("accent", "custom"), ("custom_accent", "#22D3EE")],
    );
    let custom = send(
        &h.app,
        boosted(form("/admin/settings", &body, &owner), "/admin/settings"),
    )
    .await;
    assert_eq!(custom.status, StatusCode::NO_CONTENT, "{}", custom.body);
    assert_eq!(custom.headers["hx-refresh"], "true");
    let css = send(&h.app, get("/theme.css", &[])).await;
    assert!(css.body.contains("--accent:#22d3ee"), "{}", css.body);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'theme.accent'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 2);
}
