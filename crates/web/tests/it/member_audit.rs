//! The Member Audit plugin end to end: installed from its real component
//! and migrations, Member requiring its user scopes, a character synced
//! from mocked ESI (every section of the sheet), and AA's pages: My
//! Characters (the card grid, Register Character first), the Character
//! Sheet's pages and tabs, mail behind `view_mail` and audited, the
//! Character Finder scoped by corporation, alliance or everything, Skill
//! Sets and reports.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.member-audit";
const CHRIBBA: i64 = 196379789;
const CORP: i64 = 1164409536;
const ALLIANCE: i64 = 159826257;
const JITA: i64 = 30000142;
const JITA_4_4: i64 = 60003760;
const KEEPSTAR: i64 = 1_030_000_000_001;
const PLANET: i64 = 40_009_082;
const MAIL: i64 = 77;
const CONTRACT: i64 = 5001;
const KILLMAIL: i64 = 1002;

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

const MIGRATIONS: [&str; 3] = [
    "migrations/0001_member_audit.sql",
    "migrations/0002_complete_data.sql",
    "migrations/0003_character_sheet.sql",
];

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(8);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migrations: Vec<String> = MIGRATIONS.iter().map(|m| plugin_file(m)).collect();
    let component = component();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ];
    for (name, sql) in MIGRATIONS.iter().zip(&migrations) {
        entries.push((name, sql.as_bytes()));
    }
    let bytes = testing::zip(&entries);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Every section of a character sheet, as ESI answers.
async fn mount_esi(h: &Harness) {
    let json = |value: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(value)
    };
    let hash = "b".repeat(40);
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
                { "skill_id": 3301, "finished_level": 4, "queue_position": 0,
                  "start_date": "2026-01-01T00:00:00Z", "finish_date": "2090-01-01T00:00:00Z",
                  "level_start_sp": 8000, "level_end_sp": 45255, "training_start_sp": 8000 },
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
            serde_json::json!({
                "home_location": { "location_id": JITA_4_4, "location_type": "station" },
                "last_clone_jump_date": "2026-09-01T00:00:00Z",
                "jump_clones": [
                    { "jump_clone_id": 7, "location_id": KEEPSTAR, "location_type": "structure", "implants": [9899] },
                ],
            }),
        ),
        ("implants", serde_json::json!([9899])),
        (
            "wallet/journal",
            serde_json::json!([
                { "id": 1, "date": "2026-09-24T12:00:00Z", "ref_type": "bounty_prizes",
                  "amount": 1000.0, "balance": 1234567.89, "description": "Bounty",
                  "first_party_id": 1000125, "second_party_id": CHRIBBA },
            ]),
        ),
        (
            "wallet/transactions",
            serde_json::json!([
                { "client_id": 90000010, "date": "2026-09-24T12:00:00Z", "is_buy": false,
                  "is_personal": true, "journal_ref_id": 2, "location_id": JITA_4_4,
                  "quantity": 10, "transaction_id": 3, "type_id": 34, "unit_price": 5.5 },
            ]),
        ),
        (
            "assets",
            serde_json::json!([
                { "item_id": 1_000_000_000_010_i64, "type_id": 34, "quantity": 1_000_000, "location_id": JITA_4_4,
                  "location_flag": "Hangar", "location_type": "station", "is_singleton": false },
                { "item_id": 1_000_000_000_011_i64, "type_id": 587, "quantity": 1, "location_id": JITA_4_4,
                  "location_flag": "Hangar", "location_type": "station", "is_singleton": true },
                { "item_id": 1_000_000_000_012_i64, "type_id": 9899, "quantity": 2,
                  "location_id": 1_000_000_000_011_i64, "location_flag": "Cargo",
                  "location_type": "item", "is_singleton": false },
            ]),
        ),
        (
            "contracts",
            serde_json::json!([
                { "acceptor_id": 0, "assignee_id": 0, "availability": "public",
                  "contract_id": CONTRACT, "date_expired": "2090-01-01T00:00:00Z",
                  "date_issued": "2026-09-20T00:00:00Z", "for_corporation": false,
                  "issuer_corporation_id": CORP, "issuer_id": CHRIBBA, "status": "outstanding",
                  "type": "item_exchange", "title": "Tritanium for sale", "price": 5000000.0 },
            ]),
        ),
        (
            "contracts/5001/items",
            serde_json::json!([
                { "is_included": true, "is_singleton": false, "quantity": 1000, "record_id": 1, "type_id": 34 },
            ]),
        ),
        (
            "contacts",
            serde_json::json!([
                { "contact_id": 90000010, "contact_type": "character", "standing": 10.0, "is_watched": true },
            ]),
        ),
        (
            "standings",
            serde_json::json!([
                { "from_id": 500001, "from_type": "faction", "standing": 2.5 },
            ]),
        ),
        (
            "mail",
            serde_json::json!([
                { "mail_id": MAIL, "from": 90000011, "subject": "Fleet tonight", "is_read": false,
                  "labels": [1], "timestamp": "2026-09-20T19:04:05Z",
                  "recipients": [{ "recipient_id": CHRIBBA, "recipient_type": "character" }] },
            ]),
        ),
        (
            "mail/77",
            serde_json::json!({
                "body": "<font size=\"12\">Fleet at 19:00</font><br>Bring <b>logi</b>",
                "from": 90000011, "labels": [1], "read": false,
                "recipients": [{ "recipient_id": CHRIBBA, "recipient_type": "character" }],
                "subject": "Fleet tonight", "timestamp": "2026-09-20T19:04:05Z",
            }),
        ),
        (
            "mail/labels",
            serde_json::json!({
                "labels": [{ "color": "#ffffff", "label_id": 1, "name": "Inbox", "unread_count": 1 }],
                "total_unread_count": 1,
            }),
        ),
        ("mail/lists", serde_json::json!([])),
        (
            "loyalty/points",
            serde_json::json!([{ "corporation_id": 1000035, "loyalty_points": 12345 }]),
        ),
        (
            "planets",
            serde_json::json!([
                { "last_update": "2026-09-20T00:00:00Z", "num_pins": 12, "owner_id": CHRIBBA,
                  "planet_id": PLANET, "planet_type": "temperate", "solar_system_id": JITA,
                  "upgrade_level": 4 },
            ]),
        ),
        (
            "industry/jobs",
            serde_json::json!([
                { "activity_id": 1, "blueprint_id": 55, "blueprint_location_id": JITA_4_4,
                  "blueprint_type_id": 691, "duration": 3600, "end_date": "2090-01-01T00:00:00Z",
                  "facility_id": JITA_4_4, "installer_id": CHRIBBA, "job_id": 66,
                  "output_location_id": JITA_4_4, "product_type_id": 587, "runs": 10,
                  "start_date": "2026-09-20T00:00:00Z", "station_id": JITA_4_4, "status": "active" },
            ]),
        ),
        (
            "blueprints",
            serde_json::json!([
                { "item_id": 55, "location_flag": "Hangar", "location_id": JITA_4_4,
                  "material_efficiency": 10, "quantity": -1, "runs": -1, "time_efficiency": 20,
                  "type_id": 691 },
            ]),
        ),
        (
            "orders",
            serde_json::json!([
                { "duration": 90, "is_corporation": false, "issued": "2026-09-20T00:00:00Z",
                  "location_id": JITA_4_4, "order_id": 88, "price": 6.0, "range": "station",
                  "region_id": 10000002, "type_id": 34, "volume_remain": 500, "volume_total": 1000 },
            ]),
        ),
        (
            "killmails/recent",
            serde_json::json!([{ "killmail_hash": hash, "killmail_id": KILLMAIL }]),
        ),
        (
            "corporationhistory",
            serde_json::json!([
                { "corporation_id": CORP, "record_id": 2, "start_date": "2010-01-01T00:00:00Z" },
                { "corporation_id": 1000167, "record_id": 1, "start_date": "2006-01-01T00:00:00Z" },
            ]),
        ),
        (
            "attributes",
            serde_json::json!({ "charisma": 17, "intelligence": 27, "memory": 21, "perception": 17,
                                "willpower": 17, "bonus_remaps": 1 }),
        ),
        (
            "roles",
            serde_json::json!({ "roles": ["Director"], "roles_at_hq": [], "roles_at_base": [], "roles_at_other": [] }),
        ),
        (
            "titles",
            serde_json::json!([{ "title_id": 1, "name": "<color=0xff00ff00>Quartermaster</color>" }]),
        ),
        (
            "mining",
            serde_json::json!([{ "date": "2026-09-20", "quantity": 1000, "solar_system_id": JITA, "type_id": 1230 }]),
        ),
    ];
    for (route, body) in routes {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{CHRIBBA}/{route}")))
            .respond_with(json(body))
            .mount(&h.esi_server)
            .await;
    }
    let public = [
        (
            format!("/characters/{CHRIBBA}"),
            serde_json::json!({
                "achievement_score": 0, "birthday": "2006-03-01T12:00:00Z", "bloodline_id": 5, "corporation_id": CORP,
                "description": "<b>Honest</b> trader", "gender": "male", "name": "Chribba",
                "race_id": 2, "security_status": 5.0,
            }),
        ),
        (
            format!("/killmails/{KILLMAIL}/{hash}"),
            serde_json::json!({
                "attackers": [{ "character_id": CHRIBBA, "damage_done": 500, "final_blow": true,
                                "security_status": 5.0, "ship_type_id": 587 }],
                "killmail_id": KILLMAIL, "killmail_time": "2026-09-20T19:04:05Z", "solar_system_id": JITA,
                "victim": { "character_id": 90000012, "corporation_id": 98000001, "damage_taken": 500,
                            "ship_type_id": 587 },
            }),
        ),
        (
            format!("/universe/structures/{KEEPSTAR}"),
            serde_json::json!({ "name": "Jita - Example Keepstar", "owner_id": 98000001,
                                "solar_system_id": JITA, "type_id": 35834 }),
        ),
        (
            format!("/universe/planets/{PLANET}"),
            serde_json::json!({ "name": "Jita IV", "planet_id": PLANET,
                                "position": { "x": 1.0, "y": 2.0, "z": 3.0 },
                                "system_id": JITA, "type_id": 11 }),
        ),
        (
            "/universe/categories/16".to_owned(),
            serde_json::json!({ "category_id": 16, "groups": [255], "name": "Skill", "published": true }),
        ),
        (
            "/universe/groups/255".to_owned(),
            serde_json::json!({ "category_id": 16, "group_id": 255, "name": "Gunnery",
                                "published": true, "types": [3300, 3301] }),
        ),
    ];
    for (route, body) in public {
        Mock::given(method("GET"))
            .and(path(route))
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
            { "id": 587, "name": "Rifter", "category": "inventory_type" },
            { "id": 691, "name": "Rifter Blueprint", "category": "inventory_type" },
            { "id": 1230, "name": "Veldspar", "category": "inventory_type" },
            { "id": 9899, "name": "Ocular Filter - Basic", "category": "inventory_type" },
            { "id": CORP, "name": "Example Holding", "category": "corporation" },
            { "id": ALLIANCE, "name": "Example Alliance", "category": "alliance" },
            { "id": 1000035, "name": "Caldari Navy", "category": "corporation" },
            { "id": 1000125, "name": "CONCORD", "category": "corporation" },
            { "id": 1000167, "name": "State War Academy", "category": "corporation" },
            { "id": 500001, "name": "Caldari State", "category": "faction" },
            { "id": 90000010, "name": "Friendly Pilot", "category": "character" },
            { "id": 90000011, "name": "Fleet Commander", "category": "character" },
            { "id": 90000012, "name": "Unlucky Pilot", "category": "character" },
            { "id": 98000001, "name": "Some Corp", "category": "corporation" },
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
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
    for scope in [
        "esi-skills.read_skillqueue.v1",
        "esi-mail.read_mail.v1",
        "esi-universe.read_structures.v1",
    ] {
        assert!(asked.contains(&scope.to_owned()), "{asked:?}");
    }
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

async fn plugin_warnings(h: &Harness) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// Installed, Chribba registered and read whole.
async fn synced(db: PgPool) -> (Harness, String) {
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    work(&h).await;
    let owner = register(&h, &owner).await;
    sync(&h).await;
    let problems = plugin_warnings(&h).await;
    assert!(problems.is_empty(), "{problems:?}");
    (h, owner)
}

async fn grant(h: &Harness, owner: &str, permission: &str) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{BLUE_STATE}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

async fn mail_views(h: &Harness) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT actor_name, details FROM core.audit_log \
         WHERE action = 'plugin.page_view' AND target = $1 ORDER BY id",
    )
    .bind(format!("plugin:{ID}"))
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn member_audit_end_to_end(db: PgPool) {
    let (h, owner) = synced(db).await;
    // Every section was read, within the run's budget.
    let sections: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.member-audit".section_syncs WHERE ok"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(sections, 24, "{:?}", plugin_warnings(&h).await);

    // My Characters: Tether's Register Character card first, then a card
    // per character with its portrait, logos and facts, and the totals.
    let mine = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    let body = &mine.body;
    let register = body.find(r#"<a class="card grid-card grid-card-register" href="/register">"#);
    let card = body.find(&format!("/plugins/{ID}/character/{CHRIBBA}"));
    assert!(
        register.is_some() && card.is_some() && register < card,
        "{body}"
    );
    for part in [
        format!("https://images.evetech.net/characters/{CHRIBBA}/portrait?size=128"),
        format!("https://images.evetech.net/corporations/{CORP}/logo?size=64"),
        format!("https://images.evetech.net/alliances/{ALLIANCE}/logo?size=64"),
        ">Jita<".to_owned(),
        "Rorqual".to_owned(),
        "50,000,000".to_owned(),
        // The skill in training fills live.
        r#"data-from="2026-01-01T00:00:00Z" data-to="2090-01-01T00:00:00Z""#.to_owned(),
        "Small Hybrid Turret IV".to_owned(),
        // No "More" card: the app's pages are beside the title.
        format!(r#"<a href="/plugins/{ID}/skill-sets">Skill Sets</a>"#),
    ] {
        assert!(body.contains(&part), "{part}\n\n{body}");
    }
    assert!(!body.contains(">More<"), "{body}");

    // The Character Sheet: every page and tab.
    let sheet_pages = [
        ("", 4),
        ("/skills", 4),
        ("/assets", 1),
        (&format!("/assets/{JITA_4_4}") as &str, 1),
        ("/wallet", 5),
        (&format!("/contract/{CONTRACT}") as &str, 1),
        ("/clones", 1),
        ("/industry", 4),
        ("/contacts", 2),
    ];
    let mut sheet = String::new();
    for (sub, tabs) in sheet_pages {
        for tab in 0..tabs {
            let res = page(
                &h,
                &format!("/plugins/{ID}/character/{CHRIBBA}{sub}?_tab={tab}"),
                &owner,
            )
            .await;
            if res.status != StatusCode::OK {
                panic!("{sub} {tab}: {:?}", plugin_warnings(&h).await);
            }
            sheet.push_str(&res.body);
        }
    }
    for text in [
        // Overview.
        "Chribba&#39;s Rorqual",
        "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
        ">5.0<",
        "2006-03-01",
        "Update now",
        "State War Academy",
        "Director",
        "Quartermaster",
        "Unlucky Pilot",
        "Honest trader",
        // Skills, by group.
        "Gunnery · 264,000 SP",
        "Small Hybrid Turret",
        "Charisma",
        // Assets by location, a ship's cargo in it.
        "Tritanium",
        "Ocular Filter - Basic",
        // Wallet.
        "Bounty prizes",
        "CONCORD",
        "Friendly Pilot",
        "Tritanium for sale",
        "Caldari Navy",
        // Clones: the implant's icon, the jump clone's structure.
        "https://images.evetech.net/types/9899/icon?size=32",
        "Jump clone in Jita - Example Keepstar",
        // Industry.
        "Manufacturing",
        "Rifter Blueprint",
        "Veldspar",
        "Jita IV",
        // Contacts and standings.
        "Caldari State",
        "+10.0",
        // Freshness under each tab.
        "Last update",
    ] {
        assert!(sheet.contains(text), "{text}");
    }
    // The owner's own character: Mail beside the title.
    assert!(sheet.contains(&format!(
        r#"<a href="/plugins/{ID}/mail/{CHRIBBA}">Mail</a>"#
    )));

    // Mail: the owner's own, read and audited.
    let list = page(&h, &format!("/plugins/{ID}/mail/{CHRIBBA}"), &owner).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(list.body.contains("Fleet tonight"), "{}", list.body);
    assert!(list.body.contains("Inbox (1)"), "{}", list.body);
    let one = page(&h, &format!("/plugins/{ID}/mail/{CHRIBBA}/{MAIL}"), &owner).await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.body);
    assert!(
        one.body.contains("Fleet at 19:00\nBring logi"),
        "{}",
        one.body
    );
    assert!(one.body.contains("Fleet Commander"), "{}", one.body);
    let views = mail_views(&h).await;
    assert_eq!(views.len(), 2, "{views:?}");
    assert_eq!(views[0].0, "Chribba");
    assert_eq!(views[1].1["path"], format!("mail/{CHRIBBA}/{MAIL}"));
    // Only the mail pages are audited.
    assert!(
        views
            .iter()
            .all(|(_, d)| d["path"].as_str().unwrap().starts_with("mail/"))
    );

    // Update now: queued, run, and the sheet offers it again.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/character/{CHRIBBA}"),
            &format!("_form=update_character&character={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::SEE_OTHER,
        "{:?}",
        plugin_warnings(&h).await
    );
    let queued = page(&h, &format!("/plugins/{ID}/character/{CHRIBBA}"), &owner).await;
    assert!(queued.body.contains("Update queued"), "{}", queued.body);
    work(&h).await;
    let done = page(&h, &format!("/plugins/{ID}/character/{CHRIBBA}"), &owner).await;
    assert!(done.body.contains("Updated"), "{}", done.body);
    // Once in ten minutes: the button is back after that.
    sqlx::query(
        r#"UPDATE "plugin_tether.member-audit".characters SET update_requested_at = now() - interval '11 minutes'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let again = page(&h, &format!("/plugins/{ID}/character/{CHRIBBA}"), &owner).await;
    assert!(again.body.contains("Update now"), "{}", again.body);

    // Character Finder (the owner holds everything), with its search box.
    let finder = page(&h, &format!("/plugins/{ID}/finder?q=chrib"), &owner).await;
    assert_eq!(finder.status, StatusCode::OK, "{}", finder.body);
    assert!(finder.body.contains(&format!("character/{CHRIBBA}")));
    let searched = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/finder"),
            "_form=search&q=holding",
            &owner,
        ),
    )
    .await;
    assert_eq!(searched.status, StatusCode::OK, "{}", searched.body);
    assert!(
        searched.body.contains(&format!("character/{CHRIBBA}")),
        "{}",
        searched.body
    );
    let none = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/finder"),
            "_form=search&q=nobody",
            &owner,
        ),
    )
    .await;
    assert!(none.body.contains("No characters match."), "{}", none.body);

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
    let tab = page(
        &h,
        &format!("/plugins/{ID}/character/{CHRIBBA}/skills?_tab=2"),
        &owner,
    )
    .await;
    assert!(tab.body.contains("Guns"), "{}", tab.body);

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
    // Empty for over a day, it's real: everything is forgotten, mail too.
    sqlx::query(
        r#"UPDATE "plugin_tether.member-audit".characters SET seen_at = now() - interval '25 hours'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    sync(&h).await;
    let left: (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT count(*) FROM "plugin_tether.member-audit".characters),
                  (SELECT count(*) FROM "plugin_tether.member-audit".mails)"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));
}

/// aa-memberaudit's scopes: the Finder and sheets by corporation, alliance
/// or everything; mail only with `view_mail`, and every view audited.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn who_sees_what(db: PgPool) {
    let (h, owner) = synced(db).await;
    // A member in the Blue pilot's corporation (98133756).
    sqlx::query(
        r#"INSERT INTO "plugin_tether.member-audit".characters
           (character_id, name, corporation_id, alliance_id, synced_at)
           VALUES (90000020, 'Corp Mate', 98133756, 1695357456, now())"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let sheet = |id: i64| format!("/plugins/{ID}/character/{id}");
    let mail = format!("/plugins/{ID}/mail/{CHRIBBA}");

    // Basic access alone: only their own characters.
    grant(&h, &owner, "basic").await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &blue).await.status,
        StatusCode::OK
    );
    for uri in [sheet(CHRIBBA), sheet(90000020), mail.clone()] {
        assert_eq!(
            page(&h, &uri, &blue).await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
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
    // Nor can they ask for someone else's update.
    let refused = send(
        &h.app,
        form(
            &sheet(CHRIBBA),
            &format!("_form=update_character&character={CHRIBBA}"),
            &blue,
        ),
    )
    .await;
    assert!(refused.status.is_client_error(), "{}", refused.body);

    // The Finder without a scope: their own characters only (none here).
    grant(&h, &owner, "finder").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert_eq!(finder.status, StatusCode::OK, "{}", finder.body);
    assert!(
        !finder.body.contains("Chribba") && !finder.body.contains("Corp Mate"),
        "{}",
        finder.body
    );

    // Their main's corporation: the corporation mate, listed, but no sheet
    // without `characters`.
    grant(&h, &owner, "view_same_corporation").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert!(finder.body.contains("Corp Mate"), "{}", finder.body);
    assert!(!finder.body.contains("Chribba"), "{}", finder.body);
    assert!(!finder.body.contains(&sheet(90000020)), "{}", finder.body);
    assert_eq!(
        page(&h, &sheet(90000020), &blue).await.status,
        StatusCode::NOT_FOUND
    );
    grant(&h, &owner, "characters").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert!(finder.body.contains(&sheet(90000020)), "{}", finder.body);
    assert_eq!(
        page(&h, &sheet(90000020), &blue).await.status,
        StatusCode::OK
    );
    assert_eq!(
        page(&h, &sheet(CHRIBBA), &blue).await.status,
        StatusCode::NOT_FOUND
    );

    // Everyone: Chribba too, sheet and all, but no mail without view_mail.
    grant(&h, &owner, "view_everything").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert!(finder.body.contains(&sheet(CHRIBBA)), "{}", finder.body);
    let res = page(&h, &sheet(CHRIBBA), &blue).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(!res.body.contains(&mail), "{}", res.body);
    // They may ask for an update of someone else's character, a few an
    // hour.
    let ask = || {
        form(
            &sheet(CHRIBBA),
            &format!("_form=update_character&character={CHRIBBA}"),
            &blue,
        )
    };
    let res = send(&h.app, ask()).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let account = me(&h, &blue).await["account_id"].as_i64().unwrap();
    sqlx::query(
        r#"INSERT INTO "plugin_tether.member-audit".update_asks (account_id, character_id)
           SELECT $1, $2 FROM generate_series(1, 9)"#,
    )
    .bind(account)
    .bind(CHRIBBA)
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "plugin_tether.member-audit".characters SET update_requested_at = NULL"#)
        .execute(&h.db)
        .await
        .unwrap();
    let res = send(&h.app, ask()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("updates of other members"),
        "{}",
        res.body
    );
    let before = mail_views(&h).await.len();
    assert_eq!(page(&h, &mail, &blue).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        page(&h, &format!("{mail}/{MAIL}"), &blue).await.status,
        StatusCode::NOT_FOUND
    );

    // With view_mail: the Mail link, the mail, and every view audited
    // under their name.
    grant(&h, &owner, "view_mail").await;
    let res = page(&h, &sheet(CHRIBBA), &blue).await;
    assert!(
        res.body.contains(&format!(r#"<a href="{mail}">Mail</a>"#)),
        "{}",
        res.body
    );
    let one = page(&h, &format!("{mail}/{MAIL}"), &blue).await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.body);
    assert!(one.body.contains("Fleet at 19:00"), "{}", one.body);
    let views = mail_views(&h).await;
    let last = views.last().unwrap();
    assert_eq!(last.0, "gigX");
    assert_eq!(last.1["path"], format!("mail/{CHRIBBA}/{MAIL}"));
    // Refused views were recorded too (the log is written before the app
    // decides), and nothing else.
    assert_eq!(views.len(), before + 3, "{views:?}");
}

// ---- Secure Groups: Member Audit's filters ------------------------------------

/// A smart group (auto join, no grace) with one app filter; its id.
async fn smart_group(h: &Harness, owner: &str, name: &str, filter: &str) -> i64 {
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            owner,
            &format!(r#"{{"name":"{name}","internal":false,"hidden":false}}"#),
        ),
    )
    .await;
    let group = serde_json::from_str::<serde_json::Value>(&group.body).unwrap()["id"]
        .as_i64()
        .unwrap();
    send(
        &h.app,
        form(
            &format!("/admin/groups/{group}/smart"),
            "smart=on&auto_join=on&grace_days=0",
            owner,
        ),
    )
    .await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/groups/{group}/smart/filters"),
            &format!("kind=app&app={ID}/{filter}"),
            owner,
        ),
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{group}"),
        "{}",
        res.body
    );
    group
}

async fn report_filters(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:report_filters"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn member_audit_feeds_secure_groups(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    work(&h).await;
    let owner = register(&h, &owner).await;
    sync(&h).await;
    let account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    // A skill set Chribba can use.
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

    // Chribba has Gunnery 5, Small Hybrid Turret 3 and Tritanium.
    let gunnery = smart_group(&h, &owner, "Gunners", "skill&f_skill=gunnery&f_level=5").await;
    let turret = smart_group(
        &h,
        &owner,
        "Turret 4",
        "skill&f_skill=Small+Hybrid+Turret&f_level=4",
    )
    .await;
    let not_turret = smart_group(
        &h,
        &owner,
        "Not turret 4",
        "skill&f_skill=Small+Hybrid+Turret&f_level=4&reversed=on",
    )
    .await;
    let not_gunnery = smart_group(
        &h,
        &owner,
        "Not gunners",
        "skill&f_skill=Gunnery&f_level=1&reversed=on",
    )
    .await;
    let set = smart_group(&h, &owner, "Guns", "skill_set&f_skill_set=Guns").await;
    let asset = smart_group(&h, &owner, "Miners", "asset&f_item=Tritanium").await;
    let unknown = smart_group(&h, &owner, "Titans", "asset&f_item=Avatar").await;
    let not_unknown = smart_group(&h, &owner, "No titans", "asset&f_item=Avatar&reversed=on").await;
    let in_group = |group: i64| {
        let db = h.db.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM core.group_members WHERE group_id = $1 AND account_id = $2)",
            )
            .bind(group)
            .bind(account)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };

    // Nothing reported yet: the groups wait.
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(!in_group(gunnery).await);
    assert!(!in_group(not_turret).await);

    report_filters(&h).await;
    // Every synced character is reported for every setting, 0s included.
    let reported: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT name, config, value FROM core.plugin_filter_values WHERE plugin_id = $1 \
         AND character_id = $2 ORDER BY name, config",
    )
    .bind(ID)
    .bind(CHRIBBA)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(reported.len(), 6, "{reported:?}");
    let warnings = plugin_warnings(&h).await;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("no member character has an item named \"Avatar\""),
        "{warnings:?}"
    );

    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(in_group(gunnery).await, "trained to 5, at least 5");
    assert!(!in_group(turret).await, "trained to 3, not 4");
    assert!(in_group(not_turret).await, "reversed: not trained to 4");
    assert!(!in_group(not_gunnery).await, "reversed: has Gunnery");
    assert!(in_group(set).await, "meets the skill set");
    assert!(in_group(asset).await, "has Tritanium");
    assert!(!in_group(unknown).await, "an unknown item: nobody");
    assert!(in_group(not_unknown).await, "reversed, reported as 0");
    let listed = page(&h, "/groups", &owner).await.body;
    assert!(
        listed.contains("Member Audit: Has a skill trained to a level"),
        "{listed}"
    );

    // A character without whole data (a first sync that failed, assets
    // read only in part) isn't reported, so a reversed filter can't pass
    // on it.
    sqlx::query(
        r#"UPDATE "plugin_tether.member-audit".characters SET skills_at = NULL, assets_at = NULL"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    report_filters(&h).await;
    let reported: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.plugin_filter_values WHERE plugin_id = $1")
            .bind(ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(reported, 0);
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(!in_group(not_turret).await, "reversed, but not reported");
    assert!(!in_group(not_unknown).await, "reversed, but not reported");
    assert!(!in_group(gunnery).await);
    // Synced whole again: reported again.
    sqlx::query(r#"DELETE FROM "plugin_tether.member-audit".section_syncs"#)
        .execute(&h.db)
        .await
        .unwrap();
    sync(&h).await;
    report_filters(&h).await;
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(in_group(not_turret).await);
    assert!(in_group(asset).await);

    // An unknown name while some stored items have no name yet may be one
    // of them: not answered until they're named.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.member-audit".assets
           (character_id, item_id, type_id, quantity, location_id, location_flag)
           VALUES ($1, 1000000000099, 23913, 1, $2, 'Hangar')"#,
    )
    .bind(CHRIBBA)
    .bind(JITA_4_4)
    .execute(&h.db)
    .await
    .unwrap();
    let nyx = smart_group(&h, &owner, "Supers", "asset&f_item=Nyx&reversed=on").await;
    // Nor is a skill set that doesn't exist (deleted, say).
    let no_set = smart_group(
        &h,
        &owner,
        "No logi",
        "skill_set&f_skill_set=Logi&reversed=on",
    )
    .await;
    report_filters(&h).await;
    let answered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_filter_reports WHERE plugin_id = $1 \
         AND (config LIKE '%Nyx%' OR config LIKE '%Logi%')",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(answered, 0);
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(!in_group(nyx).await, "not answered: the group waits");
    assert!(!in_group(no_set).await, "not answered: the group waits");

    // More settings than one run may report: the rest go to a follow-up
    // run, and every one is reported.
    let extra = smart_group(&h, &owner, "Extra", "skill&f_skill=Gunnery&f_level=2").await;
    for i in 0..50 {
        let config = serde_json::json!({ "level": 1, "skill": format!("Skill {i}") }).to_string();
        sqlx::query(
            "INSERT INTO core.smart_filters (group_id, kind, config) VALUES ($1, 'app', $2)",
        )
        .bind(extra)
        .bind(serde_json::json!({
            "plugin": ID, "name": "skill", "config": config,
            "sum": false, "at_least": 0, "label": format!("Skill {i}"),
        }))
        .execute(&h.db)
        .await
        .unwrap();
    }
    report_filters(&h).await;
    let settings: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.plugin_filter_reports WHERE plugin_id = $1")
            .bind(ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(settings, 57);
    let values: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_filter_values WHERE plugin_id = $1 AND character_id = $2",
    )
    .bind(ID)
    .bind(CHRIBBA)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(values, 57);
}

// ---- the Dashboard ----------------------------------------------------------

/// With Member Audit and its basic access, the Dashboard is the pilot's
/// character audit: My Characters' cards first (Register Character leading
/// them), then AA's Characters and Membership panels. Without access, the
/// Dashboard as before.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_dashboard_is_the_character_audit(db: PgPool) {
    let (h, owner) = synced(db).await;
    let widget = format!("/dashboard/widgets/{ID}/0");
    let dashboard = page(&h, "/dashboard", &owner).await;
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    let body = &dashboard.body;
    let audit = body
        .find(&format!(r#"id="character-audit" hx-get="{widget}""#))
        .expect("My Characters leads");
    let characters = body.find(r#"id="characters""#).expect("AA's Characters");
    let membership = body.find(r#"aria-label="Membership""#).expect("Membership");
    assert!(audit < characters && characters < membership, "{body}");
    // AA's panels, compactly: Change Main and Add Character stay.
    assert!(body.contains("Change Main"), "{body}");
    assert!(body.contains("Add character"), "{body}");
    assert!(!body.contains(r#"aria-label="Summary""#), "{body}");
    // Drawn once, not again among the other widgets.
    assert_eq!(body.matches(&widget).count(), 1, "{body}");

    let cards = page(&h, &widget, &owner).await;
    assert_eq!(cards.status, StatusCode::OK, "{}", cards.body);
    let register = cards
        .body
        .find(r#"href="/register""#)
        .expect("Register Character");
    let first = cards
        .body
        .find(&format!(r#"href="/plugins/{ID}/character/{CHRIBBA}""#))
        .expect("Chribba's card");
    assert!(register < first, "{}", cards.body);
    assert!(
        cards.body.contains(&format!(
            "https://images.evetech.net/characters/{CHRIBBA}/portrait?size=128"
        )),
        "{}",
        cards.body
    );
    assert!(cards.body.contains("Wallets"), "{}", cards.body);

    // Someone without Member Audit's access: the Dashboard as it was.
    let guest = log_in_as(&h, "443630591:The Mittani", None).await;
    let theirs = page(&h, "/dashboard", &guest).await.body;
    assert!(!theirs.contains("character-audit"), "{theirs}");
    assert!(theirs.contains(r#"aria-label="Summary""#), "{theirs}");
    assert_eq!(
        page(&h, &widget, &guest).await.status,
        StatusCode::NOT_FOUND
    );
}

/// A big hangar: more than the host takes in one call's parameters, so it
/// is stored in pieces, and whole.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_big_hangar_is_stored_whole(db: PgPool) {
    let (h, _) = synced(db).await;
    for page in 1..=8_i64 {
        let items: Vec<serde_json::Value> = (0..1000_i64)
            .map(|n| {
                serde_json::json!({
                    "item_id": 1_000_000_100_000_i64 + page * 1000 + n, "type_id": 34,
                    "quantity": 1, "location_id": JITA_4_4, "location_flag": "Hangar",
                    "location_type": "station", "is_singleton": false,
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/characters/{CHRIBBA}/assets")))
            .and(wiremock::matchers::query_param("page", page.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "8")
                    .set_body_json(items),
            )
            .with_priority(1)
            .mount(&h.esi_server)
            .await;
    }
    sqlx::query(
        r#"DELETE FROM "plugin_tether.member-audit".section_syncs WHERE section = 'assets'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    sync(&h).await;
    let (stored, whole): (i64, bool) = sqlx::query_as(
        r#"SELECT (SELECT count(*) FROM "plugin_tether.member-audit".assets),
                  (SELECT assets_at IS NOT NULL FROM "plugin_tether.member-audit".characters)"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(stored, 8000, "{:?}", plugin_warnings(&h).await);
    assert!(whole);
}
