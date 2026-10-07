//! The Blueprints app (aa-blueprints) end to end: installed from its real
//! component and migration; a corporate data source added through Add data source; its
//! blueprints, running jobs and where they are read; the library as each
//! viewer may see it; and a request for copies taken, fulfilled and told
//! about, in the bell and on Discord.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.blueprints";
const CHRIBBA: i64 = 196379789;
const MITTANI: i64 = 443630591;
const OUTSIDER: i64 = 1887431749;
const CORP: i64 = 1164409536;
const JITA: i64 = 60003760;
const RIFTER_BP: i64 = 691;
const MERLIN_BP: i64 = 954;
const RIFTER: i64 = 587;
const MERLIN: i64 = 603;
const KEEPSTAR: i64 = 1_055_694_841_377;
const ATHANOR: i64 = 1_045_000_000_001;
/// Offices and containers: item ids in Upwell structures' range, as EVE
/// gives them today.
const OFFICE_JITA: i64 = 1_040_000_000_101;
const OFFICE_KEEPSTAR: i64 = 1_040_000_000_102;
const OFFICE_ATHANOR: i64 = 1_040_000_000_103;
const CONTAINER: i64 = 1_040_000_000_201;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("blueprints")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/blueprints/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// The app's migrations, in order, as its package has them.
fn migrations() -> Vec<(String, String)> {
    let dir = format!(
        "{}/../../plugins/blueprints/migrations",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let sql = plugin_file(&format!("migrations/{n}"));
            (format!("migrations/{n}"), sql)
        })
        .collect()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(14);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let component = component();
    let migrations = migrations();
    let mut files: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ];
    files.extend(
        migrations
            .iter()
            .map(|(n, sql)| (n.as_str(), sql.as_bytes())),
    );
    let bytes = testing::zip(&files);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Adds Chribba, a Director, as the corporate owner (the SSO round trip).
async fn add_owner(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    for scope in [
        "esi-corporations.read_blueprints.v1",
        "esi-industry.read_corporation_jobs.v1",
        "esi-assets.read_corporation_assets.v1",
    ] {
        assert!(asked.contains(&scope.to_owned()), "{asked:?}");
    }
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{CHRIBBA}:Chribba&state={state}"),
            &[(LOGIN, &login), (SESSION, owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

/// Runs one of the app's schedules now, and whatever is due with it.
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
}

async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// Runs the app's three schedules now.
async fn sync(h: &Harness) {
    sync_reading(h, 1).await;
}

/// [`sync`], then the places follow-ups the app queues while the
/// corporation's assets are read in the background: each released once
/// that read has asked for its `last` page.
async fn sync_reading(h: &Harness, last: u32) {
    for name in ["sync_blueprints", "sync_jobs", "sync_places"] {
        run_schedule(h, name).await;
    }
    for _ in 0..100 {
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'places_again' \
             AND state = 'queued'",
        )
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap();
        if queued == 0 {
            return;
        }
        assets_read(h, last).await;
        sqlx::query(
            "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 \
             AND job_key = 'places_again' AND state = 'queued'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
        work(h).await;
    }
    panic!("the places follow-ups never ended");
}

/// Waits until the corporation's assets' `last` page was asked for (0:
/// not at all).
async fn assets_read(h: &Harness, last: u32) {
    if last == 0 {
        return;
    }
    let at = format!("/corporations/{CORP}/assets");
    let page = last.to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let asked = h
            .esi_server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| {
                r.url.path() == at && r.url.query_pairs().any(|(k, v)| k == "page" && v == page)
            });
        if asked {
            // Its answer goes into the read just after.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "page {last} was never read"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn asset(item: i64, type_id: i64, flag: &str, at: i64, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "is_singleton": true, "item_id": item, "type_id": type_id, "quantity": 1,
        "location_flag": flag, "location_id": at, "location_type": kind
    })
}

fn paged(body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("x-pages", "1")
        .set_body_json(body)
}

fn blueprint(item: i64, type_id: i64, at: i64, flag: &str, runs: i64) -> serde_json::Value {
    serde_json::json!({
        "item_id": item, "type_id": type_id, "location_id": at, "location_flag": flag,
        "material_efficiency": 10, "time_efficiency": 20,
        "quantity": if runs > 0 { -2 } else { -1 }, "runs": runs
    })
}

/// The corporation's blueprints, as ESI lists them.
async fn mount_blueprints(h: &Harness, list: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/blueprints")))
        .respond_with(paged(list))
        .mount(&h.esi_server)
        .await;
}

async fn mount(h: &Harness) {
    // A Rifter original in a container in the Jita office's second
    // hangar, a Merlin copy in its first; others in the offices of the
    // corporation's own Keepstar and of an Athanor it rents in.
    mount_blueprints(
        h,
        serde_json::json!([
            blueprint(3001, RIFTER_BP, CONTAINER, "Unlocked", -1),
            {
                "item_id": 3002, "type_id": MERLIN_BP, "location_id": OFFICE_JITA,
                "location_flag": "CorpSAG1", "material_efficiency": 8,
                "time_efficiency": 16, "quantity": -2, "runs": 10
            },
            blueprint(3003, MERLIN_BP, OFFICE_KEEPSTAR, "CorpSAG3", -1),
            blueprint(3004, RIFTER_BP, OFFICE_ATHANOR, "CorpSAG4", -1),
        ]),
    )
    .await;
    // The Keepstar is the corporation's own, so it is one of its assets,
    // in space: its office is placed in it, not in its system.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .and(wiremock::matchers::query_param("page", "1"))
        .respond_with(paged(serde_json::json!([
            asset(OFFICE_JITA, 27, "OfficeFolder", JITA, "station"),
            asset(CONTAINER, 17366, "CorpSAG2", OFFICE_JITA, "item"),
            asset(3001, RIFTER_BP, "Unlocked", CONTAINER, "item"),
            asset(3002, MERLIN_BP, "CorpSAG1", OFFICE_JITA, "item"),
            asset(KEEPSTAR, 35834, "AutoFit", 30004759, "solar_system"),
            asset(OFFICE_KEEPSTAR, 27, "OfficeFolder", KEEPSTAR, "item"),
            asset(3003, MERLIN_BP, "CorpSAG3", OFFICE_KEEPSTAR, "item"),
            asset(OFFICE_ATHANOR, 27, "OfficeFolder", ATHANOR, "item"),
            asset(3004, RIFTER_BP, "CorpSAG4", OFFICE_ATHANOR, "item"),
        ])))
        .mount(&h.esi_server)
        .await;
    mount_world(h).await;
}

/// Jobs, a station, structures, names: what every test's corporation
/// reads besides its blueprints and assets.
async fn mount_world(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/industry/jobs")))
        .respond_with(paged(serde_json::json!([{
            "activity_id": 5, "blueprint_id": 3001, "blueprint_location_id": CONTAINER,
            "blueprint_type_id": RIFTER_BP, "duration": 3600,
            "end_date": "2026-10-06T12:00:00Z", "facility_id": JITA,
            "installer_id": MITTANI, "job_id": 77, "location_id": JITA,
            "output_location_id": CONTAINER, "runs": 5,
            "start_date": "2026-10-05T11:00:00Z", "status": "active"
        }])))
        .mount(&h.esi_server)
        .await;
    // Offices and containers are never taken for structures (their ids
    // are in the same range): they're found among the assets.
    for item in [OFFICE_JITA, OFFICE_KEEPSTAR, OFFICE_ATHANOR, CONTAINER] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/structures/{item}")))
            .respond_with(ResponseTemplate::new(403))
            .expect(0)
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/universe/stations/{JITA}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "station_id": JITA,
            "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
            "system_id": 30000142, "type_id": 1531, "owner": 1000035,
            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
            "max_dockable_ship_volume": 50000000.0, "office_rental_cost": 10000.0,
            "reprocessing_efficiency": 0.5, "reprocessing_stations_take": 0.05,
            "services": ["courier-missions"],
        })))
        .mount(&h.esi_server)
        .await;
    // The structure: refused to the owner's character and then to the
    // member Tether asks next (it may not dock there yet), then named.
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{KEEPSTAR}")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({ "error": "Forbidden" })),
        )
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{KEEPSTAR}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "1DQ1-A - Imperial Palace",
            "owner_id": CORP,
            "solar_system_id": 30004759,
            "type_id": 35834,
            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
        })))
        .with_priority(2)
        .mount(&h.esi_server)
        .await;
    // Another: refused to the owner's character, named through a member
    // who may dock there.
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{ATHANOR}")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({ "error": "Forbidden" })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{ATHANOR}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "Home - Athanor",
            "owner_id": 98000001,
            "solar_system_id": 30004759,
            "type_id": 35835,
            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
        })))
        .with_priority(2)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": RIFTER_BP, "name": "Rifter Blueprint", "category": "inventory_type" },
            { "id": MERLIN_BP, "name": "Merlin Blueprint", "category": "inventory_type" },
            { "id": RIFTER, "name": "Rifter", "category": "inventory_type" },
            { "id": MERLIN, "name": "Merlin", "category": "inventory_type" },
            { "id": 27, "name": "Office", "category": "inventory_type" },
            { "id": 17366, "name": "Station Container", "category": "inventory_type" },
            { "id": CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": MITTANI, "name": "The Mittani", "category": "character" },
            { "id": 30000142, "name": "Jita", "category": "solar_system" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "inventory_types": [
                { "id": RIFTER, "name": "Rifter" },
                { "id": MERLIN, "name": "Merlin" }
            ]
        })))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    let url = if at.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{at}")
    };
    send(&h.app, form(&url, body, token)).await
}

async fn grant(h: &Harness, character: i64, permissions: &[&str]) -> i64 {
    let account: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap();
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

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn blueprints_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    // Chribba's corporation is a member corporation.
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        CORP,
    )
    .await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let owner_account = grant(&h, CHRIBBA, &[]).await;
    install(&h, &owner).await;
    mount(&h).await;

    // Discord: new requests go to the ping channel.
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
    // Its help says where channels are given to the app: its own admin
    // page, not the Discord page.
    let settings = page(&h, &format!("/plugins/{ID}/settings"), &owner).await;
    assert!(
        settings
            .body
            .contains("A channel an admin assigned this app (Administration › Apps › Blueprints)."),
        "{}",
        settings.body
    );
    let res = post(
        &h,
        &owner,
        "settings",
        &format!("_form=settings&channel={DISCORD_PING_CHANNEL}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Settings saved, the app reads at once (there's room in ESI's
    // budget): its schedules are queued before the answer.
    let ran: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'schedule.run_now' \
         AND details->>'reason' = 'settings_saved'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(ran, 3, "one per schedule");
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;

    let owner = add_owner(&h, &owner).await;
    sync(&h).await;

    // The owner (a superuser) sees everything: the original with its
    // product's icon, where it is through the hangar and container, and
    // that it's in use.
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(library.status, StatusCode::OK, "{}", library.body);
    for want in [
        "Rifter Blueprint",
        "Merlin Blueprint",
        &format!("types/{RIFTER}/icon"),
        "Jita IV - Moon 4 - Caldari Navy Assembly Plant › Corp Hangar 2 › Station Container",
        "Corp Hangar 1",
        "In use",
        "Original",
        "Copy",
    ] {
        assert!(library.body.contains(want), "{want}: {}", library.body);
    }
    // The Athanor, refused to the owner, was named through a member at once.
    assert!(
        library.body.contains("Home - Athanor › Corp Hangar 4"),
        "{}",
        library.body
    );
    // The structure ESI refused is a placeholder for now, and the app's
    // log says why.
    assert!(
        library
            .body
            .contains(&format!("Structure {KEEPSTAR} › Corp Hangar 3")),
        "{}",
        library.body
    );
    let logs: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND message LIKE 'structure %'",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(
        logs.iter()
            .any(|l| l.contains("not named") && l.contains("403")),
        "{logs:?}"
    );
    // Tether asked through a member who granted the structure scope (the
    // only one here is Chribba), and remembers the refusal for a week.
    let misses: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.structure_name_misses WHERE structure_id = $1",
    )
    .bind(KEEPSTAR)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(misses, 1);
    // Within the hour it's tried again, and named.
    let schema: String =
        sqlx::query_scalar("SELECT schema_name FROM core.plugin_storage WHERE plugin_id = $1")
            .bind(ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"UPDATE "{schema}".places SET read_at = now() - interval '2 hours' WHERE NOT named"#
    )))
    .execute(&h.db)
    .await
    .unwrap();
    sync(&h).await;
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        library
            .body
            .contains("1DQ1-A - Imperial Palace › Corp Hangar 3"),
        "{}",
        library.body
    );
    // The name is kept for every app.
    let kept: Option<String> =
        sqlx::query_scalar("SELECT name FROM core.structure_names WHERE structure_id = $1")
            .bind(KEEPSTAR)
            .fetch_optional(&h.db)
            .await
            .unwrap();
    assert_eq!(kept.as_deref(), Some("1DQ1-A - Imperial Palace"));
    // The running job is in its row (view_industry_jobs), and each row
    // has a Request button that asks first.
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        library.body.contains("Copying until 2026-10-06 12:00"),
        "{}",
        library.body
    );
    assert!(library.body.contains(">Request<"), "{}", library.body);
    // It opens the request form in a popup (not drawn on the page), led by
    // which blueprint.
    assert!(
        library
            .body
            .contains("Copies of Rifter Blueprint, from Otherworld Enterprises."),
        "{}",
        library.body
    );
    assert!(
        library.body.contains(r#"<dialog class="dialog popup""#),
        "{}",
        library.body
    );
    assert!(library.body.contains("data-opens="), "{}", library.body);

    // A pilot in the corporation who may request, without the location or
    // job permissions: the blueprints, not where they are or who's using
    // them.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    // (Logging in read his corporation; he has since joined ours.)
    sqlx::query("UPDATE core.characters SET corporation_id = $1 WHERE id = $2")
        .bind(CORP)
        .bind(MITTANI)
        .execute(&h.db)
        .await
        .unwrap();
    let pilot_account = grant(&h, MITTANI, &["basic_access", "request_blueprints"]).await;
    let library = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert!(
        library.body.contains("Rifter Blueprint"),
        "{}",
        library.body
    );
    assert!(!library.body.contains("Corp Hangar 2"), "{}", library.body);
    assert!(!library.body.contains("Copying"), "{}", library.body);
    assert!(library.body.contains(">Request<"), "{}", library.body);
    // The search is the toolbar's, the app's own, in the address (its rows'
    // buttons still match).
    assert!(
        library
            .body
            .contains(r#"placeholder="Search blueprints and owners""#),
        "{}",
        library.body
    );
    let found = page(&h, &format!("/plugins/{ID}?q=Merlin%20Blue"), &pilot).await;
    assert!(found.body.contains("Merlin Blueprint"), "{}", found.body);
    assert!(!found.body.contains("Rifter Blueprint"), "{}", found.body);

    // Someone in another corporation and alliance sees none of it.
    let outsider = log_in_as(&h, "1887431749:Outsider", None).await;
    grant(&h, OUTSIDER, &["basic_access", "request_blueprints"]).await;
    let library = page(&h, &format!("/plugins/{ID}"), &outsider).await;
    assert!(
        !library.body.contains("Rifter Blueprint"),
        "{}",
        library.body
    );
    // Nor can they ask for one: the button isn't theirs.
    let res = post(&h, &outsider, "", "_form=request&item=3001").await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // The pilot asks for copies: builders hear on Discord (not
    // in the bell, which would reach other corporations' builders). Asking
    // again while it's open is the same request.
    for _ in 0..2 {
        let res = post(&h, &pilot, "", "_form=request&item=3001&runs=5").await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    assert!(notices(&h, owner_account).await.is_empty());
    let posted: Vec<serde_json::Value> = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["embeds"][0].clone())
        .collect();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0]["title"], "Copy requested: Rifter Blueprint");
    let mine = page(&h, &format!("/plugins/{ID}/requests"), &pilot).await;
    assert!(mine.body.contains("Rifter Blueprint"), "{}", mine.body);
    let schema: String =
        sqlx::query_scalar("SELECT schema_name FROM core.plugin_storage WHERE plugin_id = $1")
            .bind(ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    let id: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT max(id) FROM "{schema}".requests"#
    )))
    .fetch_one(&h.db)
    .await
    .unwrap();

    // The builder takes it, then fulfils it; the pilot hears each time.
    let open = page(&h, &format!("/plugins/{ID}/open"), &owner).await;
    assert!(open.body.contains("The Mittani"), "{}", open.body);
    let res = post(
        &h,
        &owner,
        "open",
        &format!("_form=mark&request={id}&to=in_progress"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &owner,
        "open",
        &format!("_form=mark&request={id}&to=fulfilled"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        notices(&h, pilot_account).await,
        [
            "Blueprints: Rifter Blueprint request in progress | Chribba has started producing copies for Rifter Blueprint.",
            "Blueprints: Rifter Blueprint request completed | Chribba has finished producing copies for Rifter Blueprint.",
        ]
    );
    let mine = page(&h, &format!("/plugins/{ID}/requests"), &pilot).await;
    assert!(mine.body.contains("No open requests"), "{}", mine.body);

    // An owner no longer in use is hidden at once (and forgotten a week
    // later, so a blip loses nothing).
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"UPDATE "{schema}".owners SET missing_since = now()"#
    )))
    .execute(&h.db)
    .await
    .unwrap();
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        !library.body.contains("Rifter Blueprint"),
        "{}",
        library.body
    );
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"UPDATE "{schema}".owners SET missing_since = NULL"#
    )))
    .execute(&h.db)
    .await
    .unwrap();

    // Only builders of the owner's corporation may act on a request: not
    // the outsider, even with the permission.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}?q=Merlin%20Blue"),
            "_form=request&item=3002",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // The popup's runs are kept (none for as many as allowed).
    let runs: Vec<(i64, Option<i32>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT item_id, runs FROM "{schema}".requests ORDER BY id"#
    )))
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(runs, vec![(3001, Some(5)), (3002, None)]);
    // Hidden values the page didn't offer aren't taken.
    let forged = post(&h, &pilot, "", "_form=request&item=999&runs=1").await;
    assert_eq!(forged.status, StatusCode::CONFLICT, "{}", forged.body);
    grant(&h, OUTSIDER, &["manage_requests"]).await;
    let open = page(&h, &format!("/plugins/{ID}/open"), &outsider).await;
    assert!(!open.body.contains("Merlin Blueprint"), "{}", open.body);
}

/// Structure names are asked only through Member characters: never Blue,
/// Guest or blacklisted ones, whatever the states' order.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn structure_names_ask_only_members(db: PgPool) {
    let h = harness(db, true).await;
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        CORP,
    )
    .await;
    cover(
        &h.db,
        tether_core::states::Builtin::Blue,
        tether_core::states::EntityKind::Corporation,
        1000167,
    )
    .await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    log_in_as(&h, "443630591:The Mittani", None).await;
    let outsider = log_in_as(&h, "1887431749:Outsider", None).await;
    // Everyone granted the structure scope; the outsider's corporation is
    // a member one too, but they're blacklisted; and Blue ranks above
    // Member.
    sqlx::query(
        "UPDATE core.character_tokens SET scopes = ARRAY['esi-universe.read_structures.v1']",
    )
    .execute(&h.db)
    .await
    .unwrap();
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        98133756,
    )
    .await;
    send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=1887431749&reason=Spy&blacklisted=on&linked=on",
            &owner,
        ),
    )
    .await;
    sqlx::query("UPDATE core.states SET priority = 999999 WHERE builtin = 'blue'")
        .execute(&h.db)
        .await
        .unwrap();
    let _ = outsider;
    let asked = tether_db::structure_names::candidates(
        &h.db,
        KEEPSTAR,
        "esi-universe.read_structures.v1",
        3600.0,
        10,
    )
    .await
    .unwrap();
    assert_eq!(asked, vec![CHRIBBA]);
}

/// Installs the app, makes Chribba's corporation a member one and adds
/// him as its corporate owner; his new session.
async fn set_up(h: &Harness) -> String {
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        CORP,
    )
    .await;
    let owner = log_in_owner(h, "196379789:Chribba").await;
    install(h, &owner).await;
    add_owner(h, &owner).await
}

/// More than 50 pages of assets (aa-blueprints reads them all): the
/// office and container holding a blueprint are on the last, read in the
/// background while the app asks again each minute.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_large_corporations_places_are_all_read(db: PgPool) {
    let h = harness(db, true).await;
    mount_blueprints(
        &h,
        serde_json::json!([blueprint(3001, RIFTER_BP, CONTAINER, "Unlocked", -1)]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .and(wiremock::matchers::query_param("page", "60"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "60")
                .set_body_json(serde_json::json!([
                    asset(OFFICE_JITA, 27, "OfficeFolder", JITA, "station"),
                    asset(CONTAINER, 17366, "CorpSAG2", OFFICE_JITA, "item"),
                    asset(3001, RIFTER_BP, "Unlocked", CONTAINER, "item"),
                ])),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "60")
                .set_body_json(serde_json::json!([])),
        )
        .with_priority(2)
        .mount(&h.esi_server)
        .await;
    mount_world(&h).await;
    let owner = set_up(&h).await;
    sync_reading(&h, 60).await;
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        library.body.contains(
            "Jita IV - Moon 4 - Caldari Navy Assembly Plant › Corp Hangar 2 › Station Container"
        ),
        "{}",
        library.body
    );
    // No problem was recorded on the way.
    assert!(
        !library.body.contains("The last read had a problem"),
        "{}",
        library.body
    );
}

/// A blueprint's location says why it has none: not looked for yet, or
/// looked for and not found among its owner's assets.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn places_say_not_read_yet_until_read_then_unknown_location(db: PgPool) {
    let h = harness(db, true).await;
    // An item id below Upwell structures' range, not among the assets.
    mount_blueprints(
        &h,
        serde_json::json!([blueprint(3005, MERLIN_BP, 9_000_000_001, "CorpSAG5", -1)]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .respond_with(paged(serde_json::json!([asset(
            OFFICE_JITA,
            27,
            "OfficeFolder",
            JITA,
            "station"
        )])))
        .mount(&h.esi_server)
        .await;
    mount_world(&h).await;
    let owner = set_up(&h).await;
    run_schedule(&h, "sync_blueprints").await;
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        library.body.contains("Not read yet › Corp Hangar 5"),
        "{}",
        library.body
    );
    sync(&h).await;
    let library = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        library.body.contains("Unknown location › Corp Hangar 5"),
        "{}",
        library.body
    );
}

/// Registers `character` ("id:Name") for the app through the SSO round
/// trip; the new session (logins rotate it).
async fn register(h: &Harness, token: &str, character: &str) -> String {
    let res = send(
        &h.app,
        form(&format!("/register/start?app={ID}"), "", token),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, token)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

/// Adds or removes one of the viewer's registered characters as a
/// personal owner (Owners).
async fn personal(h: &Harness, token: &str, form_name: &str, character: i64) {
    let res = post(
        h,
        token,
        "owners",
        &format!("_form={form_name}&character={character}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

async fn schema(h: &Harness) -> String {
    sqlx::query_scalar("SELECT schema_name FROM core.plugin_storage WHERE plugin_id = $1")
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// Two pilots' own libraries, each with 60 pages of assets: more than a
/// run's 100 ESI calls together. The one a run can't reach is read a
/// minute later, not left out until the run after next (and then again).
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn owners_a_run_cannot_reach_are_read_a_minute_later(db: PgPool) {
    let h = harness(db, true).await;
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        CORP,
    )
    .await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    for (character, item, container) in [
        (CHRIBBA, 4001, 1_040_000_000_301_i64),
        (MITTANI, 4002, 1_040_000_000_302),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character}/blueprints")))
            .respond_with(paged(serde_json::json!([blueprint(
                item, RIFTER_BP, container, "Unlocked", -1
            )])))
            .mount(&h.esi_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character}/industry/jobs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&h.esi_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character}/assets")))
            .and(wiremock::matchers::query_param("page", "60"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "60")
                    .set_body_json(serde_json::json!([asset(
                        container, 17366, "Hangar", JITA, "station"
                    )])),
            )
            .with_priority(1)
            .mount(&h.esi_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character}/assets")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "60")
                    .set_body_json(serde_json::json!([])),
            )
            .with_priority(2)
            .mount(&h.esi_server)
            .await;
    }
    mount_world(&h).await;
    let owner = register(&h, &owner, "196379789:Chribba").await;
    personal(&h, &owner, "add_owner", CHRIBBA).await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    grant(
        &h,
        MITTANI,
        &["basic_access", "add_personal_blueprint_owner"],
    )
    .await;
    let pilot = register(&h, &pilot, "443630591:The Mittani").await;
    personal(&h, &pilot, "add_owner", MITTANI).await;
    sync_reading(&h, 0).await;
    let schema = schema(&h).await;
    let placed: Vec<(i64, Option<i64>)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT item_id, place_id FROM "{schema}".blueprints ORDER BY item_id"#
    )))
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(placed, vec![(4001, Some(JITA)), (4002, Some(JITA))]);
    // The second was read by the follow-up, a minute after the first.
    let follow_ups: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM core.jobs WHERE plugin_id = $1 AND job_key = 'places_again' \
         AND state = 'succeeded'",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(follow_ups.len(), 1, "{follow_ups:?}");
    let error: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT sync_error FROM "{schema}".settings"#
    )))
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(error, None);
}
