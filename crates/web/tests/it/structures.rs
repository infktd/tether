//! The Structures plugin end to end: installed from its real component
//! and migrations, fed by mocked ESI through an approved owner (data
//! source), listing structures with fuel, relaying notifications and
//! low-fuel alerts to Discord once, seen by permission, and backing off
//! an owner ESI answers 403 for.

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

/// The real package, signed with a test key.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let first = plugin_file("migrations/0001_structures.sql");
    let second = plugin_file("migrations/0002_timers_corporation_only.sql");
    let third = plugin_file("migrations/0003_starbases_orbitals_tags.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_structures.sql", first.as_bytes()),
        (
            "migrations/0002_timers_corporation_only.sql",
            second.as_bytes(),
        ),
        (
            "migrations/0003_starbases_orbitals_tags.sql",
            third.as_bytes(),
        ),
    ]);
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
    let res = send(
        &h.app,
        form(&format!("/profile/plugins/{ID}/offer"), "", owner),
    )
    .await;
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
    sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
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
            body["content"].as_str().unwrap().to_owned()
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

async fn grant(h: &Harness, owner: &str, permission: &str) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
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

    // Discord: the plugin gets the ping channel; everything goes there,
    // with alerts at 72, 24 and 6 hours and Members mentioned on attacks.
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
            &format!(
                "_form=settings&attack_channel={c}&fuel_channel={c}&state_channel={c}\
                 &moon_channel={c}&fuel_thresholds=72%2C+0&mention_members=on"
            ),
            &owner,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::OK, "{}", bad.body);
    assert!(
        bad.body.contains("Write the low-fuel alerts"),
        "{}",
        bad.body
    );
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!(
                "_form=settings&attack_channel={c}&fuel_channel={c}&state_channel={c}\
                 &moon_channel={c}&fuel_thresholds=6%2C+72%2C+24&mention_members=on"
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
    // The host passed on known structure notifications only.
    assert_eq!(count(&h, "notifications").await, 5);
    let by_owner = page(&h, &format!("/plugins/{ID}/owner/{CHRIBBA_CORP}"), &owner).await;
    assert_eq!(by_owner.status, StatusCode::OK, "{}", by_owner.body);
    assert!(by_owner.body.contains("Structures: Otherworld Enterprises"));
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/owner/{GIGX_CORP}"), &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Discord: the attack and the shields with the
    // timer (both mentioning Members), the moon drill and Tether's
    // low-fuel alert; not the old one.
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert!(
        sent[0].starts_with(&format!("<@&{DISCORD_MEMBER_ROLE}>")),
        "{sent:?}"
    );
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
    assert!(
        sent[3].starts_with("Low fuel: Jita - Keep (Astrahus) in Jita"),
        "{sent:?}"
    );
    assert!(sent[3].contains("under the 6-hour alert"), "{sent:?}");

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
    assert_eq!(count(&h, "notifications").await, 5);
    assert_eq!(discord_messages(&h).await.len(), 4);
    // Structures were read under an hour ago: not again.
    assert_eq!(
        reads(&h, &format!("/corporations/{CHRIBBA_CORP}/structures")).await,
        1
    );
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
        form(
            &format!("/admin/plugins/{ID}/sources/{CHRIBBA}/remove"),
            "",
            &owner,
        ),
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
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&attack_channel=&fuel_channel=&state_channel=&moon_channel=\
             &fuel_thresholds=72&timers_corporation_only=on",
            &owner,
        ),
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
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            &format!(
                "_form=settings&attack_channel={c}&fuel_channel={c}&state_channel={c}\
                 &moon_channel={c}&fuel_thresholds=72"
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
    let res = send(
        &h.app,
        form(
            &url,
            "_form=owner_routes&attack_channel=default&fuel_channel=none&state_channel=default\
             &moon_channel=default&mention=default&pocos_public=on",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings.body.contains("Its own for 1 of 4 kinds"),
        "{}",
        settings.body
    );

    // Attacks were sent (the tower, the customs office and the tower's
    // reinforcement from its state); no fuel alert.
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 3, "{sent:?}");
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
    let res = send(
        &h.app,
        form(
            &url,
            "_form=owner_routes&attack_channel=default&fuel_channel=default&state_channel=default\
             &moon_channel=default&mention=default&pocos_public=on",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let problems = sync(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 5, "{sent:?}");
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
    let tags = page(&h, &format!("/plugins/{ID}?_tab=6"), &owner).await;
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
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}"),
            &format!("_form=filter_tags&tag_{staging}=on"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/plugins/{ID}/tags/{staging}"));
    let filtered = page(&h, &format!("/plugins/{ID}/tags/{staging}"), &owner).await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.body);
    assert!(
        filtered.body.contains("Structures tagged Staging"),
        "{}",
        filtered.body
    );
    assert!(filtered.body.contains("Jita - Keep"), "{}", filtered.body);
    assert!(
        !filtered.body.contains("Jita - Metenox"),
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
    let res = send(
        &h.app,
        form(
            &url,
            "_form=owner_routes&attack_channel=none&fuel_channel=none&state_channel=none\
             &moon_channel=none&mention=default",
            &member,
        ),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let tags = page(&h, &format!("/plugins/{ID}?_tab=6"), &member).await;
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

    // Customs offices made private again: off the public list.
    let res = send(
        &h.app,
        form(
            &url,
            "_form=owner_routes&attack_channel=default&fuel_channel=default&state_channel=default\
             &moon_channel=default&mention=default",
            &owner,
        ),
    )
    .await;
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
