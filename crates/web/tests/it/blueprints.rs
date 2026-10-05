//! The Blueprints app (aa-blueprints) end to end: installed from its real
//! component and migration; a corporate owner added through Add owner; its
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

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(14);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_blueprints.sql");
    let named = plugin_file("migrations/0002_places_named.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_blueprints.sql", migration.as_bytes()),
        ("migrations/0002_places_named.sql", named.as_bytes()),
    ]);
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

/// Runs the app's three schedules now.
async fn sync(h: &Harness) {
    for name in ["sync_blueprints", "sync_jobs", "sync_places"] {
        sqlx::query(
            "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
        )
        .bind(format!("plugin:{ID}:{name}"))
        .execute(&h.db)
        .await
        .unwrap();
        tether_jobs::schedule::run_due(&h.db).await.unwrap();
        let mut registry = Registry::new();
        tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
        let config = WorkerConfig::default();
        while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
    }
}

fn asset(item: i64, type_id: i64, flag: &str, at: i64, kind: &str) -> serde_json::Value {
    serde_json::json!({
        "is_singleton": true, "item_id": item, "type_id": type_id, "quantity": 1,
        "location_flag": flag, "location_id": at, "location_type": kind
    })
}

async fn mount(h: &Harness) {
    let paged = |body: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(body)
    };
    // A Rifter original in a container in the office's second hangar, a
    // Merlin copy in the office's first.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/blueprints")))
        .respond_with(paged(serde_json::json!([
            {
                "item_id": 3001, "type_id": RIFTER_BP, "location_id": 2001,
                "location_flag": "Unlocked", "material_efficiency": 10,
                "time_efficiency": 20, "quantity": -1, "runs": -1
            },
            {
                "item_id": 3002, "type_id": MERLIN_BP, "location_id": 1001,
                "location_flag": "CorpSAG1", "material_efficiency": 8,
                "time_efficiency": 16, "quantity": -2, "runs": 10
            },
            {
                "item_id": 3003, "type_id": MERLIN_BP, "location_id": 1002,
                "location_flag": "CorpSAG3", "material_efficiency": 10,
                "time_efficiency": 20, "quantity": -1, "runs": -1
            }
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/industry/jobs")))
        .respond_with(paged(serde_json::json!([{
            "activity_id": 5, "blueprint_id": 3001, "blueprint_location_id": 2001,
            "blueprint_type_id": RIFTER_BP, "duration": 3600,
            "end_date": "2026-10-06T12:00:00Z", "facility_id": JITA,
            "installer_id": MITTANI, "job_id": 77, "location_id": JITA,
            "output_location_id": 2001, "runs": 5,
            "start_date": "2026-10-05T11:00:00Z", "status": "active"
        }])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .and(wiremock::matchers::query_param("page", "1"))
        .respond_with(paged(serde_json::json!([
            asset(1001, 27, "OfficeFolder", JITA, "station"),
            asset(2001, 17366, "CorpSAG2", 1001, "item"),
            asset(3001, RIFTER_BP, "Unlocked", 2001, "item"),
            asset(3002, MERLIN_BP, "CorpSAG1", 1001, "item"),
            asset(1002, 27, "OfficeFolder", KEEPSTAR, "item"),
            asset(3003, MERLIN_BP, "CorpSAG3", 1002, "item"),
        ])))
        .mount(&h.esi_server)
        .await;
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
    // The structure: refused once (the owner's character may not dock
    // there yet), then named.
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{KEEPSTAR}")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({ "error": "Forbidden" })),
        )
        .up_to_n_times(1)
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
    let res = post(
        &h,
        &owner,
        "settings",
        &format!("_form=settings&channel={DISCORD_PING_CHANNEL}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Settings saved, the app reads at once (there's room in ESI's
    // budget), in the background.
    let mut ran = 0;
    for _ in 0..100 {
        ran = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM core.audit_log WHERE action = 'schedule.run_now' \
             AND details->>'reason' = 'settings_saved'",
        )
        .fetch_one(&h.db)
        .await
        .unwrap();
        if ran >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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
    let one = page(&h, &format!("/plugins/{ID}/blueprint/3001"), &owner).await;
    assert!(one.body.contains("Copying"), "{}", one.body);
    assert!(one.body.contains("The Mittani"), "{}", one.body);

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
    let one = page(&h, &format!("/plugins/{ID}/blueprint/3001"), &pilot).await;
    assert!(!one.body.contains("Copying"), "{}", one.body);
    assert!(one.body.contains("Request copies"), "{}", one.body);

    // Someone in another corporation and alliance sees none of it.
    let outsider = log_in_as(&h, "1887431749:Outsider", None).await;
    grant(&h, OUTSIDER, &["basic_access", "request_blueprints"]).await;
    let library = page(&h, &format!("/plugins/{ID}"), &outsider).await;
    assert!(
        !library.body.contains("Rifter Blueprint"),
        "{}",
        library.body
    );
    let one = page(&h, &format!("/plugins/{ID}/blueprint/3001"), &outsider).await;
    assert_eq!(one.status, StatusCode::NOT_FOUND);

    // The pilot asks for copies of 5 runs: builders hear on Discord (not
    // in the bell, which would reach other corporations' builders). Asking
    // again while it's open is the same request.
    for _ in 0..2 {
        let res = post(&h, &pilot, "blueprint/3001", "_form=request&runs=5").await;
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
    let res = post(&h, &pilot, "blueprint/3002", "_form=request&runs=").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    grant(&h, OUTSIDER, &["manage_requests"]).await;
    let open = page(&h, &format!("/plugins/{ID}/open"), &outsider).await;
    assert!(!open.body.contains("Merlin Blueprint"), "{}", open.body);
}
