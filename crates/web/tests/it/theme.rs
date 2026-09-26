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
    let system = page(&h, "/admin/system", &owner).await.body;
    assert!(
        system.contains(r#"href="/theme.css""#),
        "every page links it"
    );
    assert!(system.contains("Violet"), "{system}");

    let denied = send(
        &h.app,
        form("/admin/system/theme", "accent=%23a78bfa", &pilot),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let res = send(
        &h.app,
        form("/admin/system/theme", "accent=%23a78bfa", &owner),
    )
    .await;
    assert_eq!(res.location(), "/admin/system", "{}", res.body);
    let css = send(&h.app, get("/theme.css", &[])).await;
    assert!(css.body.contains("--accent:#a78bfa"), "{}", css.body);
    assert_ne!(css.headers[header::ETAG].to_str().unwrap(), etag);

    // A custom colour, checked: too dark to read is refused.
    let dark = send(
        &h.app,
        form(
            "/admin/system/theme",
            "accent=custom&custom_accent=%231e3a8a",
            &owner,
        ),
    )
    .await;
    assert_eq!(dark.status, StatusCode::BAD_REQUEST);
    assert!(dark.body.contains("too dark"), "{}", dark.body);
    let custom = send(
        &h.app,
        form(
            "/admin/system/theme",
            "accent=custom&custom_accent=%2322D3EE",
            &owner,
        ),
    )
    .await;
    assert_eq!(custom.location(), "/admin/system");
    let css = send(&h.app, get("/theme.css", &[])).await;
    assert!(css.body.contains("--accent:#22d3ee"), "{}", css.body);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'theme.accent'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 2);
}
