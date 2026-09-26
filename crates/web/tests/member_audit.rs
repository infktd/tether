#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The Member Audit plugin end to end: installed from its real component
//! and migration, Member requiring its user scopes, a character synced
//! from mocked ESI, and AA's pages: My Characters, the Character Sheet,
//! Character Finder, Skill Sets and reports.

mod common;

use std::sync::OnceLock;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.member-audit";
const CHRIBBA: i64 = 196379789;
const JITA: i64 = 30000142;
const JITA_4_4: i64 = 60003760;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("member-audit"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/member-audit/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(8);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_member_audit.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_member_audit.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn mount_esi(h: &Harness) {
    let json = |value: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(value)
    };
    let routes = [
        (
            "skills",
            serde_json::json!({
                "skills": [
                    { "skill_id": 3300, "active_skill_level": 5, "trained_skill_level": 5, "skillpoints_in_skill": 256000 },
                    { "skill_id": 3301, "active_skill_level": 3, "trained_skill_level": 3, "skillpoints_in_skill": 8000 },
                ],
                "total_sp": 50_000_000, "unallocated_sp": 12000,
            }),
        ),
        (
            "skillqueue",
            serde_json::json!([
                { "skill_id": 3301, "finished_level": 4, "queue_position": 0, "finish_date": "2030-01-01T00:00:00Z" },
            ]),
        ),
        ("wallet", serde_json::json!(1234567.89)),
        (
            "location",
            serde_json::json!({ "solar_system_id": JITA, "station_id": JITA_4_4 }),
        ),
        (
            "ship",
            serde_json::json!({ "ship_item_id": 1_000_000_000_001_i64, "ship_name": "Chribba's Rorqual", "ship_type_id": 28352 }),
        ),
        (
            "clones",
            serde_json::json!({ "jump_clones": [
                { "jump_clone_id": 7, "location_id": JITA_4_4, "location_type": "station", "implants": [9899] },
            ] }),
        ),
        ("implants", serde_json::json!([9899])),
        (
            "wallet/journal",
            serde_json::json!([
                { "id": 1, "date": "2026-09-24T12:00:00Z", "ref_type": "bounty_prizes",
                  "amount": 1000.0, "balance": 1234567.89, "description": "Bounty" },
            ]),
        ),
        (
            "assets",
            serde_json::json!([
                { "item_id": 1_000_000_000_010_i64, "type_id": 34, "quantity": 1_000_000, "location_id": JITA_4_4,
                  "location_flag": "Hangar", "location_type": "station", "is_singleton": false },
            ]),
        ),
    ];
    for (route, body) in routes {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{CHRIBBA}/{route}")))
            .respond_with(json(body))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(json(serde_json::json!([
            { "id": 3300, "name": "Gunnery", "category": "inventory_type" },
            { "id": 3301, "name": "Small Hybrid Turret", "category": "inventory_type" },
            { "id": JITA, "name": "Jita", "category": "solar_system" },
            { "id": JITA_4_4, "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant", "category": "station" },
            { "id": 28352, "name": "Rorqual", "category": "inventory_type" },
            { "id": 34, "name": "Tritanium", "category": "inventory_type" },
            { "id": 9899, "name": "Ocular Filter - Basic", "category": "inventory_type" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

fn registry(h: &Harness) -> Registry {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    tether_web::states::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    registry
}

async fn work(h: &Harness) {
    let registry = registry(h);
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

async fn sync(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:sync"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
}

/// Registers Chribba with the plugin's scopes (the Register Character
/// round trip); returns the new session.
async fn register(h: &Harness, token: &str) -> String {
    let res = send(&h.app, form("/register/start", "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(
        asked.contains(&"esi-skills.read_skillqueue.v1".to_owned()),
        "{asked:?}"
    );
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{CHRIBBA}:Chribba&state={state}"),
            &[(LOGIN, &login), (SESSION, token)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn member_audit_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    work(&h).await;
    let owner = register(&h, &owner).await;

    sync(&h).await;
    let problems: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(problems.is_empty(), "{problems:?}");

    // My Characters, with the combined numbers.
    let mine = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert!(mine.body.contains(">Jita<"), "{}", mine.body);
    assert!(mine.body.contains("Rorqual"));
    assert!(
        mine.body.contains("50000000") || mine.body.contains("50,000,000"),
        "{}",
        mine.body
    );

    // The Character Sheet, every tab.
    let mut sheet = String::new();
    for tab in 0..5 {
        let res = page(
            &h,
            &format!("/plugins/{ID}/character/{CHRIBBA}?_tab={tab}"),
            &owner,
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        sheet.push_str(&res.body);
    }
    for text in [
        "Gunnery",
        "Small Hybrid Turret",
        "Tritanium",
        "Ocular Filter - Basic",
        "Chribba&#39;s Rorqual",
        "bounty_prizes",
    ] {
        assert!(sheet.contains(text), "{text}: {sheet}");
    }

    // Character Finder (the owner holds everything).
    let finder = page(&h, &format!("/plugins/{ID}/finder?q=chrib"), &owner).await;
    assert_eq!(finder.status, StatusCode::OK, "{}", finder.body);
    assert!(finder.body.contains(&format!("character/{CHRIBBA}")));

    // A skill set, and who can use it.
    let bad = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/skill-sets"),
            "_form=add_set&name=Guns&skills=Not+A+Skill+4",
            &owner,
        ),
    )
    .await;
    assert!(bad.body.contains("isn&#39;t a skill"), "{}", bad.body);
    let added = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/skill-sets"),
            "_form=add_set&name=Guns&skills=Gunnery+4%0ASmall+Hybrid+Turret+3",
            &owner,
        ),
    )
    .await;
    assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
    let sets = page(&h, &format!("/plugins/{ID}/skill-sets"), &owner).await;
    assert!(sets.body.contains("Gunnery 4"), "{}", sets.body);
    let reports = page(&h, &format!("/plugins/{ID}/reports"), &owner).await;
    assert!(reports.body.contains("Guns"), "{}", reports.body);

    // A Blue pilot with basic access sees only their own characters.
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.basic&grantee=state:{BLUE_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &blue).await.status,
        StatusCode::OK
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/character/{CHRIBBA}"), &blue)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    for uri in ["finder", "finder?q=chribba", "reports"] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/{uri}"), &blue)
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    // Only skill-set managers change them.
    let refused = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/skill-sets"),
            "_form=add_set&name=Mine&skills=Gunnery+1",
            &blue,
        ),
    )
    .await;
    assert!(refused.status.is_client_error(), "{}", refused.body);

    // Deleting a skill set.
    let set: i64 = sqlx::query_scalar("SELECT id FROM \"plugin_tether.member-audit\".skill_sets")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let deleted = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/skill-sets"),
            &format!("_form=delete_set&set={set}&confirm=on"),
            &owner,
        ),
    )
    .await;
    assert_eq!(deleted.status, StatusCode::SEE_OTHER, "{}", deleted.body);
    assert!(
        !page(&h, &format!("/plugins/{ID}/reports"), &owner)
            .await
            .body
            .contains("Guns")
    );

    // With nobody qualifying the host's list is empty, which may be the
    // host having trouble: nothing is forgotten on that alone.
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked' WHERE character_id = $1")
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    sync(&h).await;
    let characters: i64 =
        sqlx::query_scalar("SELECT count(*) FROM \"plugin_tether.member-audit\".characters")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(characters, 1);
}
