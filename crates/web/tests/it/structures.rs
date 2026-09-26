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
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_structures.sql", first.as_bytes()),
        (
            "migrations/0002_timers_corporation_only.sql",
            second.as_bytes(),
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
        ])))
        // Before the harness's own names fixture.
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
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
