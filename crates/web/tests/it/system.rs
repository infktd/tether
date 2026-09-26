//! The admin dashboard: system health, dead jobs, update checks and the
//! audit log.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;
use tether_web::updates::{self, UpdateSource};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn form(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn page(h: &Harness, uri: &str, token: &str) -> Res {
    send(&h.app, get(uri, &[(SESSION, token)])).await
}

async fn owner_and_pilot(h: &Harness) -> (String, String) {
    let owner = log_in_owner(h, "196379789:Chribba").await;
    let pilot = log_in_as(h, "443630591:The Mittani", None).await;
    (owner, pilot)
}

async fn mount_status(h: &Harness) {
    let status = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/esi/status.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(status, "application/json"))
        .mount(&h.esi_server)
        .await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_dashboard_and_audit_log_need_their_permissions(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    mount_status(&h).await;
    for uri in ["/admin/system", "/admin/audit"] {
        assert_eq!(send(&h.app, get(uri, &[])).await.location(), "/login");
        assert_eq!(
            page(&h, uri, &pilot).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(page(&h, uri, &owner).await.status, StatusCode::OK, "{uri}");
    }
    for uri in [
        "/admin/system/updates",
        "/admin/system/updates/check",
        "/admin/jobs/1/retry",
    ] {
        assert_eq!(
            send(&h.app, form(uri, "", &pilot)).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    // The dashboard is where /admin starts.
    assert_eq!(page(&h, "/admin", &owner).await.location(), "/admin/system");
    let dashboard = page(&h, "/admin/system", &owner).await.body;
    assert!(dashboard.contains("Answering"), "{dashboard}");
    assert!(dashboard.contains(r#"href="/admin/audit""#), "sidebar link");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_dashboard_says_when_esi_is_down(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(503).set_body_raw(r#"{"error":"downtime"}"#, "application/json"),
        )
        .mount(&h.esi_server)
        .await;
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains("Down"), "{body}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn dead_jobs_are_listed_and_retried(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    mount_status(&h).await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO core.jobs (kind, state, attempts, last_error) VALUES ('discord.ping', 'dead', 5, 'Discord is unavailable') RETURNING id",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains("Discord is unavailable"));

    let res = send(&h.app, form(&format!("/admin/jobs/{id}/retry"), "", &owner)).await;
    assert_eq!(res.location(), "/admin/system");
    let state: String = sqlx::query_scalar("SELECT state FROM core.jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'job.retry'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // Not dead any more.
    let again = send(&h.app, form(&format!("/admin/jobs/{id}/retry"), "", &owner)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
}

fn source(server: &MockServer) -> UpdateSource {
    UpdateSource {
        api_base: server.uri(),
        repo: "infktd/tether".to_owned(),
    }
}

async fn release(server: &MockServer, status: u16, body: serde_json::Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/repos/infktd/tether/releases/latest"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn update_checks_report_newer_releases_and_distrust_github(db: PgPool) {
    let h = harness(db, true).await;
    let github = MockServer::start().await;
    let http = github_client(&github);

    // Nothing is sent before an owner exists to switch it off.
    release(
        &github,
        200,
        serde_json::json!({ "tag_name": "v99.0.0", "html_url": "https://github.com/infktd/tether/releases/tag/v99.0.0" }),
    )
    .await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    assert!(github.received_requests().await.unwrap().is_empty());
    log_in_owner(&h, "196379789:Chribba").await;

    release(
        &github,
        200,
        serde_json::json!({ "tag_name": "v99.0.0", "html_url": "https://github.com/infktd/tether/releases/tag/v99.0.0" }),
    )
    .await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    let status = updates::status(&h.db).await.unwrap();
    assert!(status.newer);
    assert_eq!(status.latest.as_deref(), Some("v99.0.0"));
    assert_eq!(
        status.url.as_deref(),
        Some("https://github.com/infktd/tether/releases/tag/v99.0.0")
    );

    // A link somewhere else is dropped; the version still shows.
    release(
        &github,
        200,
        serde_json::json!({ "tag_name": "v99.0.1", "html_url": "https://evil.example/releases/" }),
    )
    .await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    let status = updates::status(&h.db).await.unwrap();
    assert_eq!(status.latest.as_deref(), Some("v99.0.1"));
    assert_eq!(status.url, None);

    // A tag that isn't a plain version isn't shown at all.
    release(
        &github,
        200,
        serde_json::json!({ "tag_name": "<script>alert(1)</script>", "html_url": "https://github.com/infktd/tether/releases/x" }),
    )
    .await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    let status = updates::status(&h.db).await.unwrap();
    assert_eq!(status.latest, None);
    assert!(status.error.is_some());

    // Up to date, and nothing released yet.
    release(
        &github,
        200,
        serde_json::json!({ "tag_name": format!("v{}", updates::CURRENT), "html_url": "https://github.com/infktd/tether/releases/tag/x" }),
    )
    .await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    assert!(!updates::status(&h.db).await.unwrap().newer);
    release(&github, 404, serde_json::json!({ "message": "Not Found" })).await;
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    assert!(updates::status(&h.db).await.unwrap().no_releases);

    // GitHub down: retried, and the dashboard says so.
    release(&github, 502, serde_json::json!({})).await;
    assert!(
        updates::check(&h.db, &http, &source(&github))
            .await
            .is_err()
    );
    assert!(
        updates::status(&h.db)
            .await
            .unwrap()
            .error
            .unwrap()
            .contains("502")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn update_checks_can_be_switched_off(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    mount_status(&h).await;
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&github)
        .await;

    // The form without the checkbox ticked.
    let off = send(&h.app, form("/admin/system/updates", "", &owner)).await;
    assert_eq!(off.location(), "/admin/system");
    assert!(!updates::status(&h.db).await.unwrap().enabled);
    let http = github_client(&github);
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    assert!(
        page(&h, "/admin/system", &owner)
            .await
            .body
            .contains("Update checks are off.")
    );
    let refused = send(&h.app, form("/admin/system/updates/check", "", &owner)).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);

    let on = send(&h.app, form("/admin/system/updates", "enabled=on", &owner)).await;
    assert_eq!(on.location(), "/admin/system");
    assert!(updates::status(&h.db).await.unwrap().enabled);
    let actions: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'updates.enabled' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            serde_json::json!({ "enabled": false }),
            serde_json::json!({ "enabled": true })
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_audit_log_pages_back(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    for i in 0..60 {
        tether_db::audit::record(
            &h.db,
            tether_db::audit::Actor::System,
            "test.entry",
            Some(&format!("thing:{i}")),
            serde_json::json!({ "n": i }),
        )
        .await
        .unwrap();
    }
    let first = page(&h, "/admin/audit", &owner).await.body;
    assert!(first.contains("thing:59"));
    assert!(!first.contains("thing:9<"));
    let older = first
        .split("/admin/audit?before=")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();
    let second = page(&h, &format!("/admin/audit?before={older}"), &owner)
        .await
        .body;
    assert!(second.contains("thing:9<"), "{second}");
    assert!(!second.contains("thing:59"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn github_learns_nothing_about_the_instance_and_checks_do_not_pile_up(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({})))
        .mount(&github)
        .await;
    let http = github_client(&github);
    updates::check(&h.db, &http, &source(&github))
        .await
        .unwrap();
    let sent = github.received_requests().await.unwrap();
    let agent = sent[0].headers.get("user-agent").unwrap().to_str().unwrap();
    assert_eq!(agent, format!("tether/{}", updates::CURRENT));
    assert!(!agent.contains("tether.test"));

    // Three clicks, one queued check, three audit entries.
    for _ in 0..3 {
        let res = send(&h.app, form("/admin/system/updates/check", "", &owner)).await;
        assert_eq!(res.location(), "/admin/system");
    }
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'platform.update_check' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'updates.check'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 3);
}

fn github_client(github: &MockServer) -> tether_net::Outbound {
    updates::http_client(
        tether_net::Allowlist::production().with_local(&github.address().to_string()),
    )
    .unwrap()
}
