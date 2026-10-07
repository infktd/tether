//! `/metrics`: off by default (a 404 like any address that doesn't
//! exist), and when on, only for the bearer token from the environment.
//! Counts only: no names, nothing about anyone.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;
use tether_core::Secret;
use tether_web::metrics::Metrics;
use utoipa::OpenApi;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

fn scrape(token: Option<&str>) -> Request<Body> {
    let mut req = Request::get("/metrics");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    req.body(Body::empty()).unwrap()
}

fn token() -> String {
    "0123456789abcdef".repeat(4)
}

/// The harness's app with metrics on.
fn with_metrics(h: &Harness) -> axum::Router {
    tether_web::router(tether_web::AppState {
        metrics: Arc::new(Metrics::on(&Secret::new(token())).unwrap()),
        ..h.state.clone()
    })
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn off_by_default_it_is_a_404_whatever_is_sent(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let missing = send(&h.app, get("/nothing-here", &[])).await;
    for req in [
        scrape(None),
        scrape(Some(&token())),
        get("/metrics", &[(SESSION, &owner)]),
    ] {
        let res = send(&h.app, req).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
        // Just as an address that doesn't exist.
        assert_eq!(res.body, missing.body);
        assert!(
            res.headers["x-robots-tag"]
                .to_str()
                .unwrap()
                .contains("noindex")
        );
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn on_it_answers_only_the_token(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    log_in_as(&h, GIGX, None).await;
    let app = with_metrics(&h);
    let missing = send(&app, get("/nothing-here", &[])).await;

    let wrong = format!("{}0", &token()[..63]);
    for req in [
        scrape(None),
        scrape(Some(&wrong)),
        scrape(Some(&token()[..32])),
        // A signed-in admin's browser is no scraper.
        get("/metrics", &[(SESSION, &owner)]),
        Request::get("/metrics")
            .header(header::AUTHORIZATION, format!("Basic {}", token()))
            .body(Body::empty())
            .unwrap(),
    ] {
        let res = send(&app, req).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND);
        assert_eq!(res.body, missing.body);
    }

    let res = send(&app, scrape(Some(&token()))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.headers[header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    assert_eq!(res.headers[header::CACHE_CONTROL], "no-store");
    assert!(
        res.headers["x-robots-tag"]
            .to_str()
            .unwrap()
            .contains("noindex")
    );
    let text = res.body;
    for family in [
        "tether_build_info",
        "tether_jobs",
        "tether_apps",
        "tether_esi_rate_limit_groups_held",
        "tether_esi_throttled",
        "tether_esi_responses_total",
        "tether_discord_configured",
        "tether_discord_sync_failing",
        "tether_accounts",
        "tether_accounts_deactivated",
        "tether_db_pool_connections",
        "tether_db_pool_max_connections",
    ] {
        assert!(
            text.contains(&format!("# TYPE {family} ")),
            "{family}: {text}"
        );
    }
    assert!(text.contains(&format!(
        "tether_build_info{{version=\"{}\",revision=\"\"}} 1",
        tether_web::updates::CURRENT
    )));
    assert!(text.contains("tether_jobs{state=\"dead\"} "), "{text}");
    assert!(text.contains("tether_apps{status=\"running\"} 0"), "{text}");
    assert!(text.contains("tether_discord_configured 0"), "{text}");
    assert!(text.contains("tether_accounts_deactivated 0"), "{text}");
    // Two accounts, counted by state id and kind; nobody named.
    let accounts: i64 = text
        .lines()
        .filter(|l| l.starts_with("tether_accounts{"))
        .map(|l| l.rsplit(' ').next().unwrap().parse::<i64>().unwrap())
        .sum();
    assert_eq!(accounts, 2, "{text}");
    for name in ["Chribba", "gigX", "196379789", "Member", "Guest"] {
        assert!(!text.contains(name), "{name} in {text}");
    }
    // Every line is a comment or a sample with a number.
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let value = line.rsplit(' ').next().unwrap();
        assert!(value.parse::<f64>().is_ok(), "{line}");
    }
}

#[test]
fn it_is_not_in_the_api_docs() {
    let spec = serde_json::to_value(tether_web::openapi::ApiDoc::openapi()).unwrap();
    assert!(spec["paths"].get("/metrics").is_none());
    assert!(!spec.to_string().contains("metrics"));
}
