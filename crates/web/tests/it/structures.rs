//! The Structures plugin end to end: installed from its real component
//! and migrations, fed by mocked ESI through an approved owner (data
//! source), listing structures with fuel, relaying notifications (by
//! type, pinging by severity) and aa-structures' fuel alerts to Discord
//! once, seen by permission (unanchoring by its own), rotating an owner's
//! sync characters, and backing off an owner ESI answers 403 for.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.structures";
const CHRIBBA: i64 = 196379789;
const CHRIBBA_CORP: i64 = 1164409536;
const GIGX: i64 = 1887431749;
const GIGX_CORP: i64 = 98133756;
const KEEP: i64 = 1035466617946;
const DRILL: i64 = 1035466617947;
const SYSTEM: i64 = 30000142;
const ATTACKER: i64 = 2112625428;
const METENOX: i64 = 1035466617948;
const TOWER: i64 = 1_001_000_000_001;
const SMALL_TOWER: i64 = 1_001_000_000_002;
const POCO: i64 = 1_001_000_000_010;
const SKYHOOK: i64 = 1_001_000_000_020;
const PLANET: i64 = 40009077;
const OTHER_PLANET: i64 = 40009078;
const MOON: i64 = 40009081;
const OTHER_MOON: i64 = 40009082;
const ALLIANCE: i64 = 159826257;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("structures")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/structures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// The plugin's migrations, in order.
const MIGRATIONS: [&str; 7] = [
    "migrations/0001_structures.sql",
    "migrations/0002_timers_corporation_only.sql",
    "migrations/0003_starbases_orbitals_tags.sql",
    "migrations/0004_aa_routing_fuel_alerts_sync.sql",
    "migrations/0005_all_notification_types.sql",
    "migrations/0006_outbox_cards.sql",
    "migrations/0007_aa_defaults.sql",
];

/// The real package, signed with a test key.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migrations: Vec<(&str, String)> = MIGRATIONS
        .iter()
        .map(|name| (*name, plugin_file(name)))
        .collect();
    let component = component();
    let mut files: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", component.as_slice()),
    ];
    files.extend(migrations.iter().map(|(name, sql)| (*name, sql.as_bytes())));
    let bytes = testing::zip(&files);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

fn rfc(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn json(value: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("x-pages", "1")
        .set_body_json(value)
}

/// One notification as ESI sends it.
fn notification(id: i64, kind: &str, at: DateTime<Utc>, text: &str) -> serde_json::Value {
    serde_json::json!({
        "notification_id": id,
        "sender_id": CHRIBBA_CORP,
        "sender_type": "corporation",
        "text": text,
        "timestamp": rfc(at),
        "type": kind,
        "is_read": false,
    })
}

fn attack_text() -> String {
    format!(
        "allianceID: 99005338\nallianceName: Pandemic Horde\narmorPercentage: 100.0\n\
         charID: {ATTACKER}\ncorpName: Horde Vanguard.\nhullPercentage: 100.0\n\
         shieldPercentage: 42.5\nsolarsystemID: {SYSTEM}\nstructureID: &id001 {KEEP}\n\
         structureShowInfoData:\n- showinfo\n- 35832\n- *id001\nstructureTypeID: 35832\n"
    )
}

/// The Keep's armor timer ends a day after it lost its shields.
fn shields_text() -> String {
    format!(
        "solarsystemID: {SYSTEM}\nstructureID: &id001 {KEEP}\nstructureShowInfoData:\n\
         - showinfo\n- 35832\n- *id001\nstructureTypeID: 35832\ntimeLeft: 864000000000\n\
         vulnerableTime: 9000000000\n"
    )
}

fn moon_text() -> String {
    format!(
        "autoTime: 133090956000000000\nmoonID: 40009081\n\
         moonLink: <a href=\"showinfo:14//40009081\">Jita IV - Moon 4</a>\n\
         oreVolumeByType:\n  46676: 1000000.0\nreadyTime: 133090848000000000\n\
         solarSystemID: {SYSTEM}\nstartedBy: {CHRIBBA}\nstructureID: {DRILL}\n\
         structureName: Jita - Drill\nstructureTypeID: 35835\n"
    )
}

struct Times {
    attacked: DateTime<Utc>,
    shields: DateTime<Utc>,
}

/// Starbases, customs offices, assets and sovereignty: none, unless a
/// test mounts its own first (at a higher priority).
async fn mount_nothing_else(h: &Harness) {
    for path_ in [
        format!("/corporations/{CHRIBBA_CORP}/starbases"),
        format!("/corporations/{CHRIBBA_CORP}/customs_offices"),
        format!("/corporations/{CHRIBBA_CORP}/assets"),
    ] {
        Mock::given(method("GET"))
            .and(path(path_))
            .respond_with(json(serde_json::json!([])))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/sovereignty/systems"))
        .respond_with(json(serde_json::json!({ "solar_systems": [] })))
        .mount(&h.esi_server)
        .await;
}

/// Structures, systems and names; notifications are mounted by each test.
async fn mount_esi(h: &Harness, now: DateTime<Utc>, times: &Times) {
    // The drill's moon, named for its messages.
    Mock::given(method("GET"))
        .and(path(format!("/universe/moons/{MOON}")))
        .respond_with(json(serde_json::json!({
            "moon_id": MOON, "name": "Jita IV - Moon 4", "system_id": SYSTEM,
            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(json(serde_json::json!([
            {
                "structure_id": KEEP, "name": "Jita - Keep", "corporation_id": CHRIBBA_CORP,
                "type_id": 35832, "system_id": SYSTEM, "profile_id": 1,
                "fuel_expires": rfc(now + Duration::hours(5)),
                "services": [
                    { "name": "Clone Bay", "state": "offline" },
                    { "name": "Market", "state": "online" },
                ],
                "state": "armor_reinforce",
                "state_timer_start": rfc(times.shields),
                // Within seconds of the notification's timer: one timer.
                "state_timer_end": rfc(times.shields + Duration::days(1) + Duration::seconds(30)),
                "reinforce_hour": 19,
            },
            {
                "structure_id": DRILL, "name": "Jita - Drill", "corporation_id": CHRIBBA_CORP,
                "type_id": 35835, "system_id": SYSTEM, "profile_id": 1,
                "fuel_expires": rfc(now + Duration::days(30)),
                "state": "shield_vulnerable",
                "reinforce_hour": 20,
            },
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
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(json(serde_json::json!([
            { "id": SYSTEM, "name": "Jita", "category": "solar_system" },
            { "id": 10000002, "name": "The Forge", "category": "region" },
            { "id": 35832, "name": "Astrahus", "category": "inventory_type" },
            { "id": 35835, "name": "Athanor", "category": "inventory_type" },
            { "id": CHRIBBA_CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": 159826257, "name": "Otherworld Empire", "category": "alliance" },
            { "id": ATTACKER, "name": "Some Pilot", "category": "character" },
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": 16213, "name": "Caldari Control Tower", "category": "inventory_type" },
            { "id": 20062, "name": "Caldari Control Tower Small", "category": "inventory_type" },
            { "id": 4051, "name": "Caldari Fuel Block", "category": "inventory_type" },
            { "id": 16275, "name": "Strontium Clathrates", "category": "inventory_type" },
            { "id": 81826, "name": "Metenox Moon Drill", "category": "inventory_type" },
            { "id": 81143, "name": "Magmatic Gas", "category": "inventory_type" },
            { "id": 2233, "name": "Customs Office", "category": "inventory_type" },
            { "id": 81080, "name": "Orbital Skyhook", "category": "inventory_type" },
            { "id": 35949, "name": "Standup Heavy Energy Neutralizer I", "category": "inventory_type" },
            { "id": 56202, "name": "Astrahus Upwell Quantum Core", "category": "inventory_type" },
        ])))
        // Before the harness's own names fixture.
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_nothing_else(h).await;
}

/// Offers Chribba as a structure owner (the SSO round trip) and approves
/// it.
async fn approve_owner(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(asked.contains(&"esi-characters.read_notifications.v1".to_owned()));
    assert!(asked.contains(&"esi-corporations.read_structures.v1".to_owned()));
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{CHRIBBA}:Chribba&state={state}"),
            &[(LOGIN, &login), (SESSION, owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // In use at once (AA's Add Owner): nobody approves it.
    res.cookie_value(SESSION)
}

async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// Runs the sync schedule now; returns the plugin's warnings and errors.
async fn sync(h: &Harness) -> Vec<String> {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:sync"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
    assets_follow_ups(h).await;
    sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// The assets read again while Tether reads a corporation's assets in
/// the background (`assets_again`, a minute on in the app): run as soon
/// as there's one, until none is left.
async fn assets_follow_ups(h: &Harness) {
    for _ in 0..500 {
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'assets_again' \
             AND state = 'queued'",
        )
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap();
        if queued == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        sqlx::query(
            "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 \
             AND job_key = 'assets_again' AND state = 'queued'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
        work(h).await;
    }
    panic!("the assets were never read: {}", backlog(h).await);
}

/// The app's jobs and its unsent messages, for an assertion's message:
/// what didn't run or send when a count comes up short.
async fn backlog(h: &Harness) -> String {
    let jobs: Vec<String> = sqlx::query_scalar(
        "SELECT concat_ws(' · ', payload->>'name', state, attempts, last_error, \
                'runs in ' || to_char(run_at - now(), 'HH24:MI:SS')) FROM core.jobs \
         WHERE plugin_id = $1 AND state <> 'succeeded' ORDER BY id",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    let unsent: Vec<(String, Option<String>)> = sqlx::query_as(
        r#"SELECT message, failed FROM "plugin_tether.structures".outbox
           WHERE sent_at IS NULL ORDER BY id"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    let logs: Vec<(String, String)> = sqlx::query_as(
        "SELECT level, message FROM core.plugin_logs WHERE plugin_id = $1 \
         AND level IN ('info', 'warn', 'error') ORDER BY id DESC LIMIT 20",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    format!("jobs not done: {jobs:?}\nunsent: {unsent:?}\nlatest logs: {logs:?}")
}

async fn discord_messages(h: &Harness) -> Vec<String> {
    h.discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .map(|r| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            let content = body["content"].as_str().unwrap().to_owned();
            // A card reads as its message did, "Headline: the rest", but
            // for the rest starting with a capital.
            match body["embeds"].get(0) {
                Some(card) => {
                    let text = format!(
                        "{}: {}",
                        card["title"].as_str().unwrap(),
                        card["description"].as_str().unwrap_or_default()
                    );
                    if content.is_empty() {
                        text
                    } else {
                        format!("{content} {text}")
                    }
                }
                None => content,
            }
        })
        .collect()
}

/// The cards posted, in order.
async fn discord_cards(h: &Harness) -> Vec<serde_json::Value> {
    h.discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .map(|r| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            body["embeds"][0].clone()
        })
        .collect()
}

/// How often ESI was asked for a path.
async fn reads(h: &Harness, path: &str) -> usize {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == path)
        .count()
}

async fn count(h: &Harness, table: &str) -> i64 {
    let sql = match table {
        "timers" => r#"SELECT count(*) FROM "plugin_tether.structures".timers"#,
        "notifications" => r#"SELECT count(*) FROM "plugin_tether.structures".notifications"#,
        other => panic!("{other}"),
    };
    sqlx::query_scalar(sql).fetch_one(&h.db).await.unwrap()
}

/// The fuel alert configs: (start, end) hours.
async fn fuel_configs(h: &Harness) -> Vec<(i32, i32)> {
    sqlx::query_as(
        r#"SELECT start_hours, end_hours FROM "plugin_tether.structures".fuel_alert_configs ORDER BY start_hours DESC"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// Adds a fuel alert (aa-structures' fuel alert config) from the settings
/// page's form: once, pinging nobody.
async fn add_fuel_alert(h: &Harness, token: &str, start: i32, end: i32) {
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!(
                "_form=add_fuel_alert&start_hours={start}&end_hours={end}&repeat_hours=0&ping=none"
            ),
            token,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

/// The default notification types, sorted (none: every type).
async fn default_types(h: &Harness) -> Option<Vec<String>> {
    sqlx::query_scalar(
        r#"SELECT (SELECT array_agg(t ORDER BY t) FROM unnest(notification_types) t)
           FROM "plugin_tether.structures".settings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// EVE's own fuel alert for the Keep.
fn fuel_text() -> String {
    format!(
        "listOfTypesAndQty:\n- - 307\n  - 4246\nsolarsystemID: {SYSTEM}\n\
         structureID: &id001 {KEEP}\nstructureShowInfoData:\n- showinfo\n- 35832\n- *id001\n\
         structureTypeID: 35832\n"
    )
}

/// Structures' notices in an account's notifications: "title | message".
async fn notices(h: &Harness, character: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT n.title || ' | ' || n.message FROM core.notifications n \
         JOIN core.characters c ON c.account_id = n.account_id \
         WHERE c.id = $1 AND n.plugin_id = $2 ORDER BY n.id",
    )
    .bind(character)
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// The role mention a pinged message starts with.
fn member_ping() -> String {
    format!("<@&{DISCORD_MEMBER_ROLE}>")
}

async fn grant(h: &Harness, owner: &str, permission: &str) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{MEMBER_STATE}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn structures_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, GIGX_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    // First read: an attack, the shields going, a moon drill, an attack
    // days ago (stored, not sent), and a corporation application the host
    // never passes on.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            notification(1001, "StructureUnderAttack", times.attacked, &attack_text()),
            notification(1002, "StructureLostShields", times.shields, &shields_text()),
            notification(
                1003,
                "MoonminingExtractionStarted",
                now - Duration::minutes(2),
                &moon_text()
            ),
            notification(
                1000,
                "StructureUnderAttack",
                now - Duration::days(3),
                &attack_text()
            ),
            notification(
                999,
                "CorpAppNewMsg",
                now - Duration::minutes(1),
                "applicationText: hi\ncharID: 1\ncorpID: 2\n"
            ),
            // A type newer than Tether's ESI client: read, then dropped.
            notification(
                997,
                "StructureSomethingNew",
                now - Duration::minutes(1),
                "structureID: 1\n"
            ),
            // A structure not of this corporation (one the owner character
            // left, say): kept, never sent.
            notification(
                1004,
                "StructureUnderAttack",
                now - Duration::minutes(3),
                &attack_text().replace(&KEEP.to_string(), "1035466619999")
            ),
            // EVE's own fuel alert, last.
            notification(
                1005,
                "StructureFuelAlert",
                now - Duration::seconds(30),
                &fuel_text()
            ),
        ])))
        .up_to_n_times(1)
        .mount(&h.esi_server)
        .await;
    // Later reads: the same, plus the attack as another id (another owner
    // character would see it so).
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            notification(1001, "StructureUnderAttack", times.attacked, &attack_text()),
            notification(1002, "StructureLostShields", times.shields, &shields_text()),
            notification(2001, "StructureUnderAttack", times.attacked, &attack_text()),
        ])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;

    // Discord: the plugin gets the ping channel; everything goes there.
    // A fresh install starts as aa-structures: default pings on (danger and
    // warning notifications mention Member's role), its default types, no
    // fuel alert configs.
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
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(settings.body.contains("#fleet-pings"), "{}", settings.body);
    let c = DISCORD_PING_CHANNEL;
    let bad = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=add_fuel_alert&start_hours=6&end_hours=6&repeat_hours=0&ping=none",
            &owner,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::OK, "{}", bad.body);
    assert!(
        bad.body.contains("End must be less than its Start"),
        "{}",
        bad.body
    );
    assert!(settings.body.contains("Fuel alerts"), "{}", settings.body);
    assert!(fuel_configs(&h).await.is_empty());
    let fresh = form_body(&settings.body, "settings", &[]);
    for on in [
        "default_pings=on",
        "danger_ping=Member",
        "warning_ping=Member",
        "t_structurefuelalert=on",
        "t_structureunderattack=on",
        "moon_extraction_timers=on",
        "show_jump_gates=on",
        "admin_notifications=on",
    ] {
        assert!(fresh.split('&').any(|p| p == on), "{on}: {fresh}");
    }
    for off in [
        "t_structureunanchoring=",
        "t_moonminingextractionstarted=",
        "timers_corporation_only=",
    ] {
        assert!(!fresh.contains(off), "{off}: {fresh}");
    }
    let mut defaults: Vec<String> = [
        "OrbitalAttacked",
        "OrbitalReinforced",
        "SkyhookDestroyed",
        "SkyhookLostShields",
        "SkyhookOnline",
        "SkyhookUnderAttack",
        "SovStructureDestroyed",
        "SovStructureReinforced",
        "StructureAnchoring",
        "StructureDestroyed",
        "StructureFuelAlert",
        "StructureLostArmor",
        "StructureLostShields",
        "StructureLowReagentsAlert",
        "StructureNoReagentsAlert",
        "StructureOnline",
        "StructureServicesOffline",
        "StructureUnderAttack",
        "StructureWentHighPower",
        "StructureWentLowPower",
        "TowerAlertMsg",
        "TowerResourceAlertMsg",
    ]
    .map(str::to_owned)
    .to_vec();
    defaults.sort();
    assert_eq!(default_types(&h).await, Some(defaults));
    // A new fuel alert pings the warning role, aa-structures' @here.
    assert!(
        form_body(&settings.body, "add_fuel_alert", &[]).contains("ping=warning"),
        "{}",
        settings.body
    );
    // The moon drills are ticked here, beside the defaults.
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", c),
            ("fuel_channel", c),
            ("state_channel", c),
            ("moon_channel", c),
            ("t_moonminingextractionstarted", "on"),
        ],
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

    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");

    // The structure list, with names, region, fuel, services and state.
    let list = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    for seen in [
        "Jita - Keep",
        "Jita - Drill",
        "Astrahus",
        "Athanor",
        "The Forge",
        "Jita (0.9)",
        "Otherworld Enterprises",
        "Clone Bay (offline), Market",
        "Armor reinforced",
        "Shield vulnerable",
        "19:00",
        "4h 5",
    ] {
        assert!(list.body.contains(seen), "{seen}: {}", list.body);
    }
    // The views and Manage, which Tether draws (Settings, for managers);
    // owners' logos, types' icons, and the reinforced Keep's timer
    // counting down. The tag settings open from Settings, as a chip.
    assert!(
        list.body
            .contains(&format!("href=\"/plugins/{ID}/settings\"")),
        "{}",
        list.body
    );
    assert!(
        !list
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings/tags\"")),
        "{}",
        list.body
    );
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings/tags\"")),
        "{}",
        settings.body
    );
    assert!(
        list.body.contains(&format!("href=\"/plugins/{ID}/pocos\"")),
        "{}",
        list.body
    );
    assert!(
        list.body.contains(&format!(
            "images.evetech.net/corporations/{CHRIBBA_CORP}/logo"
        )),
        "{}",
        list.body
    );
    assert!(
        list.body.contains("images.evetech.net/types/"),
        "{}",
        list.body
    );
    assert!(list.body.contains("data-countdown"), "{}", list.body);
    // Low fuel: the Keep only.
    let low = page(&h, &format!("/plugins/{ID}?_tab=1"), &owner).await;
    assert!(low.body.contains("Under 72 hours of fuel"), "{}", low.body);
    assert!(low.body.contains("Jita - Keep"), "{}", low.body);
    assert!(!low.body.contains("Jita - Drill"), "{}", low.body);
    // Timers: the Keep's armor timer.
    let timers = page(&h, &format!("/plugins/{ID}?_tab=3"), &owner).await;
    assert!(timers.body.contains("Upcoming timers"), "{}", timers.body);
    assert!(timers.body.contains("<td>Armor</td>"), "{}", timers.body);
    // One armor timer: the state's and the notification's are the same.
    assert_eq!(count(&h, "timers").await, 1);
    // The host passed on the types Structures relays only: not a type
    // newer than its ESI client, nor an application to a corporation
    // other than the owner's.
    assert_eq!(count(&h, "notifications").await, 6);
    let by_owner = page(&h, &format!("/plugins/{ID}/owner/{CHRIBBA_CORP}"), &owner).await;
    assert_eq!(by_owner.status, StatusCode::OK, "{}", by_owner.body);
    assert!(by_owner.body.contains("Structures: Otherworld Enterprises"));
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/owner/{GIGX_CORP}"), &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Discord: the attack (danger) and the shields with the timer
    // (warning), both mentioning Member's role by default, the moon drill
    // (info, no ping) and EVE's fuel alert (warning); not the old attack.
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert!(sent[0].starts_with(&member_ping()), "{sent:?}");
    assert!(
        sent[0].contains(
            "Under attack: Jita - Keep (Astrahus) in Jita by Some Pilot, Horde Vanguard., \
             Pandemic Horde. Shield 42%, armor 100%, hull 100%."
        ) || sent[0].contains("Shield 43%"),
        "{sent:?}"
    );
    let armor = (times.shields + Duration::days(1))
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert!(sent[1].contains("lost its shields"), "{sent:?}");
    assert!(sent[1].contains(&armor), "{sent:?}");
    assert!(sent[1].starts_with("<@&"), "{sent:?}");
    assert!(!sent[2].contains("<@&"), "{sent:?}");
    assert!(sent[2].contains("Jita IV - Moon 4"), "{sent:?}");
    // Each a card, as notification bots post them: the drill's green,
    // under its owner, with its render and a countdown to the chunk.
    let drill = &discord_cards(&h).await[2];
    assert_eq!(drill["title"], "Extraction started", "{drill}");
    assert_eq!(drill["color"], 0x2e_cc71, "{drill}");
    assert_eq!(
        drill["author"]["icon_url"],
        format!("https://images.evetech.net/corporations/{CHRIBBA_CORP}/logo?size=64"),
        "{drill}"
    );
    assert_eq!(
        drill["thumbnail"]["url"], "https://images.evetech.net/types/35835/render?size=128",
        "{drill}"
    );
    assert!(
        drill["description"]
            .as_str()
            .unwrap()
            .contains(" EVE · <t:"),
        "{drill}"
    );
    // The chunk's ore, as the notification gives it.
    assert!(
        drill["description"]
            .as_str()
            .unwrap()
            .contains("Total 1.0M m³. Ore: "),
        "{drill}"
    );
    assert!(
        drill["fields"].as_array().unwrap().contains(
            &serde_json::json!({ "name": "Moon", "value": "Jita IV - Moon 4", "inline": true })
        ),
        "{drill}"
    );
    assert_eq!(
        drill["footer"]["text"],
        "Structures · Moon extraction started"
    );
    // Low fuel once, by EVE's alert: no fuel alert config of Tether's
    // reports it again (aa-structures starts with none).
    assert!(sent[3].starts_with(&member_ping()), "{sent:?}");
    assert!(
        sent[3].contains("Fuel alert: Jita - Keep (Astrahus) in Jita is running low on fuel."),
        "{sent:?}"
    );
    assert!(sent.iter().all(|m| !m.contains("Low fuel")), "{sent:?}");
    // The owner's pings follow the default, on.
    let routing = page(
        &h,
        &format!("/plugins/{ID}/settings/owner/{CHRIBBA_CORP}"),
        &owner,
    )
    .await;
    assert!(routing.body.contains("Default (on)"), "{}", routing.body);

    // Read again: the same notifications, and the attack under another
    // id, send nothing more; nor does the fuel, still low.
    sqlx::query(r#"UPDATE "plugin_tether.structures".owners SET notifications_at = NULL"#)
        .execute(&h.db)
        .await
        .unwrap();
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        reads(&h, &format!("/characters/{CHRIBBA}/notifications")).await,
        2
    );
    assert_eq!(count(&h, "notifications").await, 6);
    assert_eq!(discord_messages(&h).await.len(), 4);
    // Structures were read under an hour ago: not again.
    assert_eq!(
        reads(&h, &format!("/corporations/{CHRIBBA_CORP}/structures")).await,
        1
    );
    // A fuel alert config of Tether's: the Keep is reported by it too, as
    // aa-structures does once one is set up.
    add_fuel_alert(&h, &owner, 6, 0).await;
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 5, "{sent:?}");
    assert!(
        sent[4].starts_with("Low fuel: Jita - Keep (Astrahus) in Jita"),
        "{sent:?}"
    );
    assert!(sent[4].contains("under the 6-hour alert"), "{sent:?}");
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(settings.body.contains("Sent"), "{}", settings.body);
    assert!(settings.body.contains("Chribba"), "{}", settings.body);

    // Permissions: a Member of another corporation sees nothing until
    // granted, then only what the view permission allows.
    let gigx = log_in_as(&h, "1887431749:gigX", None).await;
    let url = format!("/plugins/{ID}");
    assert_eq!(page(&h, &url, &gigx).await.status, StatusCode::NOT_FOUND);
    grant(&h, &owner, "basic_access").await;
    let seen = page(&h, &url, &gigx).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("may not see any structures"));
    grant(&h, &owner, "view_corporation_structures").await;
    let seen = page(&h, &url, &gigx).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(!seen.body.contains("Jita - Keep"), "{}", seen.body);
    // Not a manager: no links to the settings.
    assert!(
        seen.body.contains(&format!("href=\"/plugins/{ID}/pocos\"")),
        "{}",
        seen.body
    );
    assert!(
        !seen
            .body
            .contains(&format!("href=\"/plugins/{ID}/settings\"")),
        "{}",
        seen.body
    );
    assert!(
        !seen.body.contains("Otherworld Enterprises"),
        "{}",
        seen.body
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/owner/{CHRIBBA_CORP}"), &gigx)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    grant(&h, &owner, "view_all_structures").await;
    let seen = page(&h, &url, &gigx).await;
    assert!(seen.body.contains("Jita - Keep"), "{}", seen.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/settings"), &gigx)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!("_form=retry&owner={CHRIBBA}"),
            &gigx,
        ),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_owner_without_the_role_is_left_alone(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    // No Station Manager role: ESI refuses the structures.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            serde_json::json!({ "error": "Character does not have required role(s)" }),
        ))
        .mount(&h.esi_server)
        .await;
    mount_nothing_else(&h).await;
    let owner = approve_owner(&h, &owner).await;
    let structures = format!("/corporations/{CHRIBBA_CORP}/structures");

    let problems = sync(&h).await;
    assert!(
        problems
            .iter()
            .any(|p| p.contains("403") && p.contains("backing off")),
        "{problems:?}"
    );
    assert_eq!(reads(&h, &structures).await, 1);
    let backing_off: bool = sqlx::query_scalar(
        r#"SELECT structures_retry_at > now() + interval '50 minutes' FROM "plugin_tether.structures".owners WHERE character_id = $1"#,
    )
    .bind(CHRIBBA)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(backing_off);
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(settings.body.contains("Backing off"), "{}", settings.body);
    // Each owner's row has its Retry now.
    assert!(
        settings.body.contains(&format!(
            "name=\"_form\" value=\"retry\"><input type=\"hidden\" name=\"owner\" value=\"{CHRIBBA}\">"
        )),
        "{}",
        settings.body
    );

    // The next runs leave ESI alone (the notifications read still counts
    // as fresh, too).
    sync(&h).await;
    assert_eq!(reads(&h, &structures).await, 1);

    // Fixed in game: a manager retries it at once.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!("_form=retry&owner={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    assert_eq!(reads(&h, &structures).await, 2);

    // The host listing no owners for a moment keeps them.
    let res = send(
        &h.app,
        form(&format!("/apps/{ID}/owners/{CHRIBBA}/remove"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let problems = sync(&h).await;
    assert!(
        problems.iter().any(|p| p.contains("keeping them for now")),
        "{problems:?}"
    );
    let owners: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.structures".owners"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(owners, 1);
}

/// A channel the bot can't post in holds nothing up: Discord refusing it
/// is final, so the relay marks that message failed and sends the next,
/// to another channel, and no job dies. Before, the refusal came back as
/// Discord being down: every run stopped at that message, and nothing
/// went out to any channel until it expired a day later.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_channel_the_bot_cant_post_in_holds_nothing_up(db: PgPool) {
    const LOCKED: &str = "600000000000000002";
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    // The attack first, then the drill.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            notification(1001, "StructureUnderAttack", times.attacked, &attack_text()),
            notification(
                1003,
                "MoonminingExtractionStarted",
                now - Duration::minutes(2),
                &moon_text()
            ),
        ])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;

    // Attacks go to a channel the bot may not post in, with Member's
    // role; extractions to one it may.
    discord_ready(&h, &owner).await;
    let res = send(
        &h.app,
        form(
            "/admin/discord/channels",
            &format!("channel_id={LOCKED}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for channel in [DISCORD_PING_CHANNEL, LOCKED] {
        let res = send(
            &h.app,
            form(
                &format!("/admin/plugins/{ID}/channels"),
                &format!("channel_id={channel}"),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", LOCKED),
            ("fuel_channel", ""),
            ("state_channel", ""),
            ("moon_channel", DISCORD_PING_CHANNEL),
            ("default_pings", "on"),
            ("danger_ping", "Member"),
            // Not among aa-structures' default types: ticked here.
            ("t_moonminingextractionstarted", "on"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{LOCKED}/messages")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({ "code": 50013, "message": "Missing Permissions" }),
            ),
        )
        .with_priority(1)
        .mount(&h.discord_server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;

    let problems = sync(&h).await;
    assert!(
        problems.iter().any(|p| p.contains(
            "a Discord message wasn't sent: The bot can't post in that channel: give it View \
             Channel and Send Messages there"
        )),
        "{problems:?}"
    );
    // The attack's message failed, saying why; the drill's, queued after
    // it, went out.
    let outbox: Vec<(i64, String, bool, Option<String>)> = sqlx::query_as(
        r#"SELECT id, channel, sent_at IS NOT NULL, failed FROM "plugin_tether.structures".outbox
           ORDER BY id"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    let refused = outbox.iter().find(|m| m.1 == LOCKED).unwrap();
    let posted = outbox.iter().find(|m| m.1 == DISCORD_PING_CHANNEL).unwrap();
    assert!(refused.0 < posted.0, "{outbox:?}");
    assert!(
        !refused.2
            && refused
                .3
                .as_deref()
                .is_some_and(|f| f.contains("give it View Channel and Send Messages")),
        "{outbox:?}"
    );
    assert!(
        posted.2 && posted.3.is_none(),
        "{outbox:?}\n{}",
        backlog(&h).await
    );
    // Discord was asked once for the locked channel: the retry without
    // the mention got Tether's answer.
    async fn asked(h: &Harness, channel: &str) -> usize {
        let at = format!("/api/v10/channels/{channel}/messages");
        h.discord_server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path() == at)
            .count()
    }
    assert_eq!(asked(&h, LOCKED).await, 1);
    assert_eq!(asked(&h, DISCORD_PING_CHANNEL).await, 1);
    let dead: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND state = 'dead'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(dead, 0, "{}", backlog(&h).await);
}

// ---- Structure Timers: the timers Structures publishes ---------------------------

#[derive(Debug, sqlx::FromRow)]
struct Shared {
    key: String,
    title: String,
    system: String,
    details: String,
    objective: String,
    corporation_id: Option<i64>,
}

async fn shared_timers(h: &Harness) -> Vec<Shared> {
    sqlx::query_as(
        "SELECT key, title, system, details, objective, corporation_id FROM core.shared_timers \
         WHERE plugin_id = $1 ORDER BY key",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn structures_feed_structure_timers(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, GIGX_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    crate::structure_timers::install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    // The Keep lost its shields: an armor timer in a day.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([notification(
            1002,
            "StructureLostShields",
            times.shields,
            &shields_text()
        )])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");

    // Published: one armor timer, friendly, for everyone (the default).
    let shared = shared_timers(&h).await;
    assert_eq!(shared.len(), 1, "{shared:?}");
    assert_eq!(shared[0].key, format!("{KEEP}:armor"));
    assert_eq!(shared[0].title, "Jita - Keep: armor timer");
    assert_eq!(shared[0].system, "Jita");
    assert!(
        shared[0]
            .details
            .starts_with("Astrahus of Otherworld Enterprises"),
        "{shared:?}"
    );
    assert_eq!(shared[0].objective, "friendly");
    assert_eq!(shared[0].corporation_id, None);

    // Structure Timers shows it, marked automatic, with no Edit link.
    crate::structure_timers::grant(&h, &owner, "timer_view", MEMBER_STATE).await;
    let gigx = log_in_as(&h, "1887431749:gigX", None).await;
    let timers_url = "/plugins/tether.structure-timers";
    let seen = page(&h, timers_url, &gigx).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    for text in [
        "<td>Jita - Keep: armor timer</td>",
        "<td>Jita</td>",
        ">Automatic</span>",
        "<td>Structures</td>",
        ">Friendly</span>",
    ] {
        assert!(seen.body.contains(text), "{text}: {}", seen.body);
    }
    let managed = page(&h, timers_url, &owner).await;
    assert!(
        managed.body.contains("Jita - Keep: armor timer"),
        "{}",
        managed.body
    );
    assert!(!managed.body.contains("timer/0"), "{}", managed.body);

    // Corporation-only (aa-structures' STRUCTURES_TIMERS_ARE_CORP_RESTRICTED):
    // published again at once, and seen by the owning corporation alone.
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings.body.contains("Timers are corporation-only"),
        "{}",
        settings.body
    );
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", ""),
            ("fuel_channel", ""),
            ("state_channel", ""),
            ("moon_channel", ""),
            ("timers_corporation_only", "on"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    let shared = shared_timers(&h).await;
    assert_eq!(shared.len(), 1, "{shared:?}");
    assert_eq!(shared[0].corporation_id, Some(CHRIBBA_CORP));
    let seen = page(&h, timers_url, &gigx).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(!seen.body.contains("Jita - Keep"), "{}", seen.body);
    let managed = page(&h, timers_url, &owner).await;
    assert!(
        managed.body.contains("Jita - Keep: armor timer"),
        "{}",
        managed.body
    );
    assert!(
        managed.body.contains(">Automatic · Corporation</span>"),
        "{}",
        managed.body
    );
}

// ---- starbases, orbitals, fittings, Metenox, tags and per-owner routing ------

/// EVE's file time (100 ns ticks since 1601) for an instant.
fn filetime(t: DateTime<Utc>) -> i64 {
    (t.timestamp() + 11_644_473_600) * 10_000_000
}

/// What a Director owner reads beyond the Upwell structures: two
/// starbases, a customs office, a Metenox's fuel bay, the Keep's fitting
/// and a skyhook, with names, locations, planets, moons and sovereignty.
async fn mount_director_reads(h: &Harness, now: DateTime<Utc>) {
    let corp = CHRIBBA_CORP;
    // A priority above `mount_esi`'s: these win.
    let first = |m: Mock| m.with_priority(1);
    first(
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{corp}/structures")))
            .respond_with(json(serde_json::json!([
                {
                    "structure_id": KEEP, "name": "Jita - Keep", "corporation_id": corp,
                    "type_id": 35832, "system_id": SYSTEM, "profile_id": 1,
                    "fuel_expires": rfc(now + Duration::days(20)),
                    "state": "shield_vulnerable", "reinforce_hour": 19,
                },
                {
                    "structure_id": METENOX, "name": "Jita - Metenox", "corporation_id": corp,
                    "type_id": 81826, "system_id": SYSTEM, "profile_id": 1,
                    // ESI's fuel: blocks for a month. The gas lasts 12 hours.
                    "fuel_expires": rfc(now + Duration::days(30)),
                    "state": "shield_vulnerable",
                },
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{corp}/starbases")))
            .respond_with(json(serde_json::json!([
                {
                    "starbase_id": TOWER, "type_id": 16213, "system_id": SYSTEM, "moon_id": MOON,
                    "state": "reinforced", "reinforced_until": rfc(now + Duration::hours(30)),
                    "onlined_since": rfc(now - Duration::days(100)),
                },
                {
                    "starbase_id": SMALL_TOWER, "type_id": 20062, "system_id": SYSTEM,
                    "moon_id": OTHER_MOON, "state": "online",
                },
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    let detail = |fuels: serde_json::Value| {
        json(serde_json::json!({
            "allow_alliance_members": true, "allow_corporation_members": true,
            "anchor": "config_starbase_equipment_role", "attack_if_at_war": true,
            "attack_if_other_security_status_dropping": false,
            "fuel_bay_take": "config_starbase_equipment_role",
            "fuel_bay_view": "starbase_fuel_technician_role",
            "offline": "config_starbase_equipment_role", "online": "config_starbase_equipment_role",
            "unanchor": "config_starbase_equipment_role", "use_alliance_standings": true,
            "fuels": fuels,
        }))
    };
    // In its alliance's sov a large tower burns 30 blocks an hour: 960
    // last 32 hours.
    first(
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{corp}/starbases/{TOWER}")))
            .respond_with(detail(serde_json::json!([
                { "type_id": 4051, "quantity": 960 },
                { "type_id": 16275, "quantity": 400 },
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("GET"))
            .and(path(format!(
                "/corporations/{corp}/starbases/{SMALL_TOWER}"
            )))
            .respond_with(detail(
                serde_json::json!([{ "type_id": 4051, "quantity": 7200 }]),
            )),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{corp}/customs_offices")))
            .respond_with(json(serde_json::json!([{
                "office_id": POCO, "system_id": SYSTEM, "type_id": 2233,
                "reinforce_exit_start": 18, "reinforce_exit_end": 20,
                "corporation_tax_rate": 0.05, "alliance_tax_rate": 0.07,
                "allow_alliance_access": true, "allow_access_with_standings": false,
                "standing_level": "neutral", "neutral_standing_tax_rate": 0.1,
            }]))),
    )
    .mount(&h.esi_server)
    .await;
    let asset = |item: i64, type_id: i64, location: i64, flag: &str, kind: &str, quantity: i64| {
        serde_json::json!({
            "item_id": item, "type_id": type_id, "location_id": location, "location_flag": flag,
            "location_type": kind, "quantity": quantity, "is_singleton": true,
        })
    };
    first(
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{corp}/assets")))
            .respond_with(json(serde_json::json!([
                asset(1, 35949, KEEP, "HiSlot0", "item", 1),
                asset(2, 56202, KEEP, "QuantumCoreRoom", "item", 1),
                // The host keeps these away from the plugin.
                asset(3, 34, KEEP, "CorpSAG1", "item", 1000),
                asset(4, 34, 60003760, "Hangar", "station", 5000),
                // A flag newer than Tether's ESI client: the page is
                // read again, loosely.
                asset(5, 34, KEEP, "StructureDeedBay", "item", 1),
                // A corporation ship's fitting: the plugin keeps only
                // what's in its structures.
                asset(6, 35949, 1_001_999_000_000, "HiSlot0", "item", 1),
                asset(7, 4051, METENOX, "StructureFuel", "item", 1000),
                asset(8, 81143, METENOX, "StructureFuel", "item", 2400),
                asset(SKYHOOK, 81080, SYSTEM, "AutoFit", "solar_system", 1),
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("POST"))
            .and(path(format!("/corporations/{corp}/assets/names")))
            .respond_with(json(serde_json::json!([
                { "item_id": TOWER, "name": "Home Tower" },
                { "item_id": POCO, "name": "Customs Office (Jita IV)" },
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("POST"))
            .and(path(format!("/corporations/{corp}/assets/locations")))
            .respond_with(json(serde_json::json!([
                { "item_id": SKYHOOK, "position": { "x": 10.0, "y": 0.0, "z": 0.0 } },
            ]))),
    )
    .mount(&h.esi_server)
    .await;
    first(
        Mock::given(method("GET"))
            .and(path(format!("/universe/systems/{SYSTEM}")))
            .respond_with(json(serde_json::json!({
                "system_id": SYSTEM, "name": "Jita", "constellation_id": 20000020,
                "security_status": 0.9459, "position": { "x": 1.0, "y": 2.0, "z": 3.0 },
                "planets": [
                    { "planet_id": PLANET, "moons": [MOON, OTHER_MOON] },
                    { "planet_id": OTHER_PLANET },
                ],
            }))),
    )
    .mount(&h.esi_server)
    .await;
    for (planet, name, x) in [(PLANET, "Jita IV", 0.0), (OTHER_PLANET, "Jita V", 1.0e9)] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/planets/{planet}")))
            .respond_with(json(serde_json::json!({
                "planet_id": planet, "name": name, "system_id": SYSTEM, "type_id": 2016,
                "position": { "x": x, "y": 0.0, "z": 0.0 },
            })))
            .mount(&h.esi_server)
            .await;
    }
    for (moon, name) in [(MOON, "Jita IV - Moon 4"), (OTHER_MOON, "Jita IV - Moon 5")] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/moons/{moon}")))
            .respond_with(json(serde_json::json!({
                "moon_id": moon, "name": name, "system_id": SYSTEM,
                "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
            })))
            .mount(&h.esi_server)
            .await;
    }
    first(
        Mock::given(method("GET"))
            .and(path("/sovereignty/systems"))
            .respond_with(json(serde_json::json!({ "solar_systems": [
                { "solar_system_id": SYSTEM, "claim": { "alliance": {
                    "alliance_id": ALLIANCE, "corporation_id": corp,
                    "claimed_since": "2026-01-01T00:00:00Z", "is_capital_system": false,
                    "sovereignty_hub": { "id": 1 },
                    "development": { "activity_defense_multiplier": 1.0, "industrial_level": 0,
                        "military_level": 0, "strategic_level": 0 },
                } } },
                { "solar_system_id": 30000001, "claim": { "unclaimed": true } },
            ] }))),
    )
    .mount(&h.esi_server)
    .await;
}

async fn structure_items(h: &Harness) -> Vec<(i64, String)> {
    sqlx::query_as(
        r#"SELECT item_id, flag FROM "plugin_tether.structures".structure_items ORDER BY item_id"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn starbases_orbitals_fittings_tags_and_owner_routing(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, GIGX_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    crate::structure_timers::install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_director_reads(&h, now).await;
    mount_esi(&h, now, &times).await;
    // The home tower attacked; the customs office reinforced until
    // tomorrow.
    let pocos_out = now + Duration::hours(20);
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            notification(
                3001,
                "TowerAlertMsg",
                now - Duration::minutes(4),
                &format!(
                    "aggressorAllianceID: null\naggressorCorpID: null\naggressorID: {ATTACKER}\n\
                     armorValue: 1.0\nhullValue: 1.0\nmoonID: {MOON}\nshieldValue: 0.5\n\
                     solarSystemID: {SYSTEM}\ntypeID: 16213\n"
                ),
            ),
            notification(
                3002,
                "OrbitalReinforced",
                now - Duration::minutes(3),
                &format!(
                    "aggressorAllianceID: null\naggressorCorpID: null\naggressorID: {ATTACKER}\n\
                     planetID: {PLANET}\nplanetTypeID: 2016\nreinforceExitTime: {}\n\
                     solarSystemID: {SYSTEM}\ntypeID: 2233\n",
                    filetime(pocos_out)
                ),
            ),
        ])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;
    let asked = h.sso.last_requested.lock().unwrap().clone();
    for scope in [
        "esi-corporations.read_starbases.v1",
        "esi-planets.read_customs_offices.v1",
        "esi-assets.read_corporation_assets.v1",
    ] {
        assert!(asked.contains(&scope.to_owned()), "{scope}: {asked:?}");
    }
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
    let c = DISCORD_PING_CHANNEL;
    // This test reads the messages' text: pings off. Starbases reinforced
    // ticked (not among aa-structures' default types), and a fuel alert
    // under 72 hours.
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", c),
            ("fuel_channel", c),
            ("state_channel", c),
            ("moon_channel", c),
            ("default_pings", ""),
            ("t_towerreinforcedextra", "on"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    add_fuel_alert(&h, &owner, 72, 0).await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;

    // Per-owner routing (aa-structures' webhooks per owner): this owner's
    // fuel alerts go nowhere for now; its attacks follow the default.
    let url = format!("/plugins/{ID}/settings/owner/{CHRIBBA_CORP}");
    let routing = page(&h, &url, &owner).await;
    assert_eq!(routing.status, StatusCode::OK, "{}", routing.body);
    assert!(
        routing.body.contains("Default (#fleet-pings)"),
        "{}",
        routing.body
    );
    let res = save_owner(
        &h,
        &owner,
        CHRIBBA_CORP,
        &[("fuel_channel", "none"), ("pocos_public", "on")],
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings.body.contains("Its own for 1 of 7 kinds"),
        "{}",
        settings.body
    );

    // Attacks were sent (the tower, the customs office and the tower's
    // reinforcement from its state); no fuel alert.
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 3, "{sent:?}\n{}", backlog(&h).await);
    assert_eq!(
        sent[0],
        "Starbase under attack: Home Tower (Caldari Control Tower) at Jita IV - Moon 4 in Jita \
         by Some Pilot. Shield 50%, armor 100%, hull 100%."
    );
    assert!(
        sent[1].starts_with(
            "Customs office reinforced: Customs Office \\(Jita IV\\) (Customs Office) at Jita IV \
             in Jita by Some Pilot. It comes out of reinforcement"
        ),
        "{sent:?}"
    );
    assert!(
        sent[2].starts_with(
            "Starbase reinforced: Home Tower (Caldari Control Tower) at Jita IV - Moon 4 in Jita."
        ),
        "{sent:?}"
    );
    assert!(sent.iter().all(|m| !m.contains("Low fuel")), "{sent:?}");

    // The host passed only slots, bays and the skyhook; the plugin kept
    // what's in its structures.
    assert_eq!(
        structure_items(&h).await,
        vec![
            (1, "HiSlot0".to_owned()),
            (2, "QuantumCoreRoom".to_owned()),
            (7, "StructureFuel".to_owned()),
            (8, "StructureFuel".to_owned()),
        ]
    );

    // The list: starbases with moon, fuel and strontium; orbitals with the
    // planet, window and taxes; the skyhook at its nearest planet.
    let list = page(&h, &format!("/plugins/{ID}?_tab=4"), &owner).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    for seen in [
        "Home Tower",
        "Jita IV - Moon 4",
        "Caldari Control Tower Small",
        "Reinforced",
        ">400</td>",
        // 960 blocks at 30 an hour (a quarter off in sov).
        "1d 7h",
    ] {
        assert!(list.body.contains(seen), "{seen}: {}", list.body);
    }
    let orbitals = page(&h, &format!("/plugins/{ID}?_tab=5"), &owner).await;
    for seen in [
        "Customs Office (Jita IV)",
        "18:00 to 20:00",
        "5.0%",
        "7.0%",
        "Orbital Skyhook (Jita IV)",
    ] {
        assert!(orbitals.body.contains(seen), "{seen}: {}", orbitals.body);
    }
    // The Metenox runs out of magmatic gas in 12 hours: it's low on fuel.
    let low = page(&h, &format!("/plugins/{ID}?_tab=1"), &owner).await;
    assert!(low.body.contains("Jita - Metenox"), "{}", low.body);
    assert!(low.body.contains("Home Tower"), "{}", low.body);
    assert!(!low.body.contains("Jita - Keep"), "{}", low.body);

    // The Keep's page: the fitting and the core; the Metenox's gas.
    let keep = page(&h, &format!("/plugins/{ID}/structure/{KEEP}"), &owner).await;
    assert_eq!(keep.status, StatusCode::OK, "{}", keep.body);
    for seen in [
        "High slots",
        "Standup Heavy Energy Neutralizer I",
        "Astrahus Upwell Quantum Core",
        "Installed",
    ] {
        assert!(keep.body.contains(seen), "{seen}: {}", keep.body);
    }
    let metenox = page(&h, &format!("/plugins/{ID}/structure/{METENOX}"), &owner).await;
    assert!(metenox.body.contains("Magmatic gas"), "{}", metenox.body);
    assert!(metenox.body.contains("2,400"), "{}", metenox.body);

    // Timers: the tower's and the customs office's final timers go to
    // Structure Timers.
    let shared = shared_timers(&h).await;
    let titles: Vec<&str> = shared.iter().map(|t| t.title.as_str()).collect();
    assert!(titles.contains(&"Home Tower: final timer"), "{titles:?}");
    assert!(
        titles.contains(&"Customs Office (Jita IV): final timer"),
        "{titles:?}"
    );

    // Fuel alerts, once the owner's fuel goes to the default channel:
    // the tower and the Metenox (its gas).
    let res = save_owner(
        &h,
        &owner,
        CHRIBBA_CORP,
        &[("fuel_channel", "default"), ("pocos_public", "on")],
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 5, "{sent:?}\n{}", backlog(&h).await);
    let fuel = &sent[3..];
    assert!(
        fuel.iter().any(|m| m.starts_with(
            "Low fuel: Jita - Metenox (Metenox Moon Drill) in Jita runs out of magmatic gas in 1"
        )),
        "{fuel:?}"
    );
    assert!(
        fuel.iter().any(|m| m.starts_with(
            "Low fuel: Home Tower (Caldari Control Tower) in Jita runs out of fuel in 1d"
        )),
        "{fuel:?}"
    );

    // Public customs offices: this owner's, with the viewer's access.
    let pocos = page(&h, &format!("/plugins/{ID}/pocos"), &owner).await;
    assert_eq!(pocos.status, StatusCode::OK, "{}", pocos.body);
    assert!(pocos.body.contains("Jita IV"), "{}", pocos.body);
    assert!(pocos.body.contains("5.0%"), "{}", pocos.body);

    // Tags: generated ones (space type, sov) on every structure.
    let tags = page(&h, &format!("/plugins/{ID}?_tab=7"), &owner).await;
    assert!(tags.body.contains("highsec"), "{}", tags.body);
    let tagged: Vec<String> = sqlx::query_scalar(
        r#"SELECT t.name FROM "plugin_tether.structures".structure_tags s
           JOIN "plugin_tether.structures".tags t ON t.id = s.tag_id
           WHERE s.structure_id = $1 ORDER BY t.name"#,
    )
    .bind(KEEP)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(tagged, vec!["highsec", "sov"]);

    // A manager makes a tag and puts it on the Keep; the filter shows it.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings/tags"),
            "_form=save_tag&name=Staging&description=Where+we+stage&style=warning&sort_order=100",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let staging: i32 = sqlx::query_scalar(
        r#"SELECT id FROM "plugin_tether.structures".tags WHERE name = 'Staging'"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    let keep_url = format!("/plugins/{ID}/structure/{KEEP}");
    let keep = page(&h, &keep_url, &owner).await;
    assert!(
        keep.body.contains(&format!("tag_{staging}")),
        "{}",
        keep.body
    );
    let res = send(
        &h.app,
        form(
            &keep_url,
            &format!("_form=structure_tags&tag_{staging}=on"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // The toolbar's tag filter (any of those chosen), in the address; a
    // tag's own link (the Tags tab's) shows the same.
    for uri in [
        format!("/plugins/{ID}?tag={staging}"),
        format!("/plugins/{ID}/tags/{staging}"),
    ] {
        let filtered = page(&h, &uri, &owner).await;
        assert_eq!(filtered.status, StatusCode::OK, "{uri}: {}", filtered.body);
        assert!(
            filtered.body.contains("Structures tagged Staging"),
            "{uri}: {}",
            filtered.body
        );
        assert!(filtered.body.contains("Jita - Keep"), "{}", filtered.body);
        assert!(
            !filtered.body.contains("Jita - Metenox"),
            "{uri}: {}",
            filtered.body
        );
    }
    let filtered = page(&h, &format!("/plugins/{ID}?tag={staging}"), &owner).await;
    assert!(
        filtered.body.contains(&format!(
            r#"<a href="/plugins/{ID}" aria-label="Take off Tag: Staging">"#
        )),
        "{}",
        filtered.body
    );

    // A Member of another corporation who may see only its own
    // corporation's structures: not the Keep's page, nor the managers'
    // pages; its tag counts leave out what it can't see.
    grant(&h, &owner, "basic_access").await;
    grant(&h, &owner, "view_corporation_structures").await;
    let member = log_in_as(&h, "1887431749:gigX", None).await;
    for url in [
        keep_url.clone(),
        format!("/plugins/{ID}/settings/tags"),
        url.clone(),
    ] {
        assert_eq!(
            page(&h, &url, &member).await.status,
            StatusCode::NOT_FOUND,
            "{url}"
        );
    }
    // The whole form, as the owner's page has it.
    let body = form_body(
        &page(&h, &url, &owner).await.body,
        "owner_routes",
        &[("attack_channel", "none"), ("fuel_channel", "none")],
    );
    let res = send(&h.app, form(&url, &body, &member)).await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let tags = page(&h, &format!("/plugins/{ID}?_tab=7"), &member).await;
    assert!(
        !tags.body.contains(r#"<td class="num text-right">1</td>"#),
        "{}",
        tags.body
    );

    // Someone without view_structure_fit sees the Keep but not its fit;
    // nor may they tag it.
    grant(&h, &owner, "view_all_structures").await;
    let seen = page(&h, &keep_url, &member).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(
        !seen.body.contains("Standup Heavy Energy Neutralizer I"),
        "{}",
        seen.body
    );
    assert!(seen.body.contains("Staging"), "{}", seen.body);
    let res = send(
        &h.app,
        form(
            &keep_url,
            &format!("_form=structure_tags&tag_{staging}=on"),
            &member,
        ),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    grant(&h, &owner, "view_structure_fit").await;
    let seen = page(&h, &keep_url, &member).await;
    assert!(
        seen.body.contains("Standup Heavy Energy Neutralizer I"),
        "{}",
        seen.body
    );

    // A manager deletes the tag from its row on the tag page, which asks
    // first; a Member can't.
    let tag_page = page(&h, &format!("/plugins/{ID}/settings/tags"), &owner).await;
    assert!(
        tag_page
            .body
            .contains("The tag Staging is deleted and comes off its 1 structures."),
        "{}",
        tag_page.body
    );
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings/tags"),
            &format!("_form=delete_tag&tag={staging}"),
            &member,
        ),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings/tags"),
            &format!("_form=delete_tag&tag={staging}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.structures".tags WHERE name = 'Staging'"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, 0);

    // Customs offices made private again: off the public list.
    let res = save_owner(&h, &owner, CHRIBBA_CORP, &[("pocos_public", "")], None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let pocos = page(&h, &format!("/plugins/{ID}/pocos"), &member).await;
    assert_eq!(pocos.status, StatusCode::OK, "{}", pocos.body);
    assert!(
        pocos
            .body
            .contains("No owner has made its customs offices public."),
        "{}",
        pocos.body
    );
}

/// A second Station Manager of Chribba's corporation, on his account.
const ALT: i64 = 90000050;

/// The affiliation fixture's characters, and the alt in Chribba's
/// corporation.
struct AltAffiliation;

impl wiremock::Respond for AltAffiliation {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let ids: Vec<i64> = serde_json::from_slice(&request.body).unwrap();
        let known = [
            serde_json::json!({ "character_id": CHRIBBA, "corporation_id": CHRIBBA_CORP, "alliance_id": ALLIANCE }),
            serde_json::json!({ "character_id": ALT, "corporation_id": CHRIBBA_CORP, "alliance_id": ALLIANCE }),
            serde_json::json!({ "character_id": 1887431749, "corporation_id": GIGX_CORP, "alliance_id": 1695357456 }),
        ];
        let items: Vec<&serde_json::Value> = known
            .iter()
            .filter(|v| ids.contains(&v["character_id"].as_i64().unwrap()))
            .collect();
        ResponseTemplate::new(200).set_body_json(items)
    }
}

/// Posts a form to one of the plugin's pages.
async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

/// Saves the settings page's form as a browser would, with these changes.
async fn save_settings(h: &Harness, token: &str, changes: &[(&str, &str)]) -> Res {
    let at = format!("/plugins/{ID}/settings");
    let body = form_body(&page(h, &at, token).await.body, "settings", changes);
    send(&h.app, form(&at, &body, token)).await
}

/// Saves an owner's routing form as a browser would, with these changes,
/// and (given `types`) only those types ticked.
async fn save_owner(
    h: &Harness,
    token: &str,
    corp: i64,
    changes: &[(&str, &str)],
    types: Option<&[&str]>,
) -> Res {
    let at = format!("/plugins/{ID}/settings/owner/{corp}");
    let mut body = form_body(&page(h, &at, token).await.body, "owner_routes", changes);
    if let Some(types) = types {
        body = body
            .split('&')
            .filter(|pair| !pair.starts_with("t_"))
            .map(str::to_owned)
            .chain(types.iter().map(|t| format!("{t}=on")))
            .collect::<Vec<_>>()
            .join("&");
    }
    send(&h.app, form(&at, &body, token)).await
}

/// aa-structures' rules: notification types per owner, fuel alert configs
/// (any number, with repeat and ping), unanchoring for
/// view_all_unanchoring_status only, and up to 10 sync characters per
/// owner taking turns at the notifications.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn aa_structures_rules(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, GIGX_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            notification(1001, "StructureUnderAttack", times.attacked, &attack_text()),
            notification(1002, "StructureLostShields", times.shields, &shields_text()),
        ])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;
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
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    let c = DISCORD_PING_CHANNEL;
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", c),
            ("fuel_channel", c),
            ("state_channel", c),
            ("moon_channel", c),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // This owner sends only lost shields (and fuel alerts), its own types:
    // one save of its form, its types its own.
    let res = save_owner(
        &h,
        &owner,
        CHRIBBA_CORP,
        &[("types_from", "own")],
        Some(&["t_structurelostshields", "t_structurefuelalert"]),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Fuel alerts: under 6 hours, once, pinging nobody; and under 100
    // hours, every hour, pinging danger.
    add_fuel_alert(&h, &owner, 6, 0).await;
    let res = post(
        &h,
        &owner,
        "settings",
        "_form=add_fuel_alert&start_hours=100&end_hours=0&repeat_hours=1&ping=danger",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let sent = discord_messages(&h).await;
    assert!(
        sent.iter().any(|m| m.contains("lost its shields")),
        "{sent:?}"
    );
    assert!(!sent.iter().any(|m| m.contains("Under attack")), "{sent:?}");
    // The Keep (5 hours left) is in both alerts' ranges. Pings are on by
    // default: the lost shields (danger) and the 100-hour alert (danger)
    // mention Member's role, the 6-hour alert (no ping) nobody.
    for hours in [6, 100] {
        assert_eq!(
            sent.iter()
                .filter(|m| m.contains(&format!("under the {hours}-hour alert")))
                .count(),
            1,
            "{hours}: {sent:?}"
        );
    }
    for (what, pinged) in [
        ("lost its shields", true),
        ("under the 100-hour alert", true),
        ("under the 6-hour alert", false),
    ] {
        let message = sent.iter().find(|m| m.contains(what)).unwrap();
        assert_eq!(message.starts_with(&member_ping()), pinged, "{message}");
        assert_eq!(message.contains("<@&"), pinged, "{message}");
    }
    // An hour on, the repeating alert goes again; the other doesn't.
    sqlx::query(
        r#"UPDATE "plugin_tether.structures".fuel_alerts_sent SET sent_at = now() - interval '2 hours'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let sent = discord_messages(&h).await;
    assert_eq!(
        sent.iter()
            .filter(|m| m.contains("under the 100-hour alert"))
            .count(),
        2,
        "{sent:?}"
    );
    assert_eq!(
        sent.iter()
            .filter(|m| m.contains("under the 6-hour alert"))
            .count(),
        1,
        "{sent:?}"
    );
    // Refuelled, then low again: a new episode, alerted again.
    for hours in [720, 4] {
        sqlx::query(
            r#"UPDATE "plugin_tether.structures".structures
               SET fuel_expires = now() + make_interval(hours => $2),
                   blocks_expires = now() + make_interval(hours => $2)
               WHERE structure_id = $1"#,
        )
        .bind(KEEP)
        .bind(hours)
        .execute(&h.db)
        .await
        .unwrap();
        let problems = sync(&h).await;
        assert!(problems.is_empty(), "{problems:?}");
    }
    let sent = discord_messages(&h).await;
    assert_eq!(
        sent.iter()
            .filter(|m| m.contains("under the 6-hour alert"))
            .count(),
        2,
        "{sent:?}"
    );

    // Unanchoring: only for view_all_unanchoring_status.
    sqlx::query(
        r#"UPDATE "plugin_tether.structures".structures SET unanchors_at = now() + interval '2 days'
           WHERE structure_id = $1"#,
    )
    .bind(DRILL)
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "plugin_tether.structures".timers (structure_id, kind, at, corporation_id)
           VALUES ($1, 'Unanchoring', now() + interval '2 days', $2)"#,
    )
    .bind(DRILL)
    .bind(CHRIBBA_CORP)
    .execute(&h.db)
    .await
    .unwrap();
    let gigx = log_in_as(&h, "1887431749:gigX", None).await;
    for permission in ["basic_access", "view_all_structures"] {
        grant(&h, &owner, permission).await;
    }
    let drill = format!("/plugins/{ID}/structure/{DRILL}");
    let timers = format!("/plugins/{ID}?_tab=3");
    let seen = page(&h, &drill, &gigx).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(!seen.body.contains("Unanchors"), "{}", seen.body);
    let seen = page(&h, &timers, &gigx).await;
    assert!(!seen.body.contains("Unanchoring"), "{}", seen.body);
    let seen = page(&h, &drill, &owner).await;
    assert!(seen.body.contains("Unanchors"), "{}", seen.body);
    let seen = page(&h, &timers, &owner).await;
    assert!(seen.body.contains("Unanchoring"), "{}", seen.body);
    // Nor are they given to Structure Timers (aa-structures makes none).
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    assert!(
        shared_timers(&h)
            .await
            .iter()
            .all(|t| !t.title.contains("unanchoring")),
        "unanchoring published"
    );

    // A second sync character for the corporation (aa-structures' up to
    // 10), added with Add data source like the first: the two take turns, so
    // notifications are read between syncs.
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(AltAffiliation)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    // Twice: the first login brings the new character (its corporation
    // arrives after), the second adds it for that corporation.
    let mut owner = owner;
    for _ in 0..2 {
        let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", &owner)).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        let login = res.cookie_value(LOGIN);
        let state = query_param(res.location(), "state").to_owned();
        let res = send(
            &h.app,
            get(
                &format!("/auth/callback?code=ok:{ALT}:Chribba+Alt&state={state}"),
                &[(LOGIN, &login), (SESSION, &owner)],
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        owner = res.cookie_value(SESSION);
    }
    let alt_path = format!("/characters/{ALT}/notifications");
    Mock::given(method("GET"))
        .and(path(alt_path.clone()))
        .respond_with(json(serde_json::json!([notification(
            3001,
            "StructureLostShields",
            now - Duration::minutes(1),
            &shields_text()
        )])))
        .mount(&h.esi_server)
        .await;
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'notifications' \
         AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);
    assert_eq!(reads(&h, &alt_path).await, 0);
    // Chribba read them a while ago: the alt's turn, between syncs.
    sqlx::query(
        r#"UPDATE "plugin_tether.structures".owners SET notifications_at = now() - interval '5 minutes'
           WHERE character_id = $1"#,
    )
    .bind(CHRIBBA)
    .execute(&h.db)
    .await
    .unwrap();
    let before = discord_messages(&h).await.len();
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'notifications' \
         AND state = 'queued'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(&h).await;
    assert_eq!(reads(&h, &alt_path).await, 1);
    assert_eq!(
        reads(&h, &format!("/characters/{CHRIBBA}/notifications")).await,
        1
    );
    assert_eq!(discord_messages(&h).await.len(), before + 1);
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(settings.body.contains("Chribba Alt"), "{}", settings.body);
    // Admins heard of it (aa-structures' "Character added to").
    let heard = notices(&h, CHRIBBA).await;
    assert!(
        heard.contains(
            &"Structures: Character added to: Otherworld Enterprises | Chribba Alt was added \
              as a data source for Otherworld Enterprises. It now has 2 data sources."
                .to_owned()
        ),
        "{heard:?}"
    );
}

const JUMP_GATE: i64 = 1_035_466_617_949;

/// aa-structures' other notification types: the corporation's own (sent at
/// once, to their kind's channel), alliance-wide ones only through the
/// alliance main owner, sovereignty timers for Structure Timers, and the
/// notices Tether makes itself (refuelled, jump gates low on ozone).
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn wars_sovereignty_members_refuels_and_jump_fuel(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    crate::structure_timers::install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    // A jump gate with 50,000 units of liquid ozone in its fuel bay.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/structures")))
        .respond_with(json(serde_json::json!([{
            "structure_id": JUMP_GATE, "name": "Jita » Perimeter", "corporation_id": CHRIBBA_CORP,
            "type_id": 35841, "system_id": SYSTEM, "profile_id": 1,
            "fuel_expires": rfc(now + Duration::days(10)), "state": "shield_vulnerable",
        }])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/assets")))
        .respond_with(json(serde_json::json!([{
            "item_id": 9001, "type_id": 16273, "location_id": JUMP_GATE,
            "location_flag": "StructureFuel", "location_type": "item",
            "quantity": 50000, "is_singleton": false,
        }])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_esi(&h, now, &times).await;
    let decloak = now + Duration::hours(30);
    let sent = |id: i64, kind: &str, text: String, sender: i64| {
        let mut n = notification(id, kind, now - Duration::minutes(3), &text);
        n["sender_id"] = sender.into();
        n
    };
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([
            sent(
                5001,
                "CharAppAcceptMsg",
                format!("applicationText: hi\ncharID: {ATTACKER}\ncorpID: {CHRIBBA_CORP}\n"),
                ATTACKER,
            ),
            // The character's own application to another corporation:
            // never passed on.
            sent(
                5004,
                "CorpAppRejectCustomMsg",
                format!(
                    "applicationText: me\ncharID: {CHRIBBA}\ncorpID: {GIGX_CORP}\n\
                     customMessage: no\n"
                ),
                GIGX_CORP,
            ),
            sent(
                5002,
                "WarDeclared",
                format!(
                    "againstID: {ALLIANCE}\ncost: 100000000\ndeclaredByID: {CHRIBBA_CORP}\n\
                     delayHours: 24\nhostileState: false\nwarHQ: <b>Jita - Keep</b>\n"
                ),
                1000125,
            ),
            sent(
                5003,
                "SovStructureReinforced",
                format!(
                    "campaignEventType: 1\ndecloakTime: {}\nsolarSystemID: {SYSTEM}\n",
                    filetime(decloak)
                ),
                ALLIANCE,
            ),
        ])))
        .mount(&h.esi_server)
        .await;
    let owner = approve_owner(&h, &owner).await;
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
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    let c = DISCORD_PING_CHANNEL;
    // aa-structures' default types leave wars, members and jump fuel
    // alerts off: the settings say so where they're set.
    let settings_url = format!("/plugins/{ID}/settings");
    let settings = page(&h, &settings_url, &owner).await;
    for note in [
        "None of these types is ticked under Types: Wars below",
        "None of these types is ticked under Types: Members and projects below",
        "Owners on the default types send none while",
    ] {
        assert!(settings.body.contains(note), "{note}: {}", settings.body);
    }
    assert!(
        !settings
            .body
            .contains("None of these types is ticked under Types: Sovereignty and bills below"),
        "{}",
        settings.body
    );
    let res = save_settings(
        &h,
        &owner,
        &[
            ("attack_channel", c),
            ("fuel_channel", c),
            ("state_channel", c),
            ("moon_channel", c),
            ("sov_channel", c),
            ("war_channel", c),
            ("corp_channel", c),
            ("t_charappacceptmsg", "on"),
            ("t_wardeclared", "on"),
            ("t_structurejumpfuelalert", "on"),
            ("t_structurerefueledextra", "on"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let settings = page(&h, &settings_url, &owner).await;
    for note in [
        "None of these types is ticked under Types: Wars below",
        "None of these types is ticked under Types: Members and projects below",
        "Owners on the default types send none while",
    ] {
        assert!(!settings.body.contains(note), "{note}: {}", settings.body);
    }
    let res = post(
        &h,
        &owner,
        "settings",
        "_form=add_jump_fuel_alert&threshold=100000&ping=warning",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let alliance: Option<i64> = sqlx::query_scalar(
        r#"SELECT alliance_id FROM "plugin_tether.structures".owners WHERE character_id = $1"#,
    )
    .bind(CHRIBBA)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(alliance, Some(ALLIANCE));
    // Stored per corporation (each of an alliance's gets its copy), and not
    // the application to another corporation.
    let stored: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT type, event_key FROM "plugin_tether.structures".notifications ORDER BY type"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(stored.len(), 3, "{stored:?}");
    assert!(
        stored
            .iter()
            .all(|(kind, key)| key.starts_with(&format!("{kind}:{CHRIBBA_CORP}:"))),
        "{stored:?}"
    );
    let messages = discord_messages(&h).await;
    // The corporation's own, at once.
    assert!(
        messages
            .iter()
            .any(|m| m == "Joined: Some Pilot is now a member of Otherworld Enterprises."),
        "{messages:?}"
    );
    // Alliance-wide: not until this owner is the alliance's main.
    assert!(
        !messages.iter().any(|m| m.contains("War declared")),
        "{messages:?}"
    );
    assert!(
        shared_timers(&h)
            .await
            .iter()
            .all(|t| !t.key.starts_with("sov:"))
    );
    // A jump gate below the alert: once, pinging the warning role, Member's
    // by default.
    let ozone: Vec<&String> = messages
        .iter()
        .filter(|m| m.contains("Jump gate low on liquid ozone"))
        .collect();
    assert_eq!(ozone.len(), 1, "{messages:?}");
    assert!(ozone[0].starts_with(&member_ping()), "{ozone:?}");
    assert!(
        ozone[0].contains("Jita » Perimeter") && ozone[0].contains("50000 units left"),
        "{ozone:?}"
    );
    // aa-structures' Jump gates tab, with its liquid ozone; gone when
    // unticked.
    let gates = page(&h, &format!("/plugins/{ID}?_tab=6"), &owner).await;
    for seen in ["Jump gates", "Jita » Perimeter", "50,000"] {
        assert!(gates.body.contains(seen), "{seen}: {}", gates.body);
    }
    let res = save_settings(&h, &owner, &[("show_jump_gates", "")]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let list = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(!list.body.contains("Jump gates"), "{}", list.body);
    let res = save_settings(&h, &owner, &[("show_jump_gates", "on")]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let list = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(list.body.contains("Jump gates"), "{}", list.body);

    let url = format!("settings/owner/{CHRIBBA_CORP}");
    let routing = page(&h, &format!("/plugins/{ID}/{url}"), &owner).await;
    assert!(routing.body.contains("Alliance main"), "{}", routing.body);
    let res = save_owner(&h, &owner, CHRIBBA_CORP, &[("alliance_main", "on")], None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // As if they'd just arrived.
    sqlx::query(r#"UPDATE "plugin_tether.structures".notifications SET handled = false"#)
        .execute(&h.db)
        .await
        .unwrap();
    // Refuelled since the last look: its fuel lasts longer than then.
    sqlx::query(
        r#"UPDATE "plugin_tether.structures".structures SET refuel_seen = now() + interval '1 day'
           WHERE structure_id = $1"#,
    )
    .bind(JUMP_GATE)
    .execute(&h.db)
    .await
    .unwrap();
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let messages = discord_messages(&h).await;
    // Danger, so each mentions Member's role by default.
    let war = format!(
        "{} War declared: Otherworld Enterprises declared war on Otherworld Empire with \
         Jita - Keep as war headquarters.",
        member_ping()
    );
    assert!(messages.iter().any(|m| m.starts_with(&war)), "{messages:?}");
    let sov = |what: &str| {
        format!(
            "{} Sovereignty structure reinforced: The {what} in Jita belonging to Otherworld \
             Empire",
            member_ping()
        )
    };
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with(&sov("Territorial Claim Unit"))
                || m.starts_with(&sov("sovereignty structure"))),
        "{messages:?}"
    );
    // Each once, though handled again.
    assert_eq!(
        messages.iter().filter(|m| m.starts_with("Joined:")).count(),
        1,
        "{messages:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.contains("Jump gate low"))
            .count(),
        1,
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Refuelled: Jita » Perimeter")),
        "{messages:?}"
    );
    let shared = shared_timers(&h).await;
    assert!(
        shared
            .iter()
            .any(|t| t.key.starts_with("sov:") && t.title == "TCU in Jita: sov timer"),
        "{shared:?}"
    );
}

// ---- aa-structures' fresh-install defaults ------------------------------------

/// What 0007 leaves in the settings: default pings, the warning ping, the
/// default types, how many fuel alert configs, moon extraction timers,
/// admin notices and the Jump gates tab.
type Defaults = (
    bool,
    Option<String>,
    Option<Vec<String>>,
    i64,
    bool,
    bool,
    bool,
);

/// Runs 0001-0006 in a scratch schema, then `setup` (an install in some
/// state), then 0007; what the settings then say.
async fn migrated(conn: &mut sqlx::PgConnection, schema: &str, setup: &str) -> Defaults {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA scratch_{schema}; SET search_path TO scratch_{schema}"
    )))
    .execute(&mut *conn)
    .await
    .unwrap();
    for name in &MIGRATIONS[..6] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(plugin_file(name)))
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(setup.to_owned()))
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(plugin_file(MIGRATIONS[6])))
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query_as(
        "SELECT default_pings, warning_ping, notification_types, \
             (SELECT count(*) FROM fuel_alert_configs), moon_extraction_timers, \
             admin_notifications, show_jump_gates \
         FROM settings",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

/// aa-structures' defaults reach a fresh install only: one already set up
/// in any way keeps what it has.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn aa_defaults_only_for_a_fresh_install(db: PgPool) {
    let mut conn = db.acquire().await.unwrap();
    let (pings, warning, types, configs, moons, admins, gates) =
        migrated(&mut conn, "fresh", "SELECT 1").await;
    assert!(pings && moons && admins && gates);
    assert_eq!(warning.as_deref(), Some("Member"));
    assert_eq!(types.map(|t| t.len()), Some(22));
    assert_eq!(configs, 0);

    let untouched = |pings: bool, types: Option<Vec<String>>, configs: i64| -> Defaults {
        (pings, None, types, configs, false, false, true)
    };
    for (schema, setup, kept) in [
        (
            "owner",
            "INSERT INTO owners (character_id, character_name, corporation_id) VALUES (1, 'A', 2)",
            untouched(false, None, 3),
        ),
        (
            "channel",
            "UPDATE settings SET attack_channel = '600000000000000001'",
            untouched(false, None, 3),
        ),
        (
            "routing",
            "INSERT INTO owner_channels (corporation_id, category, channel) VALUES (2, 'attack', NULL)",
            untouched(false, None, 3),
        ),
        (
            "owner_settings",
            "INSERT INTO owner_settings (corporation_id, mention) VALUES (2, 'on')",
            untouched(false, None, 3),
        ),
        // Saved with pings on, nothing else: the choice is kept.
        (
            "pings",
            "UPDATE settings SET default_pings = true",
            untouched(true, None, 3),
        ),
        // Saved with some types, no channel or owner yet.
        (
            "types",
            "UPDATE settings SET notification_types = ARRAY['StructureUnderAttack']",
            untouched(false, Some(vec!["StructureUnderAttack".to_owned()]), 3),
        ),
        (
            "alerts",
            "INSERT INTO fuel_alert_configs (start_hours, end_hours) VALUES (48, 0)",
            untouched(false, None, 4),
        ),
        (
            "queued",
            "INSERT INTO outbox (key, channel, message) VALUES ('k', '1', 'm')",
            untouched(false, None, 3),
        ),
    ] {
        assert_eq!(migrated(&mut conn, schema, setup).await, kept, "{schema}");
    }
    // Owners known before count as announced to admins; ones added after
    // wait for their notice.
    sqlx::raw_sql("SET search_path TO scratch_owner")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO owners (character_id, character_name, corporation_id) VALUES (3, 'B', 2)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let announced: Vec<(i64, bool)> =
        sqlx::query_as("SELECT character_id, announced FROM owners ORDER BY 1")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(announced, vec![(1, true), (3, false)]);
}

/// The Keep's armor timer and a moon chunk: Structures' own `timers`.
async fn kinds(h: &Harness) -> Vec<String> {
    sqlx::query_scalar(r#"SELECT kind FROM "plugin_tether.structures".timers ORDER BY at, kind"#)
        .fetch_all(&h.db)
        .await
        .unwrap()
}

/// A moon extraction started, its chunk ready at `ready`.
fn started_text(ready: DateTime<Utc>) -> String {
    moon_text()
        .replace(
            "autoTime: 133090956000000000",
            &format!("autoTime: {}", filetime(ready + Duration::hours(3))),
        )
        .replace(
            "readyTime: 133090848000000000",
            &format!("readyTime: {}", filetime(ready)),
        )
}

fn cancelled_text() -> String {
    format!(
        "cancelledBy: {CHRIBBA}\nmoonID: {MOON}\n\
         moonLink: <a href=\"showinfo:14//{MOON}\">Jita IV - Moon 4</a>\n\
         solarSystemID: {SYSTEM}\nstructureID: {DRILL}\nstructureName: Jita - Drill\n\
         structureTypeID: 35835\n"
    )
}

/// aa-structures' STRUCTURES_MOON_EXTRACTION_TIMERS_ENABLED: a moon
/// extraction started becomes Structure Timers' timer for its chunk, and
/// goes when it's cancelled or the setting is turned off.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn moon_extractions_become_structure_timers(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    crate::structure_timers::install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    // The chunk comes before the Keep's armor timer.
    let ready = now + Duration::hours(2);
    let started = |id: i64, ago: i64, ready: DateTime<Utc>| {
        notification(
            id,
            "MoonminingExtractionStarted",
            now - Duration::minutes(ago),
            &started_text(ready),
        )
    };
    // Each read answers what's mounted for it, in turn.
    let reads_ = [
        serde_json::json!([started(4001, 10, ready)]),
        serde_json::json!([
            started(4001, 10, ready),
            notification(
                4002,
                "MoonminingExtractionCancelled",
                now - Duration::minutes(1),
                &cancelled_text()
            ),
        ]),
        serde_json::json!([started(4003, 5, now + Duration::days(5))]),
        serde_json::json!([started(4004, 3, now + Duration::days(6))]),
        serde_json::json!([started(4005, 2, now + Duration::days(7))]),
    ];
    for answer in reads_ {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{CHRIBBA}/notifications")))
            .respond_with(json(answer))
            .up_to_n_times(1)
            .mount(&h.esi_server)
            .await;
    }
    let read_again = || async {
        sqlx::query(r#"UPDATE "plugin_tether.structures".owners SET notifications_at = NULL"#)
            .execute(&h.db)
            .await
            .unwrap();
        let problems = sync(&h).await;
        assert!(problems.is_empty(), "{problems:?}");
    };
    let extraction = |shared: &[Shared]| {
        shared
            .iter()
            .find(|t| t.key == format!("{DRILL}:extraction"))
            .map(|t| (t.title.clone(), t.details.clone()))
    };
    let owner = approve_owner(&h, &owner).await;
    // On for a fresh install.
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        form_body(&settings.body, "settings", &[]).contains("moon_extraction_timers=on"),
        "{}",
        settings.body
    );
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");

    // Published: friendly, for everyone, the moon named.
    let shared = shared_timers(&h).await;
    let timer = shared
        .iter()
        .find(|t| t.key == format!("{DRILL}:extraction"))
        .unwrap_or_else(|| panic!("{shared:?}"));
    assert_eq!(timer.title, "Jita - Drill: extraction ready");
    assert!(
        timer.details.starts_with(
            "Moon Mining Cycle at Jita IV - Moon 4: extraction ready. Athanor of Otherworld \
             Enterprises."
        ),
        "{shared:?}"
    );
    assert_eq!(timer.objective, "friendly");
    assert_eq!(timer.corporation_id, None);
    // Structure Timers lists it; Structures' Timers tab too, labelled, while
    // its Next timer stays the Keep's armor timer.
    let board = page(&h, "/plugins/tether.structure-timers", &owner).await;
    assert!(
        board
            .body
            .contains("<td>Jita - Drill: extraction ready</td>"),
        "{}",
        board.body
    );
    let list = page(&h, &format!("/plugins/{ID}?_tab=3"), &owner).await;
    assert!(
        list.body.contains("Extraction (chunk ready)"),
        "{}",
        list.body
    );
    let caption = |t: DateTime<Utc>| {
        format!(
            "<div class=\"stat-caption\">{} EVE</div>",
            t.format("%Y-%m-%d %H:%M")
        )
    };
    let armor = times.shields + Duration::days(1) + Duration::seconds(30);
    assert!(list.body.contains(&caption(armor)), "{}", list.body);
    assert!(!list.body.contains(&caption(ready)), "{}", list.body);

    // Cancelled: the timer goes.
    read_again().await;
    assert_eq!(extraction(&shared_timers(&h).await), None);
    assert!(!kinds(&h).await.contains(&"Extraction".to_owned()));

    // Started again: a timer again. Turned off: it goes at once, and the
    // next extraction makes none.
    read_again().await;
    assert!(extraction(&shared_timers(&h).await).is_some());
    let res = save_settings(&h, &owner, &[("moon_extraction_timers", "")]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    assert_eq!(extraction(&shared_timers(&h).await), None);
    assert!(!kinds(&h).await.contains(&"Extraction".to_owned()));
    read_again().await;
    assert!(!kinds(&h).await.contains(&"Extraction".to_owned()));
    // On again: the next one makes its timer.
    let res = save_settings(&h, &owner, &[("moon_extraction_timers", "on")]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    read_again().await;
    assert!(extraction(&shared_timers(&h).await).is_some());
    assert_eq!(
        reads(&h, &format!("/characters/{CHRIBBA}/notifications")).await,
        5
    );
}

// ---- admin notices ----------------------------------------------------------

/// aa-structures' admin notifications: superusers (and holders of manage)
/// hear of an owner added and of its services going down, coming back or
/// first working; an owner left out of the service status, or the setting
/// off, sends nothing.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_hear_of_owners_and_services(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, GIGX_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(&h, now, &times).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    // A Member who may open Structures, but isn't an admin.
    let _gigx = log_in_as(&h, "1887431749:gigX", None).await;
    grant(&h, &owner, "basic_access").await;
    let owner = approve_owner(&h, &owner).await;
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        notices(&h, CHRIBBA).await,
        vec![
            "Structures: Structure owner added: Otherworld Enterprises | Otherworld \
             Enterprises was added as a new structure owner, with Chribba as its data source."
                .to_owned(),
            "Structures: Services enabled for Otherworld Enterprises | Structure services for \
             Otherworld Enterprises have been enabled."
                .to_owned(),
        ]
    );
    assert!(notices(&h, GIGX).await.is_empty());

    // Notifications stop (backing off, last read an hour ago): down.
    let stop = || async {
        sqlx::query(
            r#"UPDATE "plugin_tether.structures".owners
               SET notifications_at = now() - interval '1 hour',
                   notifications_retry_at = now() + interval '1 hour'"#,
        )
        .execute(&h.db)
        .await
        .unwrap();
        let problems = sync(&h).await;
        assert!(problems.is_empty(), "{problems:?}");
    };
    let restart = || async {
        sqlx::query(
            r#"UPDATE "plugin_tether.structures".owners
               SET notifications_at = NULL, notifications_retry_at = NULL"#,
        )
        .execute(&h.db)
        .await
        .unwrap();
        let problems = sync(&h).await;
        assert!(problems.is_empty(), "{problems:?}");
    };
    stop().await;
    let heard = notices(&h, CHRIBBA).await;
    assert_eq!(heard.len(), 3, "{heard:?}");
    assert!(
        heard[2].starts_with(
            "Structures: Services are down for Otherworld Enterprises | Structure services for \
             Otherworld Enterprises are down. Admin action is likely required to restore \
             services. Structures: up; notifications: down; assets: up."
        ),
        "{heard:?}"
    );
    restart().await;
    let heard = notices(&h, CHRIBBA).await;
    assert_eq!(
        heard.last().map(String::as_str),
        Some(
            "Structures: Services restored for Otherworld Enterprises | Structure services for \
             Otherworld Enterprises have been restored."
        ),
        "{heard:?}"
    );

    // Left out of the service status: judged, never told.
    let res = save_owner(&h, &owner, CHRIBBA_CORP, &[("in_service_status", "")], None).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    stop().await;
    assert_eq!(notices(&h, CHRIBBA).await.len(), 4);
    let up: bool = sqlx::query_scalar(
        r#"SELECT up FROM "plugin_tether.structures".owner_status WHERE corporation_id = $1"#,
    )
    .bind(CHRIBBA_CORP)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(!up);

    // Notify admins off: nothing, though the owner counts again.
    let res = save_settings(&h, &owner, &[("admin_notifications", "")]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let routing = page(
        &h,
        &format!("/plugins/{ID}/settings/owner/{CHRIBBA_CORP}"),
        &owner,
    )
    .await;
    assert!(
        routing
            .body
            .contains("Nothing is sent while Notify admins is unticked in the settings"),
        "{}",
        routing.body
    );
    let res = save_owner(
        &h,
        &owner,
        CHRIBBA_CORP,
        &[("in_service_status", "on")],
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    restart().await;
    assert_eq!(notices(&h, CHRIBBA).await.len(), 4);
    assert!(notices(&h, GIGX).await.is_empty());
}

// ---- the sync's ESI calls ----------------------------------------------------

/// Chribba's corporation, with one attack in its notifications, and
/// gigX's (a lower id, so first by id), whose customs offices run to
/// `pages` pages at ESI; each corporation has one sync character. Returns
/// the owner's session and the path of gigX's corporation's customs
/// offices.
async fn two_owners(h: &Harness, pages: u32) -> (String, String) {
    let owner = log_in_owner(h, "196379789:Chribba").await;
    install(h, &owner).await;
    let now = Utc::now();
    let times = Times {
        attacked: now - Duration::minutes(10),
        shields: now - Duration::minutes(5),
    };
    mount_esi(h, now, &times).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/notifications")))
        .respond_with(json(serde_json::json!([notification(
            1001,
            "StructureUnderAttack",
            times.attacked,
            &attack_text()
        )])))
        .mount(&h.esi_server)
        .await;
    for at in [
        format!("/characters/{GIGX}/notifications"),
        format!("/corporations/{GIGX_CORP}/structures"),
        format!("/corporations/{GIGX_CORP}/starbases"),
        format!("/corporations/{GIGX_CORP}/assets"),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(json(serde_json::json!([])))
            .mount(&h.esi_server)
            .await;
    }
    let gigx_offices = format!("/corporations/{GIGX_CORP}/customs_offices");
    Mock::given(method("GET"))
        .and(path(gigx_offices.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", pages.to_string())
                .set_body_json(serde_json::json!([])),
        )
        .mount(&h.esi_server)
        .await;
    let mut owner = approve_owner(h, &owner).await;
    // As the second sync character is added: the first login brings it,
    // the second adds it for its corporation.
    for _ in 0..2 {
        let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", &owner)).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        let login = res.cookie_value(LOGIN);
        let state = query_param(res.location(), "state").to_owned();
        let res = send(
            &h.app,
            get(
                &format!("/auth/callback?code=ok:{GIGX}:gigX&state={state}"),
                &[(LOGIN, &login), (SESSION, &owner)],
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        owner = res.cookie_value(SESSION);
    }
    (owner, gigx_offices)
}

/// gigX's last error, failures in a row of its customs offices read, and
/// when it was last read whole.
async fn gigx_offices_read(h: &Harness) -> (Option<String>, i32, Option<DateTime<Utc>>) {
    sqlx::query_as(
        r#"SELECT last_error, offices_failures, offices_at FROM "plugin_tether.structures".owners
           WHERE character_id = $1"#,
    )
    .bind(GIGX)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// One corporation's customs offices too long for a sync (more pages
/// than its ESI calls) don't use the run up: every corporation's
/// notifications are read first, and the long read stops after its first
/// page, backing off with why.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_long_read_keeps_no_corporation_from_its_notifications(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    let (owner, gigx_offices) = two_owners(&h, 120).await;

    let problems = sync(&h).await;
    // Chribba's corporation's notifications were read, though gigX's comes
    // first.
    assert_eq!(
        reads(&h, &format!("/characters/{CHRIBBA}/notifications")).await,
        1,
        "{problems:?}"
    );
    assert_eq!(count(&h, "notifications").await, 1, "{problems:?}");
    // The long read stopped at its first page, and says why.
    assert_eq!(reads(&h, &gigx_offices).await, 1, "{problems:?}");
    assert!(
        problems.iter().all(|p| !p.contains("out of ESI calls")),
        "{problems:?}"
    );
    let (why, failures, _) = gigx_offices_read(&h).await;
    assert!(
        why.as_deref().is_some_and(
            |w| w.contains("customs offices: 120 pages at ESI, more than one sync can read")
        ),
        "{why:?}"
    );
    assert_eq!(failures, 1);
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings.body.contains("120 pages at ESI"),
        "{}",
        settings.body
    );
}

/// What a sync's hourly reads can have is what its notifications left
/// (two calls here, one per corporation): a read longer than that backs
/// off as one longer than any sync, though it's within the sync's 90
/// calls. It isn't tried, its first page paid for, every sync for good.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_read_longer_than_the_notifications_leave_backs_off(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    // 89 pages at a call each: within the 90, but 88 are left.
    let (_, gigx_offices) = two_owners(&h, 89).await;

    let problems = sync(&h).await;
    assert_eq!(reads(&h, &gigx_offices).await, 1, "{problems:?}");
    let (why, failures, read) = gigx_offices_read(&h).await;
    assert!(
        why.as_deref().is_some_and(|w| w.contains(
            "customs offices: 89 pages at ESI, more than one sync can read (88 after the \
             notifications)"
        )),
        "{why:?}"
    );
    assert_eq!(failures, 1, "backing off, not tried again next sync");
    assert_eq!(read, None);
}

/// A read that fits what the notifications leave, but not what this run's
/// other reads left, is tried again next sync without counting a failure,
/// goes first then (its corporation read longest ago), and is read whole.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_read_short_of_this_runs_calls_is_made_next_sync(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    // 87 pages: within the 88 the notifications leave; gigX's structures
    // and starbases are read first this time.
    let (_, gigx_offices) = two_owners(&h, 87).await;

    let problems = sync(&h).await;
    assert_eq!(reads(&h, &gigx_offices).await, 1, "{problems:?}");
    let (why, failures, read) = gigx_offices_read(&h).await;
    assert!(
        why.as_deref()
            .is_some_and(|w| w.contains("87 pages, more than this run had ESI calls left for")),
        "{why:?}"
    );
    assert_eq!((failures, read), (0, None));

    // Ten minutes on: the notifications are due again, the hourly reads
    // made aren't, and the pause after the short read is over.
    sqlx::query(
        r#"UPDATE "plugin_tether.structures".owners SET
               notifications_at = notifications_at - interval '10 minutes',
               structures_at = structures_at - interval '10 minutes',
               starbases_at = starbases_at - interval '10 minutes',
               offices_at = offices_at - interval '10 minutes',
               offices_retry_at = offices_retry_at - interval '10 minutes',
               assets_at = assets_at - interval '10 minutes'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let problems = sync(&h).await;
    assert_eq!(
        reads(&h, &format!("/characters/{GIGX}/notifications")).await,
        2,
        "{problems:?}"
    );
    assert_eq!(reads(&h, &gigx_offices).await, 1 + 87, "{problems:?}");
    let (why, failures, read) = gigx_offices_read(&h).await;
    assert_eq!((why, failures), (None, 0));
    assert!(read.is_some());
}

/// A corporation whose assets run past what a sync could read (60 pages
/// here): Tether reads every page in the background, and what sits on the
/// last (a skyhook) comes in once it's done, with nothing wrong said
/// meanwhile, and not a page of the run's calls spent.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_page_of_a_large_corporations_assets_is_read(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    let h = harness(db, true).await;
    let gigx_assets = format!("/corporations/{GIGX_CORP}/assets");
    Mock::given(method("GET"))
        .and(path(gigx_assets.clone()))
        .and(wiremock::matchers::query_param("page", "60"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "60")
                .set_body_json(serde_json::json!([{
                    "item_id": 1_046_000_000_001_i64, "type_id": 81080, "quantity": 1,
                    "location_id": 30000142, "location_flag": "AutoFit",
                    "location_type": "solar_system", "is_singleton": true,
                }])),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(gigx_assets.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "60")
                .set_body_json(serde_json::json!([])),
        )
        .with_priority(2)
        .mount(&h.esi_server)
        .await;
    two_owners(&h, 1).await;

    sync(&h).await;
    let skyhooks: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.structures".structures
           WHERE corporation_id = $1 AND kind = 'skyhook'"#,
    )
    .bind(GIGX_CORP)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(skyhooks, 1, "{}", backlog(&h).await);
    let (why, failures, read): (Option<String>, i32, Option<DateTime<Utc>>) = sqlx::query_as(
        r#"SELECT last_error, assets_failures, assets_at FROM "plugin_tether.structures".owners
           WHERE character_id = $1"#,
    )
    .bind(GIGX)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(
        why.as_deref().is_none_or(|w| !w.contains("assets")),
        "{why:?}"
    );
    assert_eq!(failures, 0);
    assert!(read.is_some());
    // Every page, read in the background (the first again for each
    // answer, so ESI checks the role).
    assert!(reads(&h, &gigx_assets).await >= 60);
}

// ---- the relay ------------------------------------------------------------------

/// A mention of a state with no Discord role mapped is sent without it,
/// wherever it falls in a run's five sends: never failed for good.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_mention_without_a_role_is_sent_plain_never_failed(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
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
    // Member has no role (Discord isn't mapped yet).
    sqlx::query("DELETE FROM core.discord_role_mappings")
        .execute(&h.db)
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    // Four plain messages, then two mentioning Member: the first of those
    // falls on the run's fifth send.
    for i in 1..=6 {
        sqlx::query(
            r#"INSERT INTO "plugin_tether.structures".outbox (key, channel, message, mention_state)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(format!("test:{i}"))
        .bind(DISCORD_PING_CHANNEL)
        .bind(format!("Message {i}"))
        .bind((i > 4).then_some("Member"))
        .execute(&h.db)
        .await
        .unwrap();
    }
    // The sync queues the relay; each later run is let go at once.
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    for _ in 0..5 {
        let unsent: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM "plugin_tether.structures".outbox
               WHERE sent_at IS NULL AND failed IS NULL"#,
        )
        .fetch_one(&h.db)
        .await
        .unwrap();
        if unsent == 0 {
            break;
        }
        sqlx::query(
            "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'relay' \
             AND state = 'queued'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
        work(&h).await;
    }
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 6, "{sent:?}\n{}", backlog(&h).await);
    assert!(sent.iter().all(|m| !m.contains("<@&")), "{sent:?}");
    for i in 1..=6 {
        assert!(sent.contains(&format!("Message {i}")), "{i}: {sent:?}");
    }
    let failed: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.structures".outbox WHERE failed IS NOT NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(failed, 0, "{}", backlog(&h).await);
}
