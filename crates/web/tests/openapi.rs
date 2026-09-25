#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use std::collections::BTreeSet;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use utoipa::OpenApi;

/// Every JSON route the router serves. Adding a route without documenting
/// it (or listing it here) fails this test.
const API_ROUTES: &[(&str, &str)] = &[
    ("get", "/health"),
    ("get", "/ready"),
    ("get", "/api/me"),
    ("post", "/api/me/main"),
    ("get", "/api/groups"),
    ("post", "/api/groups/{id}/join"),
    ("post", "/api/groups/{id}/leave"),
    ("post", "/api/admin/groups"),
    ("delete", "/api/admin/groups/{id}"),
    ("post", "/api/admin/groups/{id}/members"),
    ("delete", "/api/admin/groups/{id}/members/{account_id}"),
    ("get", "/api/admin/groups/{id}/requests"),
    (
        "post",
        "/api/admin/groups/{id}/requests/{account_id}/approve",
    ),
    ("post", "/api/admin/groups/{id}/requests/{account_id}/deny"),
    ("get", "/api/admin/permissions"),
    ("post", "/api/admin/permissions/grants"),
    ("delete", "/api/admin/permissions/grants/{id}"),
    ("get", "/api/admin/audit"),
    ("post", "/api/admin/accounts/{id}/deactivate"),
    ("post", "/api/admin/accounts/{id}/reactivate"),
    ("get", "/api/admin/states"),
    ("post", "/api/admin/states"),
    ("patch", "/api/admin/states/{id}"),
    ("delete", "/api/admin/states/{id}"),
    ("post", "/api/admin/states/{id}/move"),
    ("post", "/api/admin/states/{id}/covers"),
    ("delete", "/api/admin/states/{id}/covers/{entity_id}"),
    ("post", "/api/admin/states/{id}/scopes"),
    ("delete", "/api/admin/states/{id}/scopes/{scope}"),
    ("post", "/api/admin/states/resolve"),
    ("get", "/api/setup"),
    ("post", "/api/setup/unlock"),
    ("post", "/api/setup/sso"),
    ("get", "/api/setup/probe"),
    ("post", "/api/setup/callback-check"),
];

fn documented() -> BTreeSet<(String, String)> {
    let spec = serde_json::to_value(tether_web::openapi::ApiDoc::openapi()).unwrap();
    let mut out = BTreeSet::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            out.insert((method.clone(), path.clone()));
        }
    }
    out
}

#[test]
fn every_route_is_documented_and_nothing_else() {
    let expected: BTreeSet<(String, String)> = API_ROUTES
        .iter()
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect();
    assert_eq!(documented(), expected);
}

#[test]
fn spec_snapshot() {
    insta::assert_json_snapshot!("openapi", tether_web::openapi::ApiDoc::openapi());
}

/// The documented routes really exist: none of them 404 or 405.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn documented_routes_are_served(db: PgPool) {
    let h = harness(db, false).await;
    for (method, path) in API_ROUTES {
        let uri = path
            .replace("{id}", "1")
            .replace("{account_id}", "1")
            .replace("{entity_id}", "1");
        let req = axum::http::Request::builder()
            .method(method.to_uppercase().as_str())
            .uri(&uri)
            .header("origin", SITE)
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap();
        let res = send(&h.app, req).await;
        // An unmatched path gets the HTML fallback page, and a wrong method
        // an empty 405; our handlers' own 404s carry a specific message.
        let unrouted = res.body.contains("nothing at this address")
            || (res.status == StatusCode::METHOD_NOT_ALLOWED && res.body.is_empty());
        assert!(!unrouted, "{method} {uri} is not routed ({})", res.status);
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn spec_is_served_as_json(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, get("/api/openapi.json", &[])).await;
    assert_eq!(res.status, StatusCode::OK);
    let spec: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(spec["info"]["title"], "Tether API");
    assert!(spec["components"]["securitySchemes"]["session"].is_object());
}

#[cfg(not(feature = "dev-docs"))]
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn docs_page_does_not_exist_without_dev_docs(db: PgPool) {
    let h = harness(db, false).await;
    assert_eq!(
        send(&h.app, get("/docs", &[])).await.status,
        StatusCode::NOT_FOUND
    );
}

#[cfg(feature = "dev-docs")]
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn docs_page_loads_pinned_scalar(db: PgPool) {
    let h = harness(db, false).await;
    let res = send(&h.app, get("/docs", &[])).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("@scalar/api-reference@1.71.0"));
    assert!(res.body.contains(r#"integrity="sha256-"#));
    assert!(res.body.contains(r#"data-url="/api/openapi.json""#));
}
