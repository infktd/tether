#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The Moon Mining plugin end to end: installed from its real component
//! and migrations, fed by mocked ESI through an approved data source,
//! pinging Discord at a pop, and showing fresh moons to Members, old moons
//! to Blue, mining totals, and the planner to Station Managers.

mod common;

use std::sync::OnceLock;

use axum::http::StatusCode;
use chrono::{Duration, SecondsFormat, Utc};
use common::*;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.moon-mining";
const CHRIBBA: i64 = 196379789;
const CHRIBBA_CORP: i64 = 1164409536;
const ATHANOR: i64 = 1030000000001;
const TATARA: i64 = 1030000000002;
const SYSTEM: i64 = 30000142;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("moon-mining")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/moon-mining/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// The real package, signed with a test key.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(7);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_moon_mining.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_moon_mining.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

fn rfc(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn mount_esi(h: &Harness) {
    let now = Utc::now();
    let json = |value: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(value)
    };
    // One chunk arrived (auto-fractures in two hours), one far off.
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/extractions"
        )))
        .respond_with(json(serde_json::json!([
            {
                "structure_id": ATHANOR,
                "moon_id": 40009081,
                "extraction_start_time": rfc(now - Duration::days(7)),
                "chunk_arrival_time": rfc(now - Duration::hours(1)),
                "natural_decay_time": rfc(now + Duration::hours(2)),
            },
            {
                "structure_id": TATARA,
                "moon_id": 40009082,
                "extraction_start_time": rfc(now - Duration::days(1)),
                "chunk_arrival_time": rfc(now + Duration::days(9)),
                "natural_decay_time": rfc(now + Duration::days(9) + Duration::hours(3)),
            },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(json(serde_json::json!([
            { "structure_id": ATHANOR, "name": "Jita - Drill One", "system_id": SYSTEM,
              "type_id": 35835, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable" },
            { "structure_id": TATARA, "name": "Jita - Drill Two", "system_id": SYSTEM,
              "type_id": 35836, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable" },
            { "structure_id": 1030000000009i64, "name": "Jita - Market", "system_id": SYSTEM,
              "type_id": 35832, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable" },
        ])))
        .mount(&h.esi_server)
        .await;
    for (moon, name) in [
        (40009081, "Jita IV - Moon 4"),
        (40009082, "Jita IV - Moon 5"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/moons/{moon}")))
            .respond_with(json(serde_json::json!({
                "moon_id": moon, "name": name, "system_id": SYSTEM,
                "position": { "x": 1.0, "y": 2.0, "z": 3.0 },
            })))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(json(serde_json::json!([
            { "id": SYSTEM, "name": "Jita", "category": "solar_system" },
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": 46676, "name": "Sylvite", "category": "inventory_type" },
        ])))
        // Before the harness's own names fixture.
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/roles")))
        .respond_with(json(serde_json::json!([
            { "character_id": CHRIBBA, "roles": ["Station_Manager", "Accountant"] },
            { "character_id": 90000099, "roles": ["Hangar_Take_1"] },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/observers"
        )))
        .respond_with(json(serde_json::json!([
            { "observer_id": ATHANOR, "observer_type": "structure", "last_updated": "2026-09-24" },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/observers/{ATHANOR}"
        )))
        .respond_with(json(serde_json::json!([
            { "character_id": CHRIBBA, "last_updated": now.date_naive().to_string(),
              "quantity": 12500, "recorded_corporation_id": CHRIBBA_CORP, "type_id": 46676 },
        ])))
        .mount(&h.esi_server)
        .await;
}

/// Offers Chribba as the data source (the SSO round trip) and approves it.
async fn approve_source(h: &Harness, owner: &str) -> String {
    let res = send(
        &h.app,
        form(&format!("/profile/plugins/{ID}/offer"), "", owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{CHRIBBA}:Chribba&state={state}"),
            &[(LOGIN, &login), (SESSION, owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let owner = res.cookie_value(SESSION);
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/sources/{CHRIBBA}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    owner
}

fn registry(h: &Harness) -> Registry {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    registry
}

async fn work(h: &Harness) {
    let registry = registry(h);
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// Runs one of the plugin's schedules now.
async fn run_schedule(h: &Harness, name: &str) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:{name}"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
    let errors: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap_or_default();
    assert!(errors.is_empty(), "{name}: {errors:?}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn moon_mining_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    let owner = approve_source(&h, &owner).await;

    run_schedule(&h, "sync").await;
    run_schedule(&h, "roles").await;
    run_schedule(&h, "ledger").await;

    // Members (the owner holds everything): extractions, with names.
    let moons = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(moons.status, StatusCode::OK, "{}", moons.body);
    assert!(moons.body.contains("Jita IV - Moon 4"), "{}", moons.body);
    assert!(moons.body.contains(">Jita<"), "{}", moons.body);
    assert!(moons.body.contains("Jita - Drill One"));
    assert!(moons.body.contains("Ready"));
    // Only refineries are kept.
    assert!(!moons.body.contains("Jita - Market"));

    let totals = page(&h, &format!("/plugins/{ID}/totals"), &owner).await;
    // A row with the pilot's name (not just the sidebar's).
    assert!(
        totals.body.contains("<td>Chribba</td>") || totals.body.contains(">Chribba<"),
        "{}",
        totals.body
    );
    assert!(
        !totals.body.contains("Character 196379789"),
        "{}",
        totals.body
    );
    assert!(
        totals.body.contains("12500") || totals.body.contains("12,500"),
        "{}",
        totals.body
    );
    assert!(totals.body.contains("Sylvite"), "{}", totals.body);

    // Chribba holds Station Manager: the planner advises the idle-soon
    // drills with a duration to set.
    let planner = page(&h, &format!("/plugins/{ID}/planner"), &owner).await;
    assert_eq!(planner.status, StatusCode::OK, "{}", planner.body);
    assert!(
        planner.body.contains("Jita - Drill Two"),
        "{}",
        planner.body
    );
    assert!(planner.body.contains("Save cadence"));
    let saved = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/planner"),
            &format!("_form=cadence_{CHRIBBA_CORP}&every_hours=12&at_time=07%3A30"),
            &owner,
        ),
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert!(
        page(&h, &format!("/plugins/{ID}/planner"), &owner)
            .await
            .body
            .contains("07:30")
    );

    // A pop pings Members on Discord, once a channel is set.
    discord_ready(&h, &owner).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/channels"),
            &format!("channel_id={DISCORD_PING_CHANNEL}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!("_form=settings&fresh_hours=4&ping_channel={DISCORD_PING_CHANNEL}&pings=on"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    // The Athanor's pop is two hours off: bring it forward.
    let pings: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key LIKE 'pop:%' AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(pings, 2);
    sqlx::query("UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key LIKE $2")
        .bind(ID)
        .bind(format!("pop:{ATHANOR}:%"))
        .execute(&h.db)
        .await
        .unwrap();
    work(&h).await;
    let sent: Vec<serde_json::Value> = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/messages"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let content = sent[0]["content"].as_str().unwrap();
    assert!(content.contains("Jita IV - Moon 4"), "{content}");
    assert!(
        content.starts_with(&format!("<@&{DISCORD_MEMBER_ROLE}>")),
        "{content}"
    );

    // Blue see only the old-moon list, once granted it.
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &blue).await.status,
        StatusCode::NOT_FOUND
    );
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.old&grantee=state:{BLUE_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let seen = page(&h, &format!("/plugins/{ID}"), &blue).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Old moons"));
    assert!(!seen.body.contains("Jita IV - Moon 4"), "{}", seen.body);
    assert!(!seen.body.contains("Fresh moons"));
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/planner"), &blue)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}
