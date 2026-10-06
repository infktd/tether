//! The Member Audit plugin end to end: bundled from its real component
//! and migrations, characters registered for it, a character synced
//! from mocked ESI (every section of the sheet), and AA's pages: My
//! Characters (the card grid, Register Character first), the Character
//! Sheet's pages and tabs, mail with the sheet and audited, the Character
//! Finder (with each character's main and state) scoped by the owner's
//! main's corporation or alliance, or everything, sharing, Skill sets,
//! reports and aa-memberaudit's settings.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing;
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

const MIGRATIONS: [&str; 5] = [
    "migrations/0001_member_audit.sql",
    "migrations/0002_complete_data.sql",
    "migrations/0003_character_sheet.sql",
    "migrations/0004_aa_settings.sql",
    "migrations/0005_data_exports.sql",
];

/// Member Audit as the image bundles it (`scripts/bundle-apps.sh`): its
/// manifest without the `[publisher]` table, and no signature. Only the
/// bundled Member Audit learns who owns each character.
fn package() -> Vec<u8> {
    let manifest = bundled_manifest();

    let migrations: Vec<String> = MIGRATIONS.iter().map(|m| plugin_file(m)).collect();
    let component = component();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ];
    for (name, sql) in MIGRATIONS.iter().zip(&migrations) {
        entries.push((name, sql.as_bytes()));
    }
    testing::zip(&entries)
}

/// The manifest as bundled: without the `[publisher]` table.
fn bundled_manifest() -> String {
    let mut skip = false;
    let manifest: String = plugin_file("plugin.toml")
        .lines()
        .filter(|line| {
            if line.starts_with('[') {
                skip = *line == "[publisher]";
            }
            !skip
        })
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(!manifest.contains("PUBLISHER_KEY"));
    manifest
}

/// A harness bundling Member Audit.
async fn bundling(db: PgPool) -> Harness {
    harness_with_bundled(db, vec![package()]).await
}

/// Installs the bundled Member Audit, after the review (which says what it
/// learns that no other app does).
async fn install(h: &Harness, owner: &str) {
    use sha2::Digest;
    let review = page(h, &format!("/admin/plugin-bundled/{ID}"), owner).await;
    assert!(
        review.body.contains("which characters share an account"),
        "{}",
        review.body
    );
    let sha: String = sha2::Sha256::digest(package())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-bundled/{ID}/approve"),
            &format!("package={sha}&reviewed=none"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/admin/plugins/{ID}"));
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

/// Registers Chribba for the app (its Register Character round trip);
/// returns the new session.
async fn register(h: &Harness, token: &str) -> String {
    let res = send(
        &h.app,
        form(&format!("/register/start?app={ID}"), "", token),
    )
    .await;
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
    let h = bundling(db).await;
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
            "/admin/permissions/set",
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
    // Roles aren't read: aa-memberaudit's MEMBERAUDIT_FEATURE_ROLES_ENABLED
    // is off by default.
    assert_eq!(sections, 23, "{:?}", plugin_warnings(&h).await);

    // My characters, which is the Dashboard: a row per character with its
    // portrait and facts, the totals, and Tether's Register Character last.
    let mine = page(&h, "/dashboard", &owner).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    let body = &mine.body;
    let register = body.find(&format!(
        r#"<a class="card grid-card grid-card-register" href="/register?app={ID}">"#
    ));
    let card = body.find(&format!("/plugins/{ID}/character/{CHRIBBA}"));
    assert!(
        register.is_some() && card.is_some() && card < register,
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
        // No "More" card: the app's views are in the bar Tether draws.
        format!(r#"<a href="/plugins/{ID}/skill-sets">Skill sets</a>"#),
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
    assert!(!sheet.contains("Corporation roles"), "roles are off");
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
    // Once a minute: the button is back after that.
    sqlx::query(
        r#"UPDATE "plugin_tether.member-audit".characters SET update_requested_at = now() - interval '61 seconds'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let again = page(&h, &format!("/plugins/{ID}/character/{CHRIBBA}"), &owner).await;
    assert!(again.body.contains("Update now"), "{}", again.body);

    // aa-memberaudit's settings, on the app's Settings page (`manage`):
    // roles on, a shorter retention and fewer mails kept.
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(
        settings.body.contains("MEMBERAUDIT_DATA_RETENTION_LIMIT"),
        "{}",
        settings.body
    );
    let refused = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&retention_days=3&max_mails=250&sharing_timeout_minutes=0",
            &owner,
        ),
    )
    .await;
    assert!(
        refused.status.is_client_error() || refused.body.contains("within its range"),
        "{}",
        refused.body
    );
    let saved = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&retention_days=30&max_mails=1&roles_enabled=on&sharing_timeout_minutes=0",
            &owner,
        ),
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let stored: (i32, i32, bool) = sqlx::query_as(
        r#"SELECT retention_days, max_mails, roles_enabled FROM "plugin_tether.member-audit".settings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(stored, (30, 1, true));
    sync(&h).await;
    let overview = page(
        &h,
        &format!("/plugins/{ID}/character/{CHRIBBA}?_tab=1"),
        &owner,
    )
    .await;
    assert!(overview.body.contains("Director"), "{}", overview.body);
    let mails: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.member-audit".mails"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(mails <= 1, "{mails}");
    // Off again: the roles read are forgotten.
    let saved = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/settings"),
            "_form=settings&retention_days=360&max_mails=250&sharing_timeout_minutes=0",
            &owner,
        ),
    )
    .await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let roles: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.member-audit".roles"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(roles, 0);

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

const CORP_MATE: i64 = 90000020;
const MATE_ALT: i64 = 90000021;
const OUTSIDER: i64 = 90000030;
const SPY_ALT: i64 = 90000031;

/// A Member account (the first character its main), each character
/// registered with Member Audit's scopes (Chribba's) and known to it.
/// Member holds Member Audit's basic access, as admins grant it: only
/// holders' characters are the app's.
async fn member_account(h: &Harness, characters: &[(i64, &str, i64, Option<i64>)]) {
    let mut tx = h.db.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO core.permission_grants (permission, state_id) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(format!("plugin.{ID}.basic_access"))
    .bind(MEMBER_STATE)
    .execute(&mut *tx)
    .await
    .unwrap();
    let account: i64 = sqlx::query_scalar(
        "INSERT INTO core.accounts (state_id, main_character_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(MEMBER_STATE)
    .bind(characters[0].0)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    for (id, name, corporation, alliance) in characters {
        sqlx::query(
            "INSERT INTO core.characters (id, account_id, name, corporation_id, alliance_id) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(account)
        .bind(name)
        .bind(corporation)
        .bind(alliance)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO core.character_tokens (character_id, refresh_token, scopes) \
             SELECT $1, refresh_token, scopes FROM core.character_tokens WHERE character_id = $2",
        )
        .bind(id)
        .bind(CHRIBBA)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO core.app_characters (plugin_id, character_id) VALUES ($1, $2)")
            .bind(ID)
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "plugin_tether.member-audit".characters
               (character_id, name, corporation_id, alliance_id, synced_at)
               VALUES ($1, $2, $3, $4, now())"#,
        )
        .bind(id)
        .bind(name)
        .bind(corporation)
        .bind(alliance)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

/// The Finder's table row naming `name`.
fn finder_row<'a>(body: &'a str, name: &str) -> &'a str {
    body.split("<tr")
        .find(|row| row.contains(name) && row.contains("</td>"))
        .unwrap_or_else(|| panic!("no row for {name}\n{body}"))
}

/// Member Audit installed from a signed package (a Tether bundling none)
/// isn't told who owns characters: corporation and alliance scopes then
/// list only the viewer's own characters.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_audit_not_bundled_scopes_to_your_own(db: PgPool) {
    use tether_plugins::testing::Key;
    cover(&db, Builtin::Member, EntityKind::Alliance, ALLIANCE).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
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
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    mount_esi(&h).await;
    work(&h).await;
    let owner = register(&h, &owner).await;
    member_account(
        &h,
        &[
            (CORP_MATE, "Corp Mate", 98133756, Some(1695357456)),
            (MATE_ALT, "Mate Alt", 98000002, None),
        ],
    )
    .await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    for permission in [
        "basic_access",
        "finder_access",
        "characters_access",
        "view_same_corporation",
    ] {
        grant(&h, &owner, permission).await;
    }
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert_eq!(finder.status, StatusCode::OK, "{}", finder.body);
    for name in ["Corp Mate", "Mate Alt", "Chribba"] {
        assert!(!finder.body.contains(name), "{name}\n{}", finder.body);
    }
    for id in [CORP_MATE, MATE_ALT] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/character/{id}"), &blue)
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
    let warnings = plugin_warnings(&h).await;
    assert!(
        warnings.iter().any(|w| w.contains("who owns characters")),
        "{warnings:?}"
    );
    // Nor does it stand in for the Dashboard: the account's own one, and
    // the package's main page at its own address.
    let dashboard = page(&h, "/dashboard", &owner).await;
    assert!(
        !dashboard.body.contains("Register another character"),
        "{}",
        dashboard.body
    );
    assert!(
        dashboard.body.contains(r#"id="characters""#),
        "{}",
        dashboard.body
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &owner).await.status,
        StatusCode::OK
    );
}

/// Skill sets has a page rule of its own (view_skill_sets), as the Finder,
/// Reports and Data export do: it opens with that alone, so the views bar
/// shows it only to those who may open it, and managing sets takes it
/// with manage.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn skill_sets_are_for_view_skill_sets(db: PgPool) {
    let (h, owner) = synced(db).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let at = format!("/plugins/{ID}/skill-sets");
    let add = || form(&at, "_form=add_set&name=Guns&skills=Gunnery+1", &blue);
    // manage alone: no page, and no adding.
    grant(&h, &owner, "manage").await;
    assert_eq!(page(&h, &at, &blue).await.status, StatusCode::NOT_FOUND);
    assert_ne!(send(&h.app, add()).await.status, StatusCode::SEE_OTHER);
    // view_skill_sets opens it, without basic_access; with manage, sets
    // are added there.
    grant(&h, &owner, "view_skill_sets").await;
    let sets = page(&h, &at, &blue).await;
    assert_eq!(sets.status, StatusCode::OK, "{}", sets.body);
    assert_eq!(send(&h.app, add()).await.status, StatusCode::SEE_OTHER);
}

/// aa-memberaudit's scopes: the Finder and sheets by corporation, alliance
/// or everything; mail with the sheet, and every view audited.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn who_sees_what(db: PgPool) {
    let (h, owner) = synced(db).await;
    // A member whose main is in the Blue pilot's corporation (98133756),
    // with an alt elsewhere; and one whose main is elsewhere, with an alt
    // in it.
    member_account(
        &h,
        &[
            (CORP_MATE, "Corp Mate", 98133756, Some(1695357456)),
            (MATE_ALT, "Mate Alt", 98000002, None),
        ],
    )
    .await;
    member_account(
        &h,
        &[
            (OUTSIDER, "Outsider", 98000003, None),
            (SPY_ALT, "Spy Alt", 98133756, Some(1695357456)),
        ],
    )
    .await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let sheet = |id: i64| format!("/plugins/{ID}/character/{id}");
    let mail = format!("/plugins/{ID}/mail/{CHRIBBA}");

    // Basic access alone: only their own characters, and no Skill sets
    // (view_skill_sets) or Settings (manage).
    grant(&h, &owner, "basic_access").await;
    for uri in ["skill-sets", "settings"] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/{uri}"), &blue)
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let home = page(&h, "/dashboard", &blue).await;
    assert_eq!(home.status, StatusCode::OK);
    // Nor their views: the bar shows what they may open.
    assert!(
        !home.body.contains(&format!("/plugins/{ID}/skill-sets")),
        "{}",
        home.body
    );
    for uri in [sheet(CHRIBBA), sheet(CORP_MATE), mail.clone()] {
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
    grant(&h, &owner, "finder_access").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert_eq!(finder.status, StatusCode::OK, "{}", finder.body);
    for name in ["Chribba", "Corp Mate", "Mate Alt", "Outsider", "Spy Alt"] {
        assert!(!finder.body.contains(name), "{name}\n{}", finder.body);
    }

    // Their main's corporation, by the owner's main as in aa-memberaudit:
    // the corporation mate and his alt in another corporation, but not the
    // alt in their corporation whose main isn't. Listed, with each one's
    // main and state, but no sheet without `characters`.
    grant(&h, &owner, "view_same_corporation").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    for name in ["Corp Mate", "Mate Alt"] {
        assert!(finder.body.contains(name), "{name}\n{}", finder.body);
    }
    for name in ["Chribba", "Outsider", "Spy Alt"] {
        assert!(!finder.body.contains(name), "{name}\n{}", finder.body);
    }
    for header in [">Main<", ">Main organisation<", ">State<"] {
        assert!(finder.body.contains(header), "{header}\n{}", finder.body);
    }
    let alt = finder_row(&finder.body, "Mate Alt");
    assert!(
        alt.contains(&format!("characters/{CORP_MATE}/portrait")) && alt.contains("Corp Mate"),
        "{alt}"
    );
    assert!(alt.contains("corporations/98133756/logo"), "{alt}");
    assert!(alt.contains(">Member<"), "{alt}");
    // Searching finds characters by their main's name too.
    let found = page(&h, &format!("/plugins/{ID}/finder?q=corp+mate"), &blue).await;
    assert!(found.body.contains("Mate Alt"), "{}", found.body);
    assert!(!finder.body.contains(&sheet(CORP_MATE)), "{}", finder.body);
    for id in [CORP_MATE, MATE_ALT] {
        assert_eq!(
            page(&h, &sheet(id), &blue).await.status,
            StatusCode::NOT_FOUND
        );
    }
    // Reports, the same scope, with their own permission (reports_access).
    sqlx::query(r#"INSERT INTO "plugin_tether.member-audit".skill_sets (name) VALUES ('Anyone')"#)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/reports"), &blue)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    grant(&h, &owner, "reports_access").await;
    let reports = page(&h, &format!("/plugins/{ID}/reports"), &blue).await;
    assert_eq!(reports.status, StatusCode::OK, "{}", reports.body);
    assert!(reports.body.contains("Mate Alt"), "{}", reports.body);
    assert!(!reports.body.contains("Spy Alt"), "{}", reports.body);
    grant(&h, &owner, "characters_access").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert!(finder.body.contains(&sheet(CORP_MATE)), "{}", finder.body);
    assert!(finder.body.contains(&sheet(MATE_ALT)), "{}", finder.body);
    for id in [CORP_MATE, MATE_ALT] {
        assert_eq!(page(&h, &sheet(id), &blue).await.status, StatusCode::OK);
    }
    for id in [SPY_ALT, OUTSIDER] {
        assert_eq!(
            page(&h, &sheet(id), &blue).await.status,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        page(&h, &sheet(CHRIBBA), &blue).await.status,
        StatusCode::NOT_FOUND
    );

    // Everyone: Chribba too, sheet and all, mail included (as in
    // aa-memberaudit, mail is part of the sheet). No Skill sets tab without
    // view_skill_sets.
    grant(&h, &owner, "view_everything").await;
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &blue).await;
    assert!(finder.body.contains(&sheet(CHRIBBA)), "{}", finder.body);
    let res = page(&h, &sheet(CHRIBBA), &blue).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains(&format!(r#"<a href="{mail}">Mail</a>"#)),
        "{}",
        res.body
    );
    let skills = page(&h, &format!("{}/skills", sheet(CHRIBBA)), &blue).await;
    assert!(!skills.body.contains("Skill sets"), "{}", skills.body);
    grant(&h, &owner, "view_skill_sets").await;
    let skills = page(&h, &format!("{}/skills", sheet(CHRIBBA)), &blue).await;
    assert!(skills.body.contains("Skill sets"), "{}", skills.body);
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
    assert!(res.body.contains("updates of other pilots"), "{}", res.body);

    // The mail, and every view audited under their name.
    let before = mail_views(&h).await.len();
    let one = page(&h, &format!("{mail}/{MAIL}"), &blue).await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.body);
    assert!(one.body.contains("Fleet at 19:00"), "{}", one.body);
    let views = mail_views(&h).await;
    let last = views.last().unwrap();
    assert_eq!(last.0, "gigX");
    assert_eq!(last.1["path"], format!("mail/{CHRIBBA}/{MAIL}"));
    assert_eq!(views.len(), before + 1, "{views:?}");
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
            "smart=on&auto_join=on",
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
    let h = bundling(db).await;
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

/// With Member Audit and its basic access, the Dashboard is its My
/// characters (DESIGN.md, Dashboard): the Dashboard's header, the
/// account's state and groups under the title, the totals and a row per
/// character with Tether's status and Make main, Register another
/// character, and nothing else. The app's own main page is the Dashboard,
/// its sidebar link goes, and its other pages lead back to it. Without
/// access, the account's own Dashboard.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_dashboard_is_the_character_audit(db: PgPool) {
    let (h, owner) = synced(db).await;
    let dashboard = page(&h, "/dashboard", &owner).await;
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    let body = &dashboard.body;
    for part in [
        r#"<h1 class="page-title">Dashboard</h1>"#,
        r#"<div class="page-eyebrow">Account</div>"#,
        r#"class="page-membership""#,
        "Wallets",
        "Register another character",
        "Change Main with EVE login",
        "grid-card-foot",
    ] {
        assert!(body.contains(part), "{part}: {body}");
    }
    let register = body
        .find(&format!(r#"href="/register?app={ID}""#))
        .expect("Register another character");
    let first = body
        .find(&format!(r#"href="/plugins/{ID}/character/{CHRIBBA}""#))
        .expect("Chribba's row");
    assert!(first < register, "{body}");
    assert!(
        body.contains(&format!(
            "https://images.evetech.net/characters/{CHRIBBA}/portrait?size=128"
        )),
        "{body}"
    );
    // Chribba is the main already: nothing to make main. No other app's
    // boxes, no watermark, and one way to add a character.
    assert!(!body.contains(">Make main<"), "{body}");
    assert!(!body.contains("widget-title"), "{body}");
    assert!(!body.contains("Viewing as"), "{body}");
    assert!(!body.contains("Add character"), "{body}");
    // The views of the app the viewer may open (the owner: all of them),
    // the first being the Dashboard; the Dashboard marked in the sidebar,
    // and no sidebar link of the app's own.
    assert!(
        body.contains(r#"<a href="/dashboard" aria-current="page">My characters</a>"#),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            r#"<a href="/plugins/{ID}/finder">Character finder</a>"#
        )),
        "{body}"
    );
    assert!(
        body.contains(r#"<a href="/dashboard" class="nav-item" aria-current="page">"#),
        "{body}"
    );
    assert!(
        !body.contains(&format!(r#"<a href="/plugins/{ID}" class="nav-item""#)),
        "{body}"
    );

    // The app's main page is the Dashboard; its content alone (a reload)
    // still comes from its address, as the Dashboard.
    let main = send(&h.app, get(&format!("/plugins/{ID}"), &[(SESSION, &owner)])).await;
    assert_eq!(main.status, StatusCode::SEE_OTHER);
    assert_eq!(main.location(), "/dashboard");
    let mut req = get(&format!("/plugins/{ID}"), &[(SESSION, &owner)]);
    req.headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    req.headers_mut()
        .insert("hx-trigger", "plugin-content".parse().unwrap());
    let reload = send(&h.app, req).await;
    assert_eq!(reload.status, StatusCode::OK, "{}", reload.body);
    assert!(
        reload
            .body
            .contains(r#"<h1 class="page-title">Dashboard</h1>"#),
        "{}",
        reload.body
    );
    assert!(!reload.body.contains("<html"), "{}", reload.body);
    // Its other pages keep the Dashboard marked and lead back to it.
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &owner).await;
    assert!(
        finder
            .body
            .contains(r#"<a href="/dashboard" class="nav-item" aria-current="page">"#),
        "{}",
        finder.body
    );
    assert!(
        finder
            .body
            .contains(r#"<a href="/dashboard">My characters</a>"#),
        "{}",
        finder.body
    );

    // Someone without Member Audit's access: the account's own Dashboard.
    let guest = log_in_as(&h, "443630591:The Mittani", None).await;
    let theirs = page(&h, "/dashboard", &guest).await.body;
    assert!(!theirs.contains("Register another character"), "{theirs}");
    assert!(theirs.contains(r#"id="characters""#), "{theirs}");
    assert!(theirs.contains("Add character"), "{theirs}");
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

/// aa-memberaudit's sharing: a pilot with `share_characters` shares their
/// own character from its sheet, and holders of `view_shared_characters`
/// (recruiters) find it and open it, mail included and audited, until it
/// stops being shared or the sharing timeout passes.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pilots_share_characters_with_recruiters(db: PgPool) {
    let (h, owner) = synced(db).await;
    let recruiter = log_in_as(&h, "1887431749:gigX", None).await;
    for permission in ["basic_access", "finder_access", "view_shared_characters"] {
        grant(&h, &owner, permission).await;
    }
    let sheet = format!("/plugins/{ID}/character/{CHRIBBA}");
    let share = |form_name: &'static str, token: &str| {
        form(
            &sheet,
            &format!("_form={form_name}&character={CHRIBBA}"),
            token,
        )
    };
    assert_eq!(
        page(&h, &sheet, &recruiter).await.status,
        StatusCode::NOT_FOUND
    );
    // Only its pilot shares it.
    let refused = send(&h.app, share("share_character", &recruiter)).await;
    assert!(refused.status.is_client_error(), "{}", refused.body);
    let own = page(&h, &sheet, &owner).await.body;
    assert!(own.contains(">Share<"), "{own}");
    let shared = send(&h.app, share("share_character", &owner)).await;
    assert_eq!(shared.status, StatusCode::SEE_OTHER, "{}", shared.body);
    assert!(page(&h, &sheet, &owner).await.body.contains("Stop sharing"));

    // The recruiter finds it and opens it, mail too.
    let finder = page(&h, &format!("/plugins/{ID}/finder"), &recruiter).await;
    assert!(finder.body.contains(&sheet), "{}", finder.body);
    let res = page(&h, &sheet, &recruiter).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Shared"), "{}", res.body);
    assert!(!res.body.contains("Stop sharing"), "{}", res.body);
    let mail = page(
        &h,
        &format!("/plugins/{ID}/mail/{CHRIBBA}/{MAIL}"),
        &recruiter,
    )
    .await;
    assert_eq!(mail.status, StatusCode::OK, "{}", mail.body);
    assert_eq!(mail_views(&h).await.last().unwrap().0, "gigX");

    // Shared by someone who no longer owns it (sold on): not shared for
    // its new pilot.
    sqlx::query(r#"UPDATE "plugin_tether.member-audit".characters SET shared_by_main = 1"#)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        page(&h, &sheet, &recruiter).await.status,
        StatusCode::NOT_FOUND
    );
    sqlx::query(r#"UPDATE "plugin_tether.member-audit".characters SET shared_by_main = $1"#)
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(page(&h, &sheet, &recruiter).await.status, StatusCode::OK);

    // Stopped: gone again.
    let stopped = send(&h.app, share("unshare_character", &owner)).await;
    assert_eq!(stopped.status, StatusCode::SEE_OTHER, "{}", stopped.body);
    assert_eq!(
        page(&h, &sheet, &recruiter).await.status,
        StatusCode::NOT_FOUND
    );

    // The sharing timeout (MEMBERAUDIT_SHARING_TIMEOUT) ends a share at the
    // next sync.
    send(&h.app, share("share_character", &owner)).await;
    sqlx::raw_sql(
        r#"UPDATE "plugin_tether.member-audit".settings SET sharing_timeout_minutes = 60;
           UPDATE "plugin_tether.member-audit".characters SET shared_at = now() - interval '2 hours'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    sync(&h).await;
    assert_eq!(
        page(&h, &sheet, &recruiter).await.status,
        StatusCode::NOT_FOUND
    );
}

/// 0.2 named aa-memberaudit's permissions `basic`, `finder` and
/// `characters`: an upgrade moves their grants to AA's names, and drops
/// `view_mail` (mail goes with the sheet).
#[test]
fn upgrading_from_0_2_moves_the_renamed_grants() {
    use tether_plugins::manifest::{Manifest, permission_renames};
    let now = Manifest::parse(&bundled_manifest()).unwrap();
    let was = Manifest::parse(
        "[plugin]\nid = \"tether.member-audit\"\nname = \"Member Audit\"\nversion = \"0.2.0\"\n\
         host_api = \"1\"\n\n[permissions]\nbasic = \"b\"\nfinder = \"f\"\ncharacters = \"c\"\n\
         view_same_corporation = \"s\"\nview_same_alliance = \"a\"\nview_everything = \"e\"\n\
         view_mail = \"m\"\nmanage = \"m\"\n",
    )
    .unwrap();
    let mut renames = permission_renames(Some(&was), &now);
    renames.sort();
    assert_eq!(
        renames,
        [
            ("basic".to_owned(), "basic_access".to_owned()),
            ("characters".to_owned(), "characters_access".to_owned()),
            ("finder".to_owned(), "finder_access".to_owned()),
        ]
    );
    for permission in [
        "reports_access",
        "view_skill_sets",
        "share_characters",
        "view_shared_characters",
    ] {
        assert!(now.permissions.contains_key(permission), "{permission}");
    }
    assert!(!now.permissions.contains_key("view_mail"));
}

/// aa-memberaudit's data exports: CSV files of every character's
/// contracts, contract items and wallet journal, for `exports_access`.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn data_exports_are_csv_files_for_exports_access(db: PgPool) {
    let (h, owner) = synced(db).await;
    let at = format!("/plugins/{ID}/data-export");
    let res = page(&h, &at, &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // The daily schedule has already built them.
    for text in [
        "Data export",
        "Wallet journal",
        "Contract item",
        "/plugins/tether.member-audit/downloads/wallet-journal",
        "updated in the last hour",
    ] {
        assert!(res.body.contains(text), "{text}: {}", res.body);
    }
    // At most once an hour, as AA's.
    let again = send(
        &h.app,
        form(&at, "_form=update_export&topic=contract", &owner),
    )
    .await;
    assert!(
        again.body.contains("updated in the last hour"),
        "{}",
        again.body
    );
    sqlx::query(
        r#"UPDATE "plugin_tether.member-audit".export_runs SET asked_at = now() - interval '61 minutes'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    for topic in ["contract", "contract-item", "wallet-journal"] {
        let res = send(
            &h.app,
            form(&at, &format!("_form=update_export&topic={topic}"), &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert!(res.body.contains("has been started"), "{}", res.body);
    }
    work(&h).await;

    let journal = page(
        &h,
        &format!("/plugins/{ID}/downloads/wallet-journal"),
        &owner,
    )
    .await;
    assert_eq!(journal.status, StatusCode::OK, "{}", journal.body);
    assert!(
        journal.body.starts_with(
            "date,owner character,owner corporation,entry id,ref type,first party,second party,\
             amount,balance,context_id,context_id_type,tax,tax_receiver,description,reason\r\n"
        ),
        "{}",
        journal.body
    );
    for text in [
        "2026-09-24 12:00:00,Chribba,",
        ",1,Bounty Prizes,",
        ",1000,1234567.89,",
        ",Bounty,",
    ] {
        assert!(journal.body.contains(text), "{text}: {}", journal.body);
    }
    let contracts = page(&h, &format!("/plugins/{ID}/downloads/contract"), &owner).await;
    for text in [
        ",Item Exchange,Outstanding,2026-09-20 00:00:00,",
        ",Public,Chribba,",
        "Tritanium for sale",
    ] {
        assert!(contracts.body.contains(text), "{text}: {}", contracts.body);
    }
    let items = page(
        &h,
        &format!("/plugins/{ID}/downloads/contract-item"),
        &owner,
    )
    .await;
    assert!(items.body.contains(",1,"), "{}", items.body);
    assert!(
        items.body.contains(",1000,yes,no,no,no,no,"),
        "{}",
        items.body
    );

    // Not for pilots without exports_access.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = page(&h, &format!("/plugins/{ID}/downloads/contract"), &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}
