//! The admin dashboard: Health (systems, dead jobs, schedules, the
//! version), Settings, update checks and the audit log.

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
    for uri in ["/admin/system", "/admin/settings", "/admin/audit"] {
        assert_eq!(send(&h.app, get(uri, &[])).await.location(), "/login");
        assert_eq!(
            page(&h, uri, &pilot).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(page(&h, uri, &owner).await.status, StatusCode::OK, "{uri}");
    }
    for uri in [
        "/admin/settings",
        "/admin/system/updates/check",
        "/admin/jobs/1/retry",
    ] {
        assert_eq!(
            send(&h.app, form(uri, "", &pilot)).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    // Administration's overview lists it.
    let overview = page(&h, "/admin", &owner).await.body;
    assert!(overview.contains(r#"href="/admin/system""#), "{overview}");
    assert!(overview.contains(r#"href="/admin/settings""#), "{overview}");
    let health = page(&h, "/admin/system", &owner).await.body;
    // ESI's line: up, with Tranquility's pilots online, not a badge.
    assert!(health.contains("pilots online"), "{health}");
    assert!(!health.contains("Answering"), "{health}");
    assert!(health.contains(r#"href="/admin/audit""#), "views bar");
    assert!(health.contains(r#"href="/admin/settings""#), "views bar");
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
async fn the_status_strip_asks_esi_once_a_minute(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = owner_and_pilot(&h).await;
    assert_eq!(
        send(&h.app, get("/status/strip", &[])).await.location(),
        "/login"
    );
    // A tab whose session ran out goes to log in, whole.
    let poll = send(
        &h.app,
        Request::get("/status/strip")
            .header("HX-Request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(poll.status, StatusCode::OK);
    assert_eq!(poll.headers["HX-Redirect"], "/login");
    assert!(poll.body.is_empty(), "{}", poll.body);
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(503).set_body_raw(r#"{"error":"downtime"}"#, "application/json"),
        )
        .mount(&h.esi_server)
        .await;
    let asked = || async {
        h.esi_server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().starts_with("/status"))
            .count()
    };
    let strip = page(&h, "/status/strip", &pilot).await;
    assert_eq!(strip.status, StatusCode::OK, "{}", strip.body);
    assert!(strip.body.contains("ESI UNREACHABLE"), "{}", strip.body);
    // Tranquility stays in the bar, unknown rather than gone.
    assert!(
        strip
            .body
            .contains(r#"TQ <span class="text-foreground">—</span>"#),
        "{}",
        strip.body
    );
    // Why is for admins, on System: pilots see only that it's down.
    assert!(
        !strip.body.contains("downtime") && !strip.body.contains("title="),
        "{}",
        strip.body
    );
    let first = asked().await;
    assert!(first >= 1);
    // Every tab polls it; the failure stands for a minute, so ESI isn't
    // asked again.
    for _ in 0..3 {
        page(&h, "/status/strip", &pilot).await;
    }
    assert_eq!(asked().await, first);
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
    let off = save_instance_settings(&h, &owner, &[("updates", "")]).await;
    assert_eq!(off.location(), "/admin/settings");
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

    let on = save_instance_settings(&h, &owner, &[("updates", "on")]).await;
    assert_eq!(on.location(), "/admin/settings");
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

/// Entries for the audit log's filters: the owner's group changes, the
/// system's app work (an app's own, one of its schedules', and a state
/// requiring it), the CLI's, and one dated last year.
async fn audit_entries(h: &Harness) -> i64 {
    use serde_json::json;
    use tether_db::audit::{Actor, record};
    let owner: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = 196379789")
            .fetch_one(&h.db)
            .await
            .unwrap();
    let me = Actor::Account(tether_db::accounts::AccountId(owner));
    record(
        &h.db,
        me,
        "group.join",
        Some("group:1"),
        json!({"name": "Capitals"}),
    )
    .await
    .unwrap();
    record(
        &h.db,
        me,
        "group.leave",
        Some("group:2"),
        json!({"name": "Logistics"}),
    )
    .await
    .unwrap();
    record(
        &h.db,
        Actor::System,
        "plugin.installed",
        Some("plugin:acme.moons"),
        json!({}),
    )
    .await
    .unwrap();
    record(
        &h.db,
        Actor::System,
        "schedule.run_now",
        Some("schedule:plugin:acme.moons:sync"),
        json!({}),
    )
    .await
    .unwrap();
    record(
        &h.db,
        Actor::System,
        "state.app_required",
        Some("state:1"),
        json!({"app": "acme.moons"}),
    )
    .await
    .unwrap();
    record(
        &h.db,
        Actor::Cli,
        "sync.trigger",
        None,
        json!({"note": "=HYPERLINK(\"x\")"}),
    )
    .await
    .unwrap();
    // Entries can't be changed, so last year's is written as it was.
    sqlx::query(
        "INSERT INTO core.audit_log (at, action, target) VALUES ('2025-03-01T12:00:00Z', 'old.thing', 'thing:old')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    owner
}

/// The rows' actions on an audit log page.
fn audit_rows(body: &str) -> Vec<String> {
    body.split(r#"<a class="num font-medium" href="#)
        .skip(1)
        .filter_map(|rest| rest.split('>').nth(1))
        .filter_map(|text| text.split('<').next())
        .map(str::to_owned)
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_audit_log_filters_by_who_action_app_and_date(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let me = audit_entries(&h).await;
    let rows = |uri: String| {
        let (h, owner) = (&h, owner.clone());
        async move {
            let res = page(h, &uri, &owner).await;
            assert_eq!(res.status, StatusCode::OK, "{uri}");
            (audit_rows(&res.body), res.body)
        }
    };
    let (all, body) = rows("/admin/audit".to_owned()).await;
    assert!(all.len() >= 7, "{all:?}");
    // The toolbar: the search, the filters and the CSV with them.
    assert!(body.contains(r#"<form class="toolbar-search" method="get" action="/admin/audit""#));
    assert!(
        body.contains(r#"href="/admin/audit?action=group.%2A">group</a>"#),
        "{body}"
    );
    assert!(body.contains(r#"href="/admin/audit?app=acme.moons">acme.moons</a>"#));
    assert!(body.contains(r#"href="/admin/audit.csv""#));

    let (mine, body) = rows(format!("/admin/audit?who={me}")).await;
    assert_eq!(mine, ["group.leave", "group.join", "setup.owner"]);
    assert!(body.contains(
        r#"<span class="filter-chip">Who <span class="filter-chip-value">Chribba</span>"#
    ));
    assert!(body.contains(&format!(r#"href="/admin/audit.csv?who={me}""#)));
    let (cli, _) = rows("/admin/audit?who=cli".to_owned()).await;
    assert_eq!(cli, ["sync.trigger"]);
    let (system, _) = rows("/admin/audit?who=system".to_owned()).await;
    assert!(
        system.contains(&"plugin.installed".to_owned())
            && !system.contains(&"sync.trigger".to_owned())
    );

    let (family, _) = rows("/admin/audit?action=group.*".to_owned()).await;
    assert_eq!(family, ["group.leave", "group.join"]);
    let (one, body) = rows("/admin/audit?action=group.join".to_owned()).await;
    assert_eq!(one, ["group.join"]);
    assert!(body.contains(r#"Action <span class="filter-chip-value">group.join</span>"#));

    let (app, _) = rows("/admin/audit?app=acme.moons".to_owned()).await;
    assert_eq!(
        app,
        ["state.app_required", "schedule.run_now", "plugin.installed"]
    );
    let (both, _) = rows("/admin/audit?app=acme.moons&action=schedule.*".to_owned()).await;
    assert_eq!(both, ["schedule.run_now"]);

    let (old, body) = rows("/admin/audit?from=2025-01-01&to=2025-12-31".to_owned()).await;
    assert_eq!(old, ["old.thing"]);
    assert!(
        body.contains(r#"Date <span class="filter-chip-value">2025-01-01 to 2025-12-31</span>"#)
    );
    let (recent, _) = rows("/admin/audit?from=2026-01-01".to_owned()).await;
    assert!(!recent.contains(&"old.thing".to_owned()));

    let (found, _) = rows("/admin/audit?q=LOGISTICS".to_owned()).await;
    assert_eq!(found, ["group.leave"]);
    // A search's % is a percent sign, not a wildcard.
    let (none, body) = rows("/admin/audit?q=%25%25".to_owned()).await;
    assert!(none.is_empty());
    assert!(body.contains("Nothing matches these filters."));
    // Nonsense is left out rather than refused.
    let (all_again, _) = rows("/admin/audit?who=nobody&app=a%20b&from=yesterday".to_owned()).await;
    assert_eq!(all_again, all);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_audit_log_pages_back_within_its_filters(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    for i in 0..60 {
        for action in ["test.kept", "other.dropped"] {
            tether_db::audit::record(
                &h.db,
                tether_db::audit::Actor::System,
                action,
                Some(&format!("thing:{i}")),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        }
    }
    let first = page(&h, "/admin/audit?action=test.kept", &owner).await.body;
    let older = first
        .split(r#"href="/admin/audit?action=test.kept&#38;before="#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();
    let second = page(
        &h,
        &format!("/admin/audit?action=test.kept&before={older}"),
        &owner,
    )
    .await
    .body;
    let rows = audit_rows(&second);
    assert_eq!(rows.len(), 10, "{rows:?}");
    assert!(rows.iter().all(|r| r == "test.kept"));
    assert!(second.contains(r#"href="/admin/audit?action=test.kept">Newest</a>"#));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_audit_log_exports_its_filtered_rows_as_csv(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let me = audit_entries(&h).await;
    assert_eq!(
        send(&h.app, get("/admin/audit.csv", &[])).await.location(),
        "/login"
    );
    assert_eq!(
        page(&h, "/admin/audit.csv", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    let res = page(&h, "/admin/audit.csv?app=acme.moons", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.headers["content-type"], "text/csv; charset=utf-8");
    let disposition = res.headers["content-disposition"].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"tether-audit-"),
        "{disposition}"
    );
    let lines: Vec<&str> = res.body.lines().collect();
    assert_eq!(lines[0], "id,at (EVE),who,account id,action,target,details");
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[1].contains(",System,,state.app_required,state:1,"));
    // A formula in the details is defused.
    let cli = page(&h, "/admin/audit.csv?who=cli", &owner).await.body;
    assert!(
        cli.contains(r#",CLI,,sync.trigger,,"{""note"":""=HYPERLINK(\""x\"")""}""#),
        "{cli}"
    );
    // The export is logged, with its filters.
    let logged: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'audit.export' ORDER BY id LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(
        logged,
        serde_json::json!({"filters": {"app": "acme.moons"}})
    );
    // Every page of the log, a thousand at a time.
    for i in 0..1_100 {
        tether_db::audit::record(
            &h.db,
            tether_db::audit::Actor::System,
            "bulk.entry",
            Some(&format!("thing:{i}")),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    }
    let bulk = page(&h, "/admin/audit.csv?action=bulk.entry", &owner)
        .await
        .body;
    assert_eq!(bulk.lines().count(), 1_101);
    let mine = page(&h, &format!("/admin/audit.csv?who={me}"), &owner)
        .await
        .body;
    assert!(mine.contains(",group.join,") && mine.contains(",group.leave,"));
    assert!(
        mine.lines()
            .skip(1)
            .all(|l| l.contains(&format!(",Chribba,{me},"))),
        "{mine}"
    );
    // Asked by htmx, it's a navigation, so the browser saves it.
    let mut req = get("/admin/audit.csv?who=cli", &[(SESSION, &owner)]);
    req.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let res = send(&h.app, req).await;
    assert_eq!(res.headers["hx-redirect"], "/admin/audit.csv?who=cli");
    assert!(res.body.is_empty());
    // A link from another site opens the log instead, exporting nothing.
    let exports = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM core.audit_log WHERE action = 'audit.export'",
        )
        .fetch_one(&h.db)
        .await
        .unwrap()
    };
    let before = exports().await;
    let mut req = get("/admin/audit.csv?q=planted", &[(SESSION, &owner)]);
    req.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let res = send(&h.app, req).await;
    assert_eq!(res.location(), "/admin/audit?q=planted");
    assert_eq!(exports().await, before);
    // Dates no log could hold are left out, not a server error.
    let res = page(&h, "/admin/audit.csv?from=-5000-01-01&who=cli", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    // At most ten exports a minute each.
    let mut last = StatusCode::OK;
    for _ in 0..10 {
        last = page(&h, "/admin/audit.csv?who=cli", &owner).await.status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
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

/// Run now: a schedule's job is queued at once, once; a run in flight
/// isn't doubled; apps' schedules are run from their pages.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn schedules_run_now_from_the_system_page(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    tether_jobs::schedule::ensure(
        &h.db,
        &tether_jobs::schedule::ScheduleSpec::new(
            "affiliation.sync",
            "affiliation.sync",
            std::time::Duration::from_secs(3600),
        ),
    )
    .await
    .unwrap();

    let res = send(
        &h.app,
        form("/admin/system/schedules/affiliation.sync/run", "", &owner),
    )
    .await;
    assert_eq!(res.location(), "/admin/system", "{}", res.body);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE schedule = 'affiliation.sync' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);

    // Still queued: not doubled.
    let again = send(
        &h.app,
        form("/admin/system/schedules/affiliation.sync/run", "", &owner),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(
        again.body.contains("still queued or running"),
        "{}",
        again.body
    );

    let app = send(
        &h.app,
        form("/admin/system/schedules/plugin:acme.x:y/run", "", &owner),
    )
    .await;
    assert_eq!(app.status, StatusCode::BAD_REQUEST);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'schedule.run_now'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 1);
    let page = page(&h, "/admin/system", &owner).await.body;
    assert!(page.contains("/admin/system/schedules/affiliation.sync/run"));
    assert!(page.contains(r#"hx-target="this" hx-swap="outerHTML""#));

    // From the page (htmx): a fragment for the button's place, no reload.
    let mut req = form("/admin/system/schedules/affiliation.sync/run", "", &owner);
    req.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let swapped = send(&h.app, req).await;
    assert_eq!(swapped.status, StatusCode::OK);
    assert!(!swapped.body.contains("<html"), "{}", swapped.body);
    assert!(
        swapped.body.contains("still queued or running"),
        "{}",
        swapped.body
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_notification_cap_is_a_setting(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let shown = page(&h, "/admin/settings", &owner).await;
    assert!(
        shown.body.contains(r#"name="max_per_user""#) && shown.body.contains(r#"value="50""#),
        "AA's default: {}",
        shown.body
    );
    let res = save_instance_settings(&h, &owner, &[("max_per_user", "0")]).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    let res = send(&h.app, form("/admin/settings", "max_per_user=3", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = save_instance_settings(&h, &owner, &[("max_per_user", "3")]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert_eq!(
        tether_db::settings::notifications_max(&h.db).await.unwrap(),
        3
    );
}

/// aa-memberaudit's MEMBERAUDIT_NOTIFY_TOKEN_ERRORS: on unless set, and
/// saved with the rest of Settings, audited.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn member_audit_token_error_notices_are_a_setting(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let stored = || async {
        tether_db::settings::get_bool_or(
            &h.db,
            tether_db::settings::MEMBER_AUDIT_TOKEN_ERRORS,
            true,
        )
        .await
        .unwrap()
    };
    let shown = page(&h, "/admin/settings", &owner).await.body;
    assert!(
        shown.contains(r#"name="token_errors" value="on" checked"#),
        "AA's default: {shown}"
    );
    // Without Member Audit, there's nobody to tell: the page says so.
    assert!(
        shown.contains("Member Audit isn't installed, so there's nobody to tell"),
        "{shown}"
    );
    // Another field saved leaves it alone.
    let res = save_instance_settings(&h, &owner, &[("max_per_user", "9")]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert!(stored().await);
    let res = save_instance_settings(&h, &owner, &[("token_errors", "")]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert!(!stored().await);
    let details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'notifications.settings' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(
        details,
        serde_json::json!({ "member_audit_token_errors": false })
    );
    let res = save_instance_settings(&h, &owner, &[("token_errors", "on")]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert!(stored().await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn settings_save_what_changed_at_once(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let audited = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT action FROM core.audit_log WHERE action IN \
             ('site.name', 'theme.accent', 'notifications.settings', 'updates.enabled') \
             ORDER BY id",
        )
        .fetch_all(&h.db)
        .await
        .unwrap()
    };

    // The page as it is: nothing to save.
    let res = save_instance_settings(&h, &owner, &[]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert!(audited().await.is_empty());

    // One bad value saves nothing, the good ones with it.
    let res = save_instance_settings(
        &h,
        &owner,
        &[("site_name", "Some Alliance"), ("max_per_user", "5000")],
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("Keep 1 to 1000"), "{}", res.body);
    assert!(audited().await.is_empty());
    // With script, what's shown stays: a toast says why.
    let res = send(
        &h.app,
        boosted(
            form(
                "/admin/settings",
                "site_name=Some+Alliance&accent=%23a78bfa&max_per_user=5000&updates=on",
                &owner,
            ),
            "/admin/settings",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let (message, tone) = toast(&res).unwrap();
    assert!(message.contains("Keep 1 to 1000"), "{message}");
    assert_eq!(tone, "problem");
    assert!(audited().await.is_empty());

    // Two changes, two saves; the rest as it was.
    let res = save_instance_settings(
        &h,
        &owner,
        &[("site_name", "Some Alliance"), ("max_per_user", "7")],
    )
    .await;
    assert_eq!(res.location(), "/admin/settings");
    assert_eq!(audited().await, ["site.name", "notifications.settings"]);
    let shown = page(&h, "/admin/settings", &owner).await.body;
    assert!(shown.contains(r#"value="Some Alliance""#), "{shown}");
    assert!(shown.contains(r#"value="7""#), "{shown}");

    // A page shown before someone else's save doesn't undo it: what was
    // left as shown stays as it is now (update checks, here).
    let res = save_instance_settings(&h, &owner, &[("updates", "")]).await;
    assert_eq!(res.location(), "/admin/settings");
    let body = form_body(&shown, "instance-settings", &[("site_name", "Other Name")]);
    let res = send(&h.app, form("/admin/settings", &body, &owner)).await;
    assert_eq!(res.location(), "/admin/settings");
    assert!(!updates::status(&h.db).await.unwrap().enabled);
    assert_eq!(
        audited().await,
        [
            "site.name",
            "notifications.settings",
            "updates.enabled",
            "site.name"
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_site_name_shows_in_tabs_and_on_the_sign_in_page(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let title = |body: &str| {
        let start = body.find("<title>").unwrap() + "<title>".len();
        body[start..start + body[start..].find("</title>").unwrap()].to_owned()
    };
    assert_eq!(
        title(&page(&h, "/dashboard", &owner).await.body),
        "Dashboard · Tether"
    );

    // Only System's admins name it.
    let res = send(&h.app, form("/admin/settings", "site_name=Nope", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(
        &h.app,
        form("/admin/system/site-name", "site_name=Nope", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    let res = save_instance_settings(&h, &owner, &[("site_name", "  Some Alliance  ")]).await;
    assert_eq!(res.location(), "/admin/settings", "{}", res.body);
    assert_eq!(
        title(&page(&h, "/dashboard", &pilot).await.body),
        "Dashboard · Some Alliance · Tether"
    );
    let login = send(&h.app, get("/login", &[])).await.body;
    assert_eq!(title(&login), "Log in · Some Alliance · Tether");
    assert!(login.contains(r#"<div class="signin-site">Some Alliance</div>"#));
    assert!(
        page(&h, "/admin/settings", &owner)
            .await
            .body
            .contains(r#"value="Some Alliance""#)
    );

    // One short line.
    let long = "x".repeat(51);
    let res = save_instance_settings(&h, &owner, &[("site_name", &long)]).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/admin/system/site-name",
            &format!("site_name={long}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    // From the setup wizard's last step: back there.
    let res = send(
        &h.app,
        form("/admin/system/site-name", "site_name=Other+Name", &owner),
    )
    .await;
    assert_eq!(res.location(), "/setup");
    // Empty: Tether's alone again.
    let res = save_instance_settings(&h, &owner, &[("site_name", "")]).await;
    assert_eq!(res.location(), "/admin/settings");
    assert_eq!(
        title(&page(&h, "/dashboard", &owner).await.body),
        "Dashboard · Tether"
    );
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'site.name'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 3);
}

// ---- upgrades from the console ------------------------------------------------

/// The updater's heartbeat, as if it were running.
fn updater_alive(h: &Harness) {
    std::fs::write(h.updater.status.join("alive"), "now").unwrap();
}

fn request(h: &Harness) -> Option<String> {
    std::fs::read_to_string(h.updater.requests.join("request")).ok()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn upgrades_go_through_the_updater(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    mount_status(&h).await;

    // Only admins, and not without the updater.
    for (uri, body) in [
        ("/admin/system/upgrade", "tag=edge"),
        ("/admin/system/rollback", "confirmation=x"),
    ] {
        assert_eq!(
            send(&h.app, form(uri, body, &pilot)).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    assert_eq!(
        page(&h, "/admin/system/upgrade", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    let card = page(&h, "/admin/system", &owner).await.body;
    assert!(card.contains("The updater isn't running"), "{card}");
    let res = send(&h.app, form("/admin/system/upgrade", "tag=edge", &owner)).await;
    assert!(
        res.body.contains("The updater isn't running"),
        "{}",
        res.body
    );
    assert!(request(&h).is_none());

    // Running: edge offers the newest edge, and nothing else.
    updater_alive(&h);
    let card = page(&h, "/admin/system/upgrade", &owner).await.body;
    assert!(card.contains("Update to the newest edge"), "{card}");
    assert!(card.contains("ghcr.io/acme/tether:edge"), "{card}");
    let res = send(&h.app, form("/admin/system/upgrade", "tag=1.2.0", &owner)).await;
    assert!(
        res.body.contains("isn&#39;t offered") || res.body.contains("isn't offered"),
        "{}",
        res.body
    );
    assert!(request(&h).is_none());

    let res = send(&h.app, form("/admin/system/upgrade", "tag=edge", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let asked = request(&h).unwrap();
    let lines: Vec<&str> = asked.lines().collect();
    assert_eq!(lines[1..], ["action=upgrade", "tag=edge"], "{asked}");
    let id = lines[0].strip_prefix("id=").unwrap();
    assert!(
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    let audited: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'platform.upgrade'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited["to"], "edge");
    assert_eq!(audited["request"], id);

    // One at a time: the card polls until the updater is done.
    let card = page(&h, "/admin/system/upgrade", &owner).await.body;
    assert!(card.contains("Under way"), "{card}");
    assert!(card.contains(r#"hx-trigger="every 3s""#), "{card}");
    let res = send(&h.app, form("/admin/system/upgrade", "tag=edge", &owner)).await;
    assert!(res.body.contains("already under way"), "{}", res.body);

    // The updater took it, upgraded, and can go one step back. This start
    // migrated after a snapshot, which the rollback restores.
    std::fs::remove_file(h.updater.requests.join("request")).unwrap();
    std::fs::write(
        h.updater.status.join("status.json"),
        r#"{"id":"1","action":"upgrade","state":"done","message":"Upgraded to edge.","at":"2026-09-28T12:00:00Z"}"#,
    )
    .unwrap();
    std::fs::write(
        h.updater.status.join("previous"),
        "ghcr.io/acme/tether@sha256:aa",
    )
    .unwrap();
    let before = tether_web::upgrader::Updater {
        revision: Some("abcdef1234".to_owned()),
        ..(*h.updater).clone()
    };
    tether_web::upgrader::record_start(&h.db, &before, None)
        .await
        .unwrap();
    tether_web::upgrader::record_start(&h.db, &h.updater, Some("core-20260928T120000Z.tsnap"))
        .await
        .unwrap();
    // A restart of the same build keeps the snapshot from before it.
    tether_web::upgrader::record_start(&h.db, &h.updater, None)
        .await
        .unwrap();
    let card = page(&h, "/admin/system/upgrade", &owner).await.body;
    assert!(card.contains("Upgraded to edge."), "{card}");
    assert!(card.contains("Roll back to"), "{card}");
    assert!(card.contains("(abcdef1)"), "{card}");
    assert!(card.contains("restores the snapshot"), "{card}");

    let res = send(
        &h.app,
        form("/admin/system/rollback", "confirmation=nope", &owner),
    )
    .await;
    assert!(res.body.contains("to confirm the rollback"), "{}", res.body);
    assert!(request(&h).is_none());
    let confirm = format!(
        "confirmation={}",
        urlencoding(&format!("{} (abcdef1)", env!("CARGO_PKG_VERSION")))
    );
    let res = send(&h.app, form("/admin/system/rollback", &confirm, &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let asked = request(&h).unwrap();
    assert!(asked.contains("action=rollback\n"), "{asked}");
    assert!(
        asked.contains("snapshot=core-20260928T120000Z.tsnap\n"),
        "{asked}"
    );
}

fn urlencoding(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn upgrading_needs_a_recent_eve_login(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    updater_alive(&h);
    sqlx::query(
        "UPDATE core.sessions SET reauthenticated_at = now() - interval '30 minutes' \
         WHERE token_hash = sha256($1::bytea)",
    )
    .bind(owner.as_bytes())
    .execute(&h.db)
    .await
    .unwrap();
    let res = send(&h.app, form("/admin/system/upgrade", "tag=edge", &owner)).await;
    assert!(
        res.location().contains("action=platform_upgrade"),
        "{} {}",
        res.status,
        res.location()
    );
    assert!(request(&h).is_none());
}

/// Health (Jay, 2026-10-06): a verdict over a line for each system, saying
/// in words what's wrong rather than in a badge.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn health_says_what_is_wrong(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    mount_status(&h).await;
    for (name, every) in [("backups.nightly", 86_400), ("discord.sync_all", 300)] {
        tether_jobs::schedule::ensure(
            &h.db,
            &tether_jobs::schedule::ScheduleSpec::new(
                name,
                name,
                std::time::Duration::from_secs(every),
            ),
        )
        .await
        .unwrap();
    }
    // Nothing has run yet, and Discord isn't set up: not problems.
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains("All systems nominal"), "{body}");
    assert!(body.contains("None yet"), "{body}");
    assert!(body.contains("Not set up"), "{body}");
    assert!(body.contains("every 5 minutes"), "{body}");
    assert!(body.contains("every day"), "{body}");
    assert!(!body.contains("(s)"), "plurals: {body}");

    // The nightly backup gave up: a problem, its error the line's detail,
    // and a dead job in the queue.
    sqlx::query(
        "INSERT INTO core.jobs (kind, state, attempts, last_error, schedule, finished_at)
         VALUES ('backups.nightly', 'dead', 5, 'pg_dump not found', 'backups.nightly', now())",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains("1 problem"), "{body}");
    assert!(body.contains("pg_dump not found"), "{body}");
    assert!(body.contains("1 dead job<"), "{body}");
    assert!(body.contains(">Failed<"), "{body}");

    // It ran again: backed up. The dead job still wants someone.
    sqlx::query(
        "INSERT INTO core.jobs (kind, state, schedule, finished_at)
         VALUES ('backups.nightly', 'succeeded', 'backups.nightly', now())",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains("Backed up"), "{body}");
    assert!(body.contains("Needs attention"), "{body}");
    assert!(body.contains("1 warning"), "{body}");
    assert!(body.contains(">Succeeded<"), "{body}");
}

/// An app's schedules show under its name, run from its Apps page: Health
/// offers Run now only for Tether's own.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_schedules_go_under_their_app(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    mount_status(&h).await;
    tether_jobs::schedule::ensure(
        &h.db,
        &tether_jobs::schedule::ScheduleSpec::new(
            "plugin:acme.thing:refresh",
            "plugin.job",
            std::time::Duration::from_secs(1800),
        ),
    )
    .await
    .unwrap();
    let body = page(&h, "/admin/system", &owner).await.body;
    assert!(body.contains(">refresh<"), "{body}");
    assert!(
        body.contains(r#"href="/admin/plugins/acme.thing""#),
        "{body}"
    );
    assert!(body.contains("every 30 minutes"), "{body}");
    assert!(
        !body.contains("/admin/system/schedules/plugin:acme.thing:refresh/run"),
        "{body}"
    );
}
