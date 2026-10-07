//! The Moon Mining plugin end to end: installed from its real component
//! and migrations, fed by mocked ESI through an approved data source,
//! pinging Discord at a pop, and showing fresh moons to Members, old moons
//! to Blue, mining totals, and the planner to Station Managers; moon
//! surveys pasted and valued at ESI's market prices, the Moons page's tabs,
//! moon and extraction details, and Reports, each behind aa-moonmining's
//! permissions.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, SecondsFormat, Utc};
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
const ZEOLITES: i64 = 45490;
const SYLVITE: i64 = 45491;
const XENOTIME: i64 = 45510;

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
    let first = plugin_file("migrations/0001_moon_mining.sql");
    let second = plugin_file("migrations/0002_surveys_and_prices.sql");
    let third = plugin_file("migrations/0003_tether_rules_optional.sql");
    let fourth = plugin_file("migrations/0004_old_moons_shown.sql");
    let fifth = plugin_file("migrations/0005_refinery_drills.sql");
    let sixth = plugin_file("migrations/0006_corporation_reads.sql");
    let seventh = plugin_file("migrations/0007_admin_notifications.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_moon_mining.sql", first.as_bytes()),
        ("migrations/0002_surveys_and_prices.sql", second.as_bytes()),
        (
            "migrations/0003_tether_rules_optional.sql",
            third.as_bytes(),
        ),
        ("migrations/0004_old_moons_shown.sql", fourth.as_bytes()),
        ("migrations/0005_refinery_drills.sql", fifth.as_bytes()),
        ("migrations/0006_corporation_reads.sql", sixth.as_bytes()),
        (
            "migrations/0007_admin_notifications.sql",
            seventh.as_bytes(),
        ),
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
            // Long done: Past.
            {
                "structure_id": ATHANOR,
                "moon_id": 40009081,
                "extraction_start_time": rfc(now - Duration::days(20)),
                "chunk_arrival_time": rfc(now - Duration::days(13)),
                "natural_decay_time": rfc(now - Duration::days(13) + Duration::hours(3)),
            },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(json(serde_json::json!([
            { "structure_id": ATHANOR, "name": "Jita - Drill One", "system_id": SYSTEM,
              "type_id": 35835, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable",
              "services": [{ "name": "Moon Drilling", "state": "online" }] },
            { "structure_id": TATARA, "name": "Jita - Drill Two", "system_id": SYSTEM,
              "type_id": 35836, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable",
              "services": [{ "name": "Moon Drilling", "state": "offline" }, { "name": "Reprocessing", "state": "online" }] },
            // A refinery without a Moon Drill: reprocessing only.
            { "structure_id": 1030000000010i64, "name": "Jita - Reprocessing Plant", "system_id": SYSTEM,
              "type_id": 35835, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable",
              "services": [{ "name": "Reprocessing", "state": "online" }] },
            { "structure_id": 1030000000009i64, "name": "Jita - Market", "system_id": SYSTEM,
              "type_id": 35832, "corporation_id": CHRIBBA_CORP, "profile_id": 1, "state": "shield_vulnerable" },
        ])))
        .mount(&h.esi_server)
        .await;
    for (moon, name) in [
        (40009081, "Jita IV - Moon 4"),
        (40009082, "Jita IV - Moon 5"),
        (40009083, "Jita IV - Moon 6"),
        (40009084, "Jita IV - Moon 7"),
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
            { "id": CHRIBBA_CORP, "name": "Chribba Corp", "category": "corporation" },
            { "id": 20000020, "name": "Kimotoro", "category": "constellation" },
            { "id": 10000002, "name": "The Forge", "category": "region" },
            { "id": ZEOLITES, "name": "Zeolites", "category": "inventory_type" },
            { "id": SYLVITE, "name": "Sylvite", "category": "inventory_type" },
            { "id": XENOTIME, "name": "Xenotime", "category": "inventory_type" },
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
            // One day's mining split across two corporations (the pilot
            // moved), and a row a later page repeated: 12,500 in all.
            { "character_id": CHRIBBA, "last_updated": now.date_naive().to_string(),
              "quantity": 10000, "recorded_corporation_id": CHRIBBA_CORP, "type_id": SYLVITE },
            { "character_id": CHRIBBA, "last_updated": now.date_naive().to_string(),
              "quantity": 2500, "recorded_corporation_id": 98000009, "type_id": SYLVITE },
            { "character_id": CHRIBBA, "last_updated": now.date_naive().to_string(),
              "quantity": 2500, "recorded_corporation_id": 98000009, "type_id": SYLVITE },
        ])))
        .mount(&h.esi_server)
        .await;
}

/// Moon ores' item groups, CCP's prices and Jita's place: what values and
/// the Moons page need.
async fn mount_prices(h: &Harness) {
    let json = |value: serde_json::Value| ResponseTemplate::new(200).set_body_json(value);
    for (group, types) in [
        (1884, vec![ZEOLITES, SYLVITE]),
        (1920, vec![]),
        (1921, vec![]),
        (1922, vec![]),
        (1923, vec![XENOTIME]),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/groups/{group}")))
            .respond_with(json(serde_json::json!({
                "category_id": 25, "group_id": group, "name": "Moon Asteroids",
                "published": true, "types": types,
            })))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/markets/prices"))
        .respond_with(json(serde_json::json!([
            { "type_id": ZEOLITES, "average_price": 10000.0, "adjusted_price": 9000.0 },
            { "type_id": SYLVITE, "average_price": 20000.0, "adjusted_price": 19000.0 },
            // No average: the adjusted price stands in.
            { "type_id": XENOTIME, "adjusted_price": 100000.0 },
            { "type_id": 34, "average_price": 5.0, "adjusted_price": 5.0 },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/universe/systems/{SYSTEM}")))
        .respond_with(json(serde_json::json!({
            "system_id": SYSTEM, "name": "Jita", "constellation_id": 20000020,
            "security_status": 0.9459, "position": { "x": 1.0, "y": 2.0, "z": 3.0 },
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/constellations/20000020"))
        .respond_with(json(serde_json::json!({
            "constellation_id": 20000020, "name": "Kimotoro", "region_id": 10000002,
            "position": { "x": 1.0, "y": 2.0, "z": 3.0 }, "systems": [SYSTEM],
        })))
        .mount(&h.esi_server)
        .await;
}

/// Adds Chribba as the data source (the SSO round trip).
async fn approve_source(h: &Harness, owner: &str) -> String {
    add_source(h, owner, &format!("{CHRIBBA}:Chribba")).await
}

/// Adds `character` ("id:name") as a data source.
async fn add_source(h: &Harness, owner: &str, character: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{character}&state={state}"),
            &[(LOGIN, &login), (SESSION, owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // In use at once (AA's Add Owner): nobody approves it.
    res.cookie_value(SESSION)
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

async fn account_of(h: &Harness, character: i64) -> i64 {
    sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// The app's notices an account has, oldest first: "title | message".
async fn notices(h: &Harness, account: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT title || ' | ' || message FROM core.notifications \
         WHERE account_id = $1 AND plugin_id = $2 ORDER BY id",
    )
    .bind(account)
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// Runs one of the plugin's schedules now, whatever it logs.
async fn run_due_now(h: &Harness, name: &str) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:{name}"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
}

/// What the app logged at warn or error.
async fn warnings(h: &Harness) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap_or_default()
}

/// Runs one of the plugin's schedules now; it logs no warnings.
async fn run_schedule(h: &Harness, name: &str) {
    run_due_now(h, name).await;
    let errors = warnings(h).await;
    assert!(errors.is_empty(), "{name}: {errors:?}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn moon_mining_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // aa-moonmining's periodic tasks: refineries and extractions every 10
    // minutes, ledgers hourly, values daily.
    let schedules: Vec<(String, i32)> = sqlx::query_as(
        "SELECT name, every_secs FROM core.schedules WHERE name LIKE $1 ORDER BY name",
    )
    .bind(format!("plugin:{ID}:%"))
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        schedules,
        [
            (format!("plugin:{ID}:ledger"), 3600),
            (format!("plugin:{ID}:prices"), 86400),
            (format!("plugin:{ID}:roles"), 86400),
            (format!("plugin:{ID}:sync"), 600),
        ]
    );
    mount_esi(&h).await;
    mount_prices(&h).await;
    let owner = approve_source(&h, &owner).await;

    run_schedule(&h, "sync").await;
    run_schedule(&h, "roles").await;
    run_schedule(&h, "ledger").await;
    run_schedule(&h, "prices").await;
    // aa-moonmining's admin notice, once: the superuser hears of the new
    // owner.
    let added = [
        "Moon Mining: Owner added: Chribba Corp | Chribba Corp was added as a new owner."
            .to_owned(),
    ];
    assert_eq!(notices(&h, account_of(&h, CHRIBBA).await).await, added);
    run_schedule(&h, "sync").await;
    assert_eq!(notices(&h, account_of(&h, CHRIBBA).await).await, added);

    // Members (the owner holds everything): extractions, with names.
    let moons = page(&h, &format!("/plugins/{ID}"), &owner).await;
    // Corporation scopes: its Data sources page, under Manage, says whose
    // data it is, and the source works once the schedules read through it.
    let sources = page(&h, &format!("/plugins/{ID}/data-sources"), &owner).await;
    assert!(
        sources
            .body
            .contains("reads their corporation&#39;s data through"),
        "{}",
        sources.body
    );
    assert!(sources.body.contains(">Working</span>"), "{}", sources.body);
    // aa-moonmining has no Members-only window: off unless turned on.
    assert!(!moons.body.contains("Fresh moons"), "{}", moons.body);
    assert_eq!(moons.status, StatusCode::OK, "{}", moons.body);
    assert!(moons.body.contains("Jita IV - Moon 4"), "{}", moons.body);
    assert!(moons.body.contains(">Jita (0.9)<"), "{}", moons.body);
    assert!(moons.body.contains("Jita - Drill One"));
    assert!(moons.body.contains("Ready"));
    // Only refineries are kept.
    assert!(!moons.body.contains("Jita - Market"));
    // The views (the planner for a Station Manager) and Manage, opening
    // Settings, which Tether draws; refineries with their type's
    // icon, and the chunk's arrival counting down.
    assert!(
        moons
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings\"")),
        "{}",
        moons.body
    );
    let admin = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        admin
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings\"")),
        "{}",
        admin.body
    );
    // Only while it runs: a stopped app's settings page isn't there.
    let res = send(
        &h.app,
        form(&format!("/admin/plugins/{ID}/disable"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let admin = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        !admin
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings\"")),
        "{}",
        admin.body
    );
    let res = send(
        &h.app,
        form(&format!("/admin/plugins/{ID}/enable"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for href in ["moons", "reports", "totals", "planner", "upload"] {
        assert!(
            moons
                .body
                .contains(&format!("href=\"/plugins/{ID}/{href}\"")),
            "{href}: {}",
            moons.body
        );
    }
    assert!(
        moons.body.contains("aria-label=\"Views\""),
        "{}",
        moons.body
    );
    assert!(
        moons.body.contains("images.evetech.net/types/"),
        "{}",
        moons.body
    );
    assert!(moons.body.contains("data-countdown"), "{}", moons.body);

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
    // Pilots with their portraits.
    assert!(
        totals
            .body
            .contains("images.evetech.net/characters/196379789/portrait"),
        "{}",
        totals.body
    );

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
    // Every refinery Moon Mining reads, the idle ones listed first, under
    // their corporation's name.
    assert!(planner.body.contains("Idle refineries"), "{}", planner.body);
    // A refinery without a Moon Drill isn't planned.
    assert!(
        !planner.body.contains("Reprocessing Plant"),
        "{}",
        planner.body
    );
    assert!(
        !planner
            .body
            .contains(&format!("Corporation {CHRIBBA_CORP}")),
        "{}",
        planner.body
    );
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

    // Optional, not in aa-moonmining: a pop pings Members on Discord, once
    // a channel is set, and a Members-only window before Blue see it.
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
            &format!(
                "_form=settings&fresh_hours=4&ping_channel={DISCORD_PING_CHANNEL}&pings=on\
                 &volume_per_day=960400&days_per_month=30.4&stale_hours=12&old_moons_shown=5"
            ),
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
    // A card under Member's mention: the moon, its drill's render, the
    // owner above and the survey's ores.
    assert_eq!(
        sent[0]["content"],
        format!("<@&{DISCORD_MEMBER_ROLE}>"),
        "{sent:?}"
    );
    let card = &sent[0]["embeds"][0];
    assert_eq!(card["title"], "Moon popped: Jita IV - Moon 4", "{card}");
    assert_eq!(card["color"], 0x2e_cc71, "{card}");
    assert_eq!(
        card["thumbnail"]["url"], "https://images.evetech.net/types/35835/render?size=128",
        "{card}"
    );
    assert!(
        card["description"]
            .as_str()
            .unwrap()
            .contains("The ore is in space now"),
        "{card}"
    );
    assert_eq!(card["footer"]["text"], "Moon Mining", "{card}");
    // The channel taken away from the app: Settings still opens, on "No
    // pings".
    sqlx::query("DELETE FROM core.plugin_channels WHERE plugin_id = $1")
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(settings.body.contains("No pings"), "{}", settings.body);
    assert!(
        settings
            .body
            .contains("The channel pops went to isn&#39;t this app&#39;s any more"),
        "{}",
        settings.body
    );
    // The next pop isn't posted, and its job is done, not dead: the log
    // says why.
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key LIKE 'pop:%'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(&h).await;
    let messages = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path().ends_with("/messages"))
        .count();
    assert_eq!(messages, 1);
    let dead: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND state = 'dead'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(dead, 0);
    let warned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND level = 'warn' \
         AND message LIKE 'a pop wasn''t posted%'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(warned, 1);

    // Blue see only the old-moon list, once granted it.
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &blue).await.status,
        StatusCode::NOT_FOUND
    );
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
            &format!("permission=plugin.{ID}.basic_access&grantee=state:{BLUE_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let seen = page(&h, &format!("/plugins/{ID}"), &blue).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Old moons"));
    // Which moons, never when: that would map out the pop schedule.
    assert!(!seen.body.contains(">Popped<"), "{}", seen.body);
    let fresh = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(fresh.body.contains("Fresh moons"), "{}", fresh.body);
    // Old moons beside the fresh ones, not a tab of their own.
    let row = fresh
        .body
        .split(r#"class="section-row""#)
        .nth(1)
        .expect("fresh and old moons side by side");
    let fresh_at = row.find("Fresh moons").unwrap();
    let old_at = row.find("Old moons").unwrap();
    assert!(fresh_at < old_at, "{row}");
    let tabs = fresh.body.split(r#"aria-label="Tabs""#).nth(1).unwrap();
    let tabs = &tabs[..tabs.find("</nav>").unwrap()];
    assert!(
        tabs.contains(">Past<") && !tabs.contains("Old moons"),
        "{tabs}"
    );
    assert!(!seen.body.contains("Jita IV - Moon 4"), "{}", seen.body);
    assert!(!seen.body.contains("Fresh moons"));
    // Blue have the old moons and Moons (as aa-moonmining's navbar), no
    // more.
    assert!(seen.body.contains("aria-label=\"Views\""), "{}", seen.body);
    assert!(seen.body.contains(&format!("href=\"/plugins/{ID}/moons\"")));
    assert!(
        !seen
            .body
            .contains(&format!("href=\"/plugins/{ID}/reports\""))
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/planner"), &blue)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // With the window off again (aa-moonmining's way), basic_access alone
    // opens Moons, as aa-moonmining's index does.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&fresh_hours=0&ping_channel=&volume_per_day=960400\
             &days_per_month=30.4&stale_hours=12&old_moons_shown=5",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let seen = page(&h, &format!("/plugins/{ID}"), &blue).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(!seen.body.contains("Old moons"), "{}", seen.body);
    assert!(seen.body.contains("<h1"), "{}", seen.body);
    assert!(seen.body.contains(">Moons<"), "{}", seen.body);
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Pastes moon surveys as `token`: the page the upload answers with.
async fn upload_surveys(h: &Harness, token: &str, paste: &str) -> Res {
    send(
        &h.app,
        form(
            &format!("/plugins/{ID}/upload"),
            &format!("_form=survey&scan={}", urlencode(paste)),
            token,
        ),
    )
    .await
}

async fn grant(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{state}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

/// An amount of ISK as a cell shows it on hover, e.g. "49,633,472,000".
fn grouped(isk: f64) -> String {
    let n = (isk.trunc() as i64).to_string();
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && (n.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn has_isk(body: &str, grouped: &str) -> bool {
    body.contains(&format!("{grouped} ISK"))
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn surveys_values_moons_and_reports(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    let owner = approve_source(&h, &owner).await;
    run_schedule(&h, "sync").await;
    run_schedule(&h, "ledger").await;
    run_schedule(&h, "prices").await;

    // Three moons pasted as the game copies them, one of them broken.
    let paste = format!(
        "Moon\tMoon Product\tQuantity\tOre TypeID\tSolarSystemID\tPlanetID\tMoonID\n\
         Jita IV - Moon 4\n\
         \tZeolites\t0.300000011921\t{ZEOLITES}\t{SYSTEM}\t40009077\t40009081\n\
         \tSylvite\t0.200000002980\t{SYLVITE}\t{SYSTEM}\t40009077\t40009081\n\
         \tXenotime\t0.100000001490\t{XENOTIME}\t{SYSTEM}\t40009077\t40009081\n\
         Jita IV - Moon 6\n\
         \tSylvite\t0.5\t{SYLVITE}\t{SYSTEM}\t40009077\t40009083\n\
         Jita IV - Moon 9\n\
         \tSylvite\tlots\t{SYLVITE}\t{SYSTEM}\t40009077\t40009089\n"
    );
    let res = upload_surveys(&h, &owner, &paste).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("2 of 3 moons stored"), "{}", res.body);
    assert!(res.body.contains("Not stored"), "{}", res.body);
    assert!(
        res.body.contains("the quantity should be a share"),
        "{}",
        res.body
    );
    // Names and places come from ESI, in the job the upload queued.
    work(&h).await;

    // Σ share × price = 0.3 × 10,000 + 0.2 × 20,000 + 0.1 × 100,000 (the
    // adjusted price: no average) = 17,000 a unit; × 960,400 m³ × 30.4
    // days ÷ 10 m³ ≈ 49.6 billion ISK a month.
    let worth = 0.300000011921 * 10000.0 + 0.200000002980 * 20000.0 + 0.100000001490 * 100000.0;
    let moon4 = grouped(worth * 960400.0 * 30.4 / 10.0);

    // Moons: the owner holds every tab. Owned first: the refineries' moons.
    let moons = page(&h, &format!("/plugins/{ID}/moons"), &owner).await;
    assert_eq!(moons.status, StatusCode::OK, "{}", moons.body);
    for part in [
        "Owned Moons",
        "All Moons",
        "My Uploaded Moons",
        "Jita IV - Moon 4",
        "Jita IV - Moon 5",
        "Jita (0.9)",
        "Kimotoro, The Forge",
        "Jita - Drill One",
        "R64",
        // A moon's name opens its record panel beside the list.
        &format!("href=\"/plugins/{ID}/moons?moon=40009081\""),
    ] {
        assert!(moons.body.contains(part), "{part}: {}", moons.body);
    }
    // Its panel: what its page has, in short, and the way there; the
    // list as it was (its tab and search kept).
    let panel = page(
        &h,
        &format!("/plugins/{ID}/moons?_tab=1&q=moon&moon=40009081"),
        &owner,
    )
    .await;
    assert_eq!(panel.status, StatusCode::OK, "{}", panel.body);
    for part in [
        r#"<aside class="record-panel bk""#,
        r#"class="record-panel-title">Jita IV - Moon 4</h2>"#,
        "Moon · R64",
        "Jita - Drill One",
        &format!(r#"href="/plugins/{ID}/moon/40009081">Open moon</a>"#),
        &format!(r#"href="/plugins/{ID}/moons?q=moon&#38;_tab=1" aria-label="Close""#),
        r#"<tr data-selected aria-current="true">"#,
    ] {
        assert!(panel.body.contains(part), "{part}: {}", panel.body);
    }
    // One the viewer may not see, or that isn't there, opens nothing.
    let none = page(&h, &format!("/plugins/{ID}/moons?moon=1"), &owner).await;
    assert_eq!(none.status, StatusCode::OK, "{}", none.body);
    assert!(!none.body.contains("record-panel"), "{}", none.body);
    assert!(!moons.body.contains("Jita IV - Moon 6"), "{}", moons.body);
    assert!(has_isk(&moons.body, &moon4), "{moon4}: {}", moons.body);
    // All Moons: surveyed ones too (Sylvite 0.5 × 20,000, a month).
    let all = page(&h, &format!("/plugins/{ID}/moons?_tab=1"), &owner).await;
    assert!(all.body.contains("Jita IV - Moon 6"), "{}", all.body);
    assert!(has_isk(&all.body, "29,196,160,000"), "{}", all.body);
    let mine = page(&h, &format!("/plugins/{ID}/moons?_tab=2"), &owner).await;
    assert!(mine.body.contains("Jita IV - Moon 6"), "{}", mine.body);
    // Not surveyed by anyone: not the owner's upload.
    assert!(!mine.body.contains("Jita IV - Moon 5"), "{}", mine.body);
    // The toolbar's search, in the address, narrows the tab it's made
    // from; the app searches its own data (owners it doesn't show too).
    let searched = page(&h, &format!("/plugins/{ID}/moons?_tab=1&q=moon+6"), &owner).await;
    assert_eq!(searched.status, StatusCode::OK, "{}", searched.body);
    assert!(searched.body.contains("Jita IV - Moon 6"));
    assert!(
        !searched.body.contains("Jita IV - Moon 4"),
        "{}",
        searched.body
    );
    assert!(
        searched
            .body
            .contains(r#"placeholder="Search moons, systems, regions, refineries, owners""#)
            && !searched.body.contains("data-instant"),
        "{}",
        searched.body
    );
    let r64 = page(&h, &format!("/plugins/{ID}/moons?_tab=1&rarity=64"), &owner).await;
    assert!(r64.body.contains("Jita IV - Moon 4"), "{}", r64.body);
    assert!(!r64.body.contains("Jita IV - Moon 6"), "{}", r64.body);
    // The filter applied, as a chip; the tab kept when it's taken off.
    assert!(
        r64.body.contains(&format!(
            r#"<a href="/plugins/{ID}/moons?_tab=1" aria-label="Take off Rarity: R64">"#
        )),
        "{}",
        r64.body
    );
    // Any other rarity is none.
    let any = page(&h, &format!("/plugins/{ID}/moons?_tab=1&rarity=5"), &owner).await;
    assert!(any.body.contains("Jita IV - Moon 6"), "{}", any.body);

    // A moon's details: composition with icons, value, the last survey.
    let moon = page(&h, &format!("/plugins/{ID}/moon/40009081"), &owner).await;
    assert_eq!(moon.status, StatusCode::OK, "{}", moon.body);
    for part in [
        "Ore composition",
        "Zeolites",
        "30.0%",
        &format!("images.evetech.net/types/{XENOTIME}/"),
        "Chribba Corp",
        "Surveyed by",
        "Chunk arrival",
    ] {
        assert!(moon.body.contains(part), "{part}: {}", moon.body);
    }
    assert!(has_isk(&moon.body, &moon4), "{}", moon.body);

    // Extractions: est. value from the survey, mined from the ledger, and
    // the Past tab.
    let main = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(main.body.contains("Past"), "{}", main.body);
    let past = page(&h, &format!("/plugins/{ID}?_tab=1"), &owner).await;
    assert!(past.body.contains("Completed"), "{}", past.body);
    assert!(past.body.contains("Past extractions"), "{}", past.body);
    // 167 hours × 960,400 m³ a day ÷ 24 ÷ 10 m³ a unit × 17,000.
    let chunk = grouped(167.0 * 960400.0 / 24.0 / 10.0 * worth);
    assert!(has_isk(&main.body, &chunk), "{chunk}: {}", main.body);
    // 12,500 Sylvite at 20,000.
    assert!(has_isk(&main.body, "250,000,000"), "{}", main.body);
    let at = main
        .body
        .split(&format!("href=\"/plugins/{ID}/extraction/{ATHANOR}/"))
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();
    let extraction = format!("/plugins/{ID}/extraction/{ATHANOR}/{at}");
    let shown = page(&h, &extraction, &owner).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    for part in [
        "Products (est.)",
        "Xenotime",
        "Mining ledger",
        "Totals by character",
        "Chribba",
    ] {
        assert!(shown.body.contains(part), "{part}: {}", shown.body);
    }
    let per = page(&h, &format!("{extraction}?_tab=1"), &owner).await;
    assert!(per.body.contains("100.0%"), "{}", per.body);
    let totals = page(&h, &format!("/plugins/{ID}/totals"), &owner).await;
    assert!(has_isk(&totals.body, "250,000,000"), "{}", totals.body);

    // Reports.
    let reports = page(&h, &format!("/plugins/{ID}/reports"), &owner).await;
    assert_eq!(reports.status, StatusCode::OK, "{}", reports.body);
    for part in [
        "Potential monthly income",
        "Member mining",
        "Member uploads",
        "Ore prices",
        "The Forge",
        "not surveyed",
    ] {
        assert!(reports.body.contains(part), "{part}: {}", reports.body);
    }
    assert!(has_isk(&reports.body, &moon4), "{}", reports.body);
    // 12,500 units of 10 m³, this month.
    let mining = page(&h, &format!("/plugins/{ID}/reports?_tab=1"), &owner).await;
    assert!(mining.body.contains("125,000"), "{}", mining.body);
    assert!(has_isk(&mining.body, "250,000,000"), "{}", mining.body);
    let prices = page(&h, &format!("/plugins/{ID}/reports?_tab=3"), &owner).await;
    assert!(prices.body.contains("Xenotime"), "{}", prices.body);

    // Blue, with basic_access only: Moons shows nothing; the rest is
    // closed.
    grant(&h, &owner, "basic_access", BLUE_STATE).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let theirs = page(&h, &format!("/plugins/{ID}/moons"), &blue).await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.body);
    assert!(
        theirs.body.contains("No moons to show you"),
        "{}",
        theirs.body
    );
    assert!(!theirs.body.contains("Jita IV - Moon 4"));
    for closed in [
        format!("/plugins/{ID}/upload"),
        format!("/plugins/{ID}/reports"),
        extraction.clone(),
        format!("/plugins/{ID}/totals"),
        format!("/plugins/{ID}/settings"),
        format!("/plugins/{ID}/moon/40009081"),
    ] {
        assert_eq!(
            page(&h, &closed, &blue).await.status,
            StatusCode::NOT_FOUND,
            "{closed}"
        );
    }
    let refused = upload_surveys(&h, &blue, &paste).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);

    // With upload_moon_scan: their own uploads, and only those.
    grant(&h, &owner, "upload_moon_scan", BLUE_STATE).await;
    let res = upload_surveys(
        &h,
        &blue,
        &format!("Jita IV - Moon 7\n\tZeolites\t0.4\t{ZEOLITES}\t{SYSTEM}\t40009077\t40009084\n"),
    )
    .await;
    assert!(res.body.contains("1 of 1 moons stored"), "{}", res.body);
    // Not someone else's survey (they don't see owned moons), and only
    // moon ores.
    let res = upload_surveys(
        &h,
        &blue,
        &format!(
            "Jita IV - Moon 4\n\tZeolites\t0.4\t{ZEOLITES}\t{SYSTEM}\t40009077\t40009081\n\
             Jita IV - Moon 5\n\tTritanium\t0.4\t34\t{SYSTEM}\t40009077\t40009082\n"
        ),
    )
    .await;
    assert!(res.body.contains("0 of 2 moons stored"), "{}", res.body);
    assert!(
        res.body.contains("someone else already surveyed this moon"),
        "{}",
        res.body
    );
    assert!(
        res.body.contains("ore type 34 isn&#39;t a moon ore"),
        "{}",
        res.body
    );
    // An owned moon nobody surveyed: stored, but its refinery isn't shown
    // to them.
    let res = upload_surveys(
        &h,
        &blue,
        &format!("Jita IV - Moon 5\n\tSylvite\t0.4\t{SYLVITE}\t{SYSTEM}\t40009077\t40009082\n"),
    )
    .await;
    assert!(res.body.contains("1 of 1 moons stored"), "{}", res.body);
    work(&h).await;
    let theirs = page(&h, &format!("/plugins/{ID}/moon/40009082"), &blue).await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.body);
    assert!(!theirs.body.contains("Jita - Drill Two"), "{}", theirs.body);
    assert!(!theirs.body.contains("Refinery"), "{}", theirs.body);
    let theirs = page(&h, &format!("/plugins/{ID}/moons"), &blue).await;
    assert!(theirs.body.contains("My Uploaded Moons"), "{}", theirs.body);
    assert!(!theirs.body.contains("Owned Moons"), "{}", theirs.body);
    assert!(theirs.body.contains("Jita IV - Moon 7"), "{}", theirs.body);
    assert!(theirs.body.contains("Jita IV - Moon 5"), "{}", theirs.body);
    assert!(!theirs.body.contains("Jita - Drill Two"), "{}", theirs.body);
    assert!(!theirs.body.contains("Jita IV - Moon 4"), "{}", theirs.body);
    // Nor does their search find moons by refinery or owner: that would
    // tell them which moons are ours.
    assert!(
        theirs
            .body
            .contains(r#"placeholder="Search moons, systems, regions""#),
        "{}",
        theirs.body
    );
    for q in ["Drill+Two", "Chribba"] {
        let found = page(&h, &format!("/plugins/{ID}/moons?_tab=0&q={q}"), &blue).await;
        assert!(
            !found.body.contains("Jita IV - Moon 5"),
            "{q}: {}",
            found.body
        );
    }
    let found = page(&h, &format!("/plugins/{ID}/moons?q=Moon+5"), &blue).await;
    assert!(found.body.contains("Jita IV - Moon 5"), "{}", found.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/moon/40009084"), &blue)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/moon/40009081"), &blue)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // With view_all_moons: every moon, but not the extractions behind them.
    grant(&h, &owner, "view_all_moons", BLUE_STATE).await;
    let all = page(&h, &format!("/plugins/{ID}/moon/40009081"), &blue).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert!(all.body.contains("Ore composition"));
    assert!(!all.body.contains("Chunk arrival"), "{}", all.body);

    // With extractions_access but not view_moon_ledgers: no ledger.
    grant(&h, &owner, "extractions_access", BLUE_STATE).await;
    let shown = page(&h, &extraction, &blue).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    assert!(shown.body.contains("Products (est.)"));
    assert!(
        !shown.body.contains("Totals by character"),
        "{}",
        shown.body
    );
}

/// Observers' ids from `FIRST_OBSERVER` on.
const FIRST_OBSERVER: i64 = 1030000100000;

/// Chribba Corp's observers, as ESI lists them (before `mount_esi`'s).
async fn list_observers(h: &Harness, ids: impl IntoIterator<Item = i64>) {
    let list: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            serde_json::json!({ "observer_id": id, "observer_type": "structure",
                                "last_updated": "2026-10-01" })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/observers"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(list),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

/// Any other observer's ledger: Chribba mined Sylvite today.
async fn mount_ledgers(h: &Harness) {
    let today = Utc::now().date_naive().to_string();
    Mock::given(method("GET"))
        .and(path_regex(format!(
            r"^/corporation/{CHRIBBA_CORP}/mining/observers/\d+$"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([
                    { "character_id": CHRIBBA, "last_updated": today, "quantity": 100,
                      "recorded_corporation_id": CHRIBBA_CORP, "type_id": SYLVITE },
                ])),
        )
        .with_priority(10)
        .mount(&h.esi_server)
        .await;
}

/// The ledgers asked of ESI so far, by observer.
async fn ledger_reads(h: &Harness) -> Vec<i64> {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|r| {
            let path = r.url.path().to_owned();
            let rest = path.strip_prefix("/corporation/")?;
            let (_, observer) = rest.split_once("/mining/observers/")?;
            observer.trim_end_matches('/').parse().ok()
        })
        .collect()
}

/// An observer whose ledger was last tried `minutes_ago` (never: `None`).
async fn seed_observer(h: &Harness, observer: i64, corporation: i64, minutes_ago: Option<i32>) {
    sqlx::query(
        r#"INSERT INTO "plugin_tether.moon-mining".observers (observer_id, corporation_id, synced_at)
           VALUES ($1, $2, now() - make_interval(mins => $3))"#,
    )
    .bind(observer)
    .bind(corporation)
    .bind(minutes_ago)
    .execute(&h.db)
    .await
    .unwrap();
}

async fn ledger_more_queued(h: &Harness) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'ledger_more' \
         AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// Observers whose ledger was tried in the last few minutes.
async fn tried_now(h: &Harness) -> Vec<i64> {
    sqlx::query_scalar(
        r#"SELECT observer_id FROM "plugin_tether.moon-mining".observers
           WHERE synced_at > now() - interval '5 minutes' ORDER BY observer_id"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn ledgers_read_every_listed_observer_hourly(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // More observers than one run's ESI calls.
    let listed: Vec<i64> = (0..100).map(|n| FIRST_OBSERVER + n).collect();
    list_observers(&h, listed.clone()).await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    mount_ledgers(&h).await;
    // One ESI no longer lists, and one of a corporation with no data
    // source.
    const GONE: i64 = 1030000999999;
    const OTHER: i64 = 1030000888888;
    seed_observer(&h, GONE, CHRIBBA_CORP, None).await;
    seed_observer(&h, OTHER, 98000001, None).await;
    // Adding the owner runs every schedule at once: the ledger run is
    // that one.
    approve_source(&h, &owner).await;
    work(&h).await;
    let first = ledger_reads(&h).await;
    assert!(!first.is_empty() && first.len() <= 81, "{}", first.len());
    assert_eq!(ledger_more_queued(&h).await, 1);
    let left: Vec<i64> = sqlx::query_scalar(
        r#"SELECT observer_id FROM "plugin_tether.moon-mining".observers
           WHERE observer_id = ANY($1) ORDER BY observer_id"#,
    )
    .bind(vec![GONE, OTHER])
    .fetch_all(&h.db)
    .await
    .unwrap();
    // Gone isn't read any more; the other corporation's waits for a
    // source.
    assert_eq!(left, [OTHER]);
    assert!(!first.contains(&OTHER) && !first.contains(&GONE));

    // A minute later the rest.
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'ledger_more'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(&h).await;
    assert_eq!(tried_now(&h).await, listed);
    let mut read = ledger_reads(&h).await;
    read.sort_unstable();
    read.dedup();
    assert_eq!(read, listed);
    assert_eq!(ledger_more_queued(&h).await, 0);
    let mined: i64 = sqlx::query_scalar(
        r#"SELECT count(DISTINCT observer_id) FROM "plugin_tether.moon-mining".ledger"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(mined, 100);
    assert!(warnings(&h).await.is_empty(), "{:?}", warnings(&h).await);

    // Within the hour: only the list is read.
    let before = ledger_reads(&h).await.len();
    run_schedule(&h, "ledger").await;
    assert_eq!(ledger_reads(&h).await.len(), before);

    // An hour on, read again.
    sqlx::query(
        r#"UPDATE "plugin_tether.moon-mining".observers SET synced_at = now() - interval '61 minutes'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    run_schedule(&h, "ledger").await;
    assert!(ledger_reads(&h).await.len() > before);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn refused_ledgers_neither_block_nor_loop(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let listed: Vec<i64> = (0..6).map(|n| FIRST_OBSERVER + n).collect();
    list_observers(&h, listed.clone()).await;
    // ESI refuses the first five's ledgers (the in-game role gone, say).
    for observer in &listed[..5] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/corporation/{CHRIBBA_CORP}/mining/observers/{observer}"
            )))
            .respond_with(ResponseTemplate::new(403).set_body_json(
                serde_json::json!({ "error": "Character does not have required role(s)" }),
            ))
            .with_priority(1)
            .mount(&h.esi_server)
            .await;
    }
    mount_esi(&h).await;
    mount_prices(&h).await;
    mount_ledgers(&h).await;
    // The refused ones are oldest; the good one was read 55 minutes ago.
    for (hours, observer) in (1..=5).rev().zip(&listed[..5]) {
        seed_observer(&h, *observer, CHRIBBA_CORP, Some(hours * 60)).await;
    }
    let good = listed[5];
    seed_observer(&h, good, CHRIBBA_CORP, Some(55)).await;
    approve_source(&h, &owner).await;
    work(&h).await;
    // Four refusals end the run, before the good one, with no follow-up:
    // a refusing corporation can't loop every minute.
    assert_eq!(ledger_reads(&h).await, listed[..4]);
    assert_eq!(tried_now(&h).await, listed[..4]);
    assert_eq!(ledger_more_queued(&h).await, 0);

    // The next run: the fifth, then the good one. Refused ones went to
    // the back.
    run_due_now(&h, "ledger").await;
    assert_eq!(ledger_reads(&h).await[4..], [listed[4], good]);
    let mined: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.moon-mining".ledger WHERE observer_id = $1"#,
    )
    .bind(good)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(mined, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_ledger_too_big_to_store_doesnt_stop_the_rest(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let (big, good) = (FIRST_OBSERVER, FIRST_OBSERVER + 1);
    list_observers(&h, [big, good]).await;
    // More than a statement may carry (1 MiB).
    let today = Utc::now().date_naive().to_string();
    let rows: Vec<serde_json::Value> = (0..12_000)
        .map(|n| {
            serde_json::json!({ "character_id": 90000000 + n, "last_updated": today,
                                "quantity": 100, "recorded_corporation_id": CHRIBBA_CORP,
                                "type_id": SYLVITE })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/observers/{big}"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(rows),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    mount_ledgers(&h).await;
    seed_observer(&h, big, CHRIBBA_CORP, Some(120)).await;
    seed_observer(&h, good, CHRIBBA_CORP, Some(60)).await;
    approve_source(&h, &owner).await;
    work(&h).await;
    assert_eq!(ledger_reads(&h).await, [big, good]);
    // The big one is tried for this hour, and the next is stored.
    assert_eq!(tried_now(&h).await, [big, good]);
    let mined: Vec<i64> = sqlx::query_scalar(
        r#"SELECT DISTINCT observer_id FROM "plugin_tether.moon-mining".ledger"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(mined, [good]);
    let warned = warnings(&h).await;
    assert!(
        warned
            .iter()
            .any(|w| w.contains(&format!("observer {big}: its ledger couldn't be stored"))),
        "{warned:?}"
    );
}

/// A thousand pilots from `MANY` on, each an owner in a corporation of
/// its own (`MANY_CORP` on), whose one mining observer is `MANY_OBSERVER`
/// on.
const MANY: i64 = 91000000;
const MANY_CORP: i64 = 98100000;
const MANY_OBSERVER: i64 = 1030000300000;

/// /characters/affiliation: those pilots in their own corporations, the
/// rest as the fixture has them.
struct Affiliations(Vec<serde_json::Value>);

impl wiremock::Respond for Affiliations {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let ids: Vec<i64> = serde_json::from_slice(&request.body).unwrap();
        let items: Vec<serde_json::Value> = ids
            .into_iter()
            .filter_map(|id| {
                if (MANY..MANY + 1000).contains(&id) {
                    Some(serde_json::json!({ "character_id": id, "corporation_id": MANY_CORP + id - MANY }))
                } else {
                    self.0.iter().find(|v| v["character_id"] == id).cloned()
                }
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(items)
    }
}

/// A corporation's mining observers as ESI lists them: one, its id from
/// the corporation's.
struct OneObserverEach;

impl wiremock::Respond for OneObserverEach {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let corp: i64 = request
            .url
            .path()
            .split('/')
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(serde_json::json!([
                { "observer_id": MANY_OBSERVER + corp - MANY_CORP, "observer_type": "structure",
                  "last_updated": "2026-10-01" },
            ]))
    }
}

/// The corporations whose observers ESI was asked for, a time each.
async fn observer_lists(h: &Harness) -> Vec<i64> {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|r| {
            let path = r.url.path().to_owned();
            let rest = path.strip_prefix("/corporation/")?;
            rest.strip_suffix("/mining/observers")?.parse().ok()
        })
        .collect()
}

/// Runs the queued `ledger_more` now, until none is left.
async fn finish_ledgers(h: &Harness) {
    for _ in 0..5 {
        if ledger_more_queued(h).await == 0 {
            return;
        }
        sqlx::query(
            "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'ledger_more'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
        work(h).await;
    }
    panic!("ledger_more kept coming");
}

/// More owner corporations than one run's ESI calls: Chribba Corp and 90
/// more, each with one mining observer where Chribba mined, and nothing
/// else. Returns the corporations, in order.
async fn many_owners(h: &Harness) -> Vec<i64> {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/esi/characters_affiliation.json"
    ))
    .unwrap();
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(Affiliations(serde_json::from_str(&fixture).unwrap()))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let owner = log_in_owner(h, "196379789:Chribba").await;
    install(h, &owner).await;
    for empty in [
        r"^/corporation/\d+/mining/extractions$",
        r"^/corporations/\d+/structures$",
        r"^/corporations/\d+/roles$",
    ] {
        Mock::given(method("GET"))
            .and(path_regex(empty))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "1")
                    .set_body_json(serde_json::json!([])),
            )
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex(r"^/corporation/\d+/mining/observers$"))
        .respond_with(OneObserverEach)
        .mount(&h.esi_server)
        .await;
    let today = Utc::now().date_naive().to_string();
    Mock::given(method("GET"))
        .and(path_regex(r"^/corporation/\d+/mining/observers/\d+$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([
                    { "character_id": CHRIBBA, "last_updated": today, "quantity": 100,
                      "recorded_corporation_id": CHRIBBA_CORP, "type_id": SYLVITE },
                ])),
        )
        .mount(&h.esi_server)
        .await;
    mount_prices(h).await;
    let mut session = approve_source(h, &owner).await;
    for n in 0..90 {
        session = add_source(h, &session, &format!("{}:Pilot{n}", MANY + n)).await;
    }
    let mut corporations: Vec<i64> = (0..90).map(|n| MANY_CORP + n).collect();
    corporations.push(CHRIBBA_CORP);
    corporations.sort_unstable();
    corporations
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_corporations_observers_are_listed_however_many(db: PgPool) {
    let h = harness(db, true).await;
    // Their observers were listed in the data sources' order from one
    // run's calls, so the last few's never were.
    let corporations = many_owners(&h).await;
    // Adding the owners ran every schedule: the ledger run is that one.
    work(&h).await;
    let first = observer_lists(&h).await;
    assert!(first.len() < corporations.len(), "{}", first.len());
    assert_eq!(ledger_more_queued(&h).await, 1);
    let waiting = format!(
        "{} corporations' mining observers wait for the next run",
        corporations.len() - first.len()
    );
    let logged: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level = 'info'",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(logged.iter().any(|m| m.starts_with(&waiting)), "{logged:?}");

    // The next runs list the rest, then read every ledger.
    finish_ledgers(&h).await;
    let mut listed = observer_lists(&h).await;
    listed.sort_unstable();
    assert_eq!(listed, corporations);
    let observers: Vec<i64> = corporations
        .iter()
        .map(|c| MANY_OBSERVER + c - MANY_CORP)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(tried_now(&h).await, observers);

    // The next hour starts with the longest unlisted: those the last
    // round reached last.
    let late: Vec<i64> = sqlx::query_scalar(
        r#"SELECT corporation_id FROM "plugin_tether.moon-mining".corporations
           ORDER BY listed_at DESC, corporation_id LIMIT $1"#,
    )
    .bind(i64::try_from(corporations.len() - first.len()).unwrap())
    .fetch_all(&h.db)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "plugin_tether.moon-mining".corporations SET listed_at = listed_at - interval '1 hour'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "plugin_tether.moon-mining".corporations SET listed_at = listed_at - interval '1 hour'
           WHERE corporation_id = ANY($1)"#,
    )
    .bind(&late)
    .execute(&h.db)
    .await
    .unwrap();
    run_due_now(&h, "ledger").await;
    let again = observer_lists(&h).await;
    for corp in &late {
        assert_eq!(again.iter().filter(|c| *c == corp).count(), 2, "{corp}");
    }
    assert!(warnings(&h).await.is_empty(), "{:?}", warnings(&h).await);
}

/// The app's jobs under `key`, oldest first: state and last error.
async fn jobs_keyed(h: &Harness, key: &str) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT state, last_error FROM core.jobs WHERE plugin_id = $1 AND job_key = $2 ORDER BY id",
    )
    .bind(ID)
    .bind(key)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// Runs the queued job under `key` now, and the other follow-up's later.
async fn run_keyed(h: &Harness, key: &str, other: &str) {
    sqlx::query(
        "UPDATE core.jobs SET run_at = CASE WHEN job_key = $2 THEN now() \
                                            ELSE now() + interval '1 hour' END \
         WHERE plugin_id = $1 AND job_key IN ($2, $3) AND state = 'queued'",
    )
    .bind(ID)
    .bind(key)
    .bind(other)
    .execute(&h.db)
    .await
    .unwrap();
    work(h).await;
}

/// Renames one of the app's tables, so what uses it fails.
async fn rename_table(h: &Harness, from: &str, to: &str) {
    // Both come from this file.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"ALTER TABLE "plugin_tether.moon-mining".{from} RENAME TO {to}"#
    )))
    .execute(&h.db)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn failed_follow_ups_keep_their_error(db: PgPool) {
    let h = harness(db, true).await;
    let corporations = many_owners(&h).await;
    // Adding the owners ran every schedule: the sync and the ledger run
    // each carry on a minute later.
    work(&h).await;
    assert_eq!(
        jobs_keyed(&h, "sync_more").await,
        [("queued".to_owned(), None)]
    );
    assert_eq!(
        jobs_keyed(&h, "ledger_more").await,
        [("queued".to_owned(), None)]
    );

    // A sync follow-up that queued the next and then failed (its places
    // can't be stored) ended at once as replaced, its error lost.
    rename_table(&h, "systems", "systems_away").await;
    run_keyed(&h, "sync_more", "ledger_more").await;
    let sync = jobs_keyed(&h, "sync_more").await;
    assert_eq!(sync.len(), 1, "{sync:?}");
    assert_eq!(sync[0].0, "queued");
    assert!(
        sync[0]
            .1
            .as_deref()
            .is_some_and(|e| e.contains("finding systems")),
        "{sync:?}"
    );
    // Tried again, it finishes the round.
    rename_table(&h, "systems_away", "systems").await;
    run_keyed(&h, "sync_more", "ledger_more").await;
    while sync_more_queued(&h).await > 0 {
        run_keyed(&h, "sync_more", "ledger_more").await;
    }
    let synced: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.moon-mining".corporations WHERE synced_at IS NOT NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(synced, i64::try_from(corporations.len()).unwrap());
    assert!(
        jobs_keyed(&h, "sync_more")
            .await
            .iter()
            .all(|(state, _)| state != "dead")
    );

    // The same for a ledger follow-up whose names can't be stored.
    rename_table(&h, "names", "names_away").await;
    run_keyed(&h, "ledger_more", "sync_more").await;
    let ledger = jobs_keyed(&h, "ledger_more").await;
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    assert_eq!(ledger[0].0, "queued");
    assert!(
        ledger[0]
            .1
            .as_deref()
            .is_some_and(|e| e.contains("reading names")),
        "{ledger:?}"
    );
    rename_table(&h, "names_away", "names").await;
    finish_ledgers(&h).await;
    assert_eq!(tried_now(&h).await.len(), corporations.len());
    assert!(
        jobs_keyed(&h, "ledger_more")
            .await
            .iter()
            .all(|(state, _)| state != "dead")
    );
}

/// /universe/names fails while `fails` is above zero, a time each call.
struct NamesFail(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl wiremock::Match for NamesFail {
    fn matches(&self, _: &wiremock::Request) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst) > 0
    }
}

impl wiremock::Respond for NamesFail {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": "Bad request" }))
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_owner_added_notice_waits_for_its_corporations_name(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // Before mount_esi's names, which it overrides while failing.
    let fails = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .and(NamesFail(fails.clone()))
        .respond_with(NamesFail(fails.clone()))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    approve_source(&h, &owner).await;
    // ESI names nothing in the first sync, which tries twice: the notice
    // went out as "corporation 1164409536", and never again.
    fails.store(2, std::sync::atomic::Ordering::SeqCst);
    work(&h).await;
    assert_eq!(fails.load(std::sync::atomic::Ordering::SeqCst), 0);
    let superuser = account_of(&h, CHRIBBA).await;
    assert!(notices(&h, superuser).await.is_empty());
    let told: bool =
        sqlx::query_scalar(r#"SELECT announced FROM "plugin_tether.moon-mining".sources"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(!told);
    // The next sync names it, then tells.
    run_due_now(&h, "sync").await;
    assert_eq!(
        notices(&h, superuser).await,
        ["Moon Mining: Owner added: Chribba Corp | Chribba Corp was added as a new owner."]
    );
}

/// gigX's corporation, a second owner's.
const GIGX: i64 = 1887431749;
const GIGX_CORP: i64 = 98133756;
const GIGX_DRILL: i64 = 1030000000020;

async fn sync_more_queued(h: &Harness) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'sync_more' \
         AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn has_structure(h: &Harness, structure: i64) -> bool {
    sqlx::query_scalar(
        r#"SELECT EXISTS (SELECT 1 FROM "plugin_tether.moon-mining".structures WHERE structure_id = $1)"#,
    )
    .bind(structure)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_corporation_is_read_however_many(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // Chribba Corp's structures need more ESI calls than a sync has, so
    // a corporation after it in the list was never read.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "100")
                .set_body_json(serde_json::json!([])),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let json = |value: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(value)
    };
    Mock::given(method("GET"))
        .and(path(format!("/corporation/{GIGX_CORP}/mining/extractions")))
        .respond_with(json(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{GIGX_CORP}/structures")))
        .respond_with(json(serde_json::json!([
            { "structure_id": GIGX_DRILL, "name": "Jita - Drill Three", "system_id": SYSTEM,
              "type_id": 35835, "corporation_id": GIGX_CORP, "profile_id": 1,
              "state": "shield_vulnerable", "services": [{ "name": "Moon Drilling", "state": "online" }] },
        ])))
        .mount(&h.esi_server)
        .await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    let owner = approve_source(&h, &owner).await;
    add_source(&h, &owner, &format!("{GIGX}:gigX")).await;
    // The sync adding the owner ran: it read Chribba Corp, ran out of
    // calls on its pages, and carries on a minute later.
    work(&h).await;
    assert!(!has_structure(&h, GIGX_DRILL).await);
    assert_eq!(sync_more_queued(&h).await, 1);
    let warned = warnings(&h).await;
    assert!(
        warned.iter().any(|w| w.contains(&format!(
            "corporation {CHRIBBA_CORP}: its structures have more pages than one run may read"
        ))),
        "{warned:?}"
    );
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'sync_more'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(&h).await;
    assert!(has_structure(&h, GIGX_DRILL).await);
    assert_eq!(sync_more_queued(&h).await, 0);
    // The next round starts with the longest unread.
    let read: Vec<i64> = sqlx::query_scalar(
        r#"SELECT corporation_id FROM "plugin_tether.moon-mining".corporations ORDER BY synced_at"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(read, [CHRIBBA_CORP, GIGX_CORP]);
}

/// A pilot's account, holding these of the app's permissions.
async fn pilot_with(h: &Harness, character: &str, permissions: &[&str]) -> i64 {
    log_in_as(h, character, None).await;
    let id: i64 = character.split(':').next().unwrap().parse().unwrap();
    let account = account_of(h, id).await;
    for p in permissions {
        sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
            .bind(format!("plugin.{ID}.{p}"))
            .bind(account)
            .execute(&h.db)
            .await
            .unwrap();
    }
    account
}

/// ESI refuses Chribba Corp's structures (the Station Manager role gone)
/// for the next `times` reads.
async fn refuse_structures(h: &Harness, times: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            serde_json::json!({ "error": "Character does not have required role(s)" }),
        ))
        .up_to_n_times(times)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admin_notices_follow_aa_moonmining(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    // On for a new install, as aa-moonmining's default.
    let on: bool = sqlx::query_scalar(
        r#"SELECT admin_notifications FROM "plugin_tether.moon-mining".settings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(on);
    let manager = pilot_with(&h, "90000002:Manager", &["manage"]).await;
    let miner = pilot_with(&h, "90000003:Miner", &["extractions_access"]).await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    let owner = approve_source(&h, &owner).await;
    work(&h).await;
    let superuser = account_of(&h, CHRIBBA).await;
    // The corporation, never the character: holders of `manage` don't see
    // who the data sources are.
    let added = "Moon Mining: Owner added: Chribba Corp | Chribba Corp was added as a new owner.";
    assert_eq!(notices(&h, superuser).await, [added]);
    assert_eq!(notices(&h, manager).await, [added]);
    assert!(notices(&h, miner).await.is_empty());

    // ESI refuses the owner's refineries: told once, as danger.
    refuse_structures(&h, 2).await;
    run_due_now(&h, "sync").await;
    let refused = "Moon Mining: Owner can't be read: Chribba Corp | Moon Mining can no longer \
                   read Chribba Corp's refineries: ESI refused (403): the character lacks an \
                   in-game role or a scope. It keeps trying at each sync; an app admin can check \
                   the owner on its Data sources page.";
    assert_eq!(notices(&h, manager).await, [added, refused]);
    let level: String = sqlx::query_scalar(
        "SELECT level FROM core.notifications WHERE account_id = $1 AND plugin_id = $2 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(manager)
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(level, "danger");
    run_due_now(&h, "sync").await;
    assert_eq!(notices(&h, manager).await, [added, refused]);
    // A read that works ends the streak.
    run_due_now(&h, "sync").await;
    let failing: Option<String> = sqlx::query_scalar(
        r#"SELECT failing_since::text FROM "plugin_tether.moon-mining".sources"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(failing, None);
    // Refused again: told again (the first read, as the host doesn't
    // repeat an unread notice).
    sqlx::query("UPDATE core.notifications SET read_at = now()")
        .execute(&h.db)
        .await
        .unwrap();
    refuse_structures(&h, 1).await;
    run_due_now(&h, "sync").await;
    assert_eq!(notices(&h, manager).await, [added, refused, refused]);

    // Turned off in Settings: nothing more.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&fresh_hours=0&ping_channel=&volume_per_day=960400\
             &days_per_month=30.4&stale_hours=12&old_moons_shown=5",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    run_due_now(&h, "sync").await;
    sqlx::query("UPDATE core.notifications SET read_at = now()")
        .execute(&h.db)
        .await
        .unwrap();
    refuse_structures(&h, 1).await;
    run_due_now(&h, "sync").await;
    assert_eq!(notices(&h, manager).await, [added, refused, refused]);
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings
            .body
            .contains("Tell admins when an owner is added or can&#39;t be read"),
        "{}",
        settings.body
    );
}

/// The app's migrations run into an empty schema, with `seed` run before
/// the last (data only an owner brings in): the admin notices and whether
/// the owners were recorded.
async fn migrated(db: &PgPool, schema: &str, seed: Option<&str>) -> (bool, bool) {
    // Its own connection, closed after: the search path is changed.
    let mut conn = db.acquire().await.unwrap().detach();
    // `schema` comes from this file.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        r#"CREATE SCHEMA "{schema}"; SET search_path = "{schema}""#
    )))
    .execute(&mut conn)
    .await
    .unwrap();
    let files = [
        "0001_moon_mining.sql",
        "0002_surveys_and_prices.sql",
        "0003_tether_rules_optional.sql",
        "0004_old_moons_shown.sql",
        "0005_refinery_drills.sql",
        "0006_corporation_reads.sql",
    ];
    for file in files {
        sqlx::raw_sql(sqlx::AssertSqlSafe(plugin_file(&format!(
            "migrations/{file}"
        ))))
        .execute(&mut conn)
        .await
        .unwrap();
    }
    if let Some(seed) = seed {
        // `seed` comes from this file.
        sqlx::raw_sql(sqlx::AssertSqlSafe(seed))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(plugin_file(
        "migrations/0007_admin_notifications.sql",
    )))
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query_as("SELECT admin_notifications, sources_known FROM settings")
        .fetch_one(&mut conn)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn existing_installs_keep_no_admin_notices(db: PgPool) {
    // An install already in use keeps none until a manager turns them on;
    // a new one has them, as aa-moonmining.
    let in_use = [
        "INSERT INTO structures (structure_id, corporation_id, name) VALUES (1, 2, 'Drill')",
        // Owners whose corporations have no refineries: only the daily
        // roles read shows them.
        "INSERT INTO station_managers (character_id, corporation_id) VALUES (3, 2)",
        // Mining kept from observers ESI no longer lists.
        "INSERT INTO ledger (observer_id, character_id, type_id, day, corporation_id, quantity) \
         VALUES (1, 3, 45491, '2026-10-01', 2, 100)",
    ];
    for (n, seed) in in_use.into_iter().enumerate() {
        assert_eq!(
            migrated(&db, &format!("in_use_{n}"), Some(seed)).await,
            (false, false),
            "{seed}"
        );
    }
    assert_eq!(migrated(&db, "fresh", None).await, (true, true));

    // An upgraded install whose manager turned them on before its first
    // sync: the owners in use then aren't news.
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    approve_source(&h, &owner).await;
    sqlx::query(
        r#"UPDATE "plugin_tether.moon-mining".settings SET admin_notifications = true,
           sources_known = false"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    work(&h).await;
    assert!(notices(&h, account_of(&h, CHRIBBA).await).await.is_empty());
    let told: bool =
        sqlx::query_scalar(r#"SELECT announced FROM "plugin_tether.moon-mining".sources"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(told);
}

/// Seeds an extraction at `structure` whose chunk arrived `arrived_ago`
/// (three hours before its auto-fracture): its details link.
async fn seed_extraction(h: &Harness, structure: i64, arrived_ago: Duration) -> String {
    let arrival = Utc::now() - arrived_ago;
    let arrival = chrono::DateTime::from_timestamp(arrival.timestamp(), 0).unwrap();
    sqlx::query(
        r#"INSERT INTO "plugin_tether.moon-mining".extractions
           (structure_id, chunk_arrival, moon_id, corporation_id, extraction_start, natural_decay)
           VALUES ($1, $2, 40009082, $3, $2 - interval '7 days', $2 + interval '3 hours')"#,
    )
    .bind(structure)
    .bind(arrival)
    .bind(CHRIBBA_CORP)
    .execute(&h.db)
    .await
    .unwrap();
    format!(
        "href=\"/plugins/{ID}/extraction/{structure}/{}\"",
        arrival.timestamp()
    )
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn upcoming_until_twelve_hours_after_auto_fracture(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    mount_prices(&h).await;
    let owner = approve_source(&h, &owner).await;
    run_schedule(&h, "sync").await;
    // aa-moonmining's rule: Upcoming until 12 hours after the automatic
    // fracture, then Past. Fractured 5 hours ago (arrived 8 hours ago):
    // still Upcoming, its chunk in space. Fractured 13 hours ago: Past.
    let recent = seed_extraction(&h, TATARA, Duration::hours(8)).await;
    let stale = seed_extraction(&h, TATARA, Duration::hours(16)).await;
    let upcoming = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(upcoming.body.contains(&recent), "{}", upcoming.body);
    assert!(!upcoming.body.contains(&stale), "{}", upcoming.body);
    assert!(upcoming.body.contains("Completed"), "{}", upcoming.body);
    let past = page(&h, &format!("/plugins/{ID}?_tab=1"), &owner).await;
    assert!(past.body.contains(&stale), "{}", past.body);
    assert!(!past.body.contains(&recent), "{}", past.body);
    // The setting's label says where its hours start.
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings
            .body
            .contains("Hours after auto-fracture until an extraction is Past"),
        "{}",
        settings.body
    );
}
