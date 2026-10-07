//! Buyback end to end (aa-buybackprogram): installed from its real
//! component and migration; a manager added as a data source; a location
//! and a program set up; a paste priced from Fuzzwork with Tether's
//! built-in static data; the contract made with its tracking number read,
//! checked and announced; its acceptance told to the seller.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "tether.buyback";
const CHRIBBA: i64 = 196379789;
const CORP: i64 = 1164409536;
const JITA_STATION: i64 = 60003760;
const TRITANIUM: i64 = 34;
const PYERITE: i64 = 35;
const VELDSPAR: i64 = 1230;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("buyback")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/buyback/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(14);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_buyback.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_buyback.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Adds Chribba as a manager (the SSO round trip).
async fn add_manager(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
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
    res.cookie_value(SESSION)
}

async fn run(h: &Harness, schedule: &str) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:{schedule}"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

fn at(offset: Duration) -> String {
    (Utc::now() + offset).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Fuzzwork's aggregates for Jita, as the app asks for them.
async fn mount_fuzzwork(fuzzwork: &MockServer) {
    let price = |buy: f64, sell: f64| {
        serde_json::json!({
            "buy": { "max": buy.to_string(), "percentile": buy.to_string() },
            "sell": { "min": sell.to_string(), "percentile": sell.to_string() },
        })
    };
    let mut answer = serde_json::Map::new();
    answer.insert(TRITANIUM.to_string(), price(4.0, 5.0));
    answer.insert(PYERITE.to_string(), price(10.0, 12.0));
    answer.insert(VELDSPAR.to_string(), price(15.0, 18.0));
    Mock::given(method("GET"))
        .and(path("/aggregates/"))
        .and(header("x-tether-test-host", "market.fuzzwork.co.uk"))
        .and(wiremock::matchers::query_param(
            "station",
            JITA_STATION.to_string(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Object(answer)))
        .mount(fuzzwork)
        .await;
}

async fn mount_esi(h: &Harness, contracts: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/contracts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(contracts),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contracts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([])),
        )
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(format!(r"^/characters/{CHRIBBA}/contracts/\d+/items$")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "record_id": 1, "type_id": TRITANIUM, "quantity": 600, "is_included": true, "is_singleton": false },
            { "record_id": 2, "type_id": TRITANIUM, "quantity": 400, "is_included": true, "is_singleton": false },
            { "record_id": 3, "type_id": VELDSPAR, "quantity": 100, "is_included": true, "is_singleton": false },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/markets/prices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "type_id": TRITANIUM, "average_price": 4.5, "adjusted_price": 4.4 },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": JITA_STATION, "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant", "category": "station" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

fn contract(id: i64, status: &str, price: f64, title: &str) -> serde_json::Value {
    serde_json::json!({
        "contract_id": id,
        "type": "item_exchange",
        "assignee_id": CHRIBBA,
        "acceptor_id": 0,
        "availability": "personal",
        "for_corporation": false,
        "issuer_id": CHRIBBA,
        "issuer_corporation_id": CORP,
        "start_location_id": JITA_STATION,
        "end_location_id": JITA_STATION,
        "status": status,
        "title": title,
        "volume": 20.0,
        "collateral": 0.0,
        "reward": 0.0,
        "price": price,
        "date_issued": at(Duration::hours(-1)),
        "date_expired": at(Duration::days(13)),
        "date_completed": if status == "finished" { Some(at(Duration::minutes(-5))) } else { None },
    })
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn buyback_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let fuzzwork = MockServer::start().await;
    h.plugins.route_http_to(&fuzzwork.address().to_string());
    mount_fuzzwork(&fuzzwork).await;
    let owner = add_manager(&h, &owner).await;

    // A location, then a program there: 10% tax, refined ore at 90%.
    let res = post(
        &h,
        &owner,
        "manage/locations",
        &format!(
            "_form=location&owner={CHRIBBA}&name=Jita+4-4&system=Jita&structure_id={JITA_STATION}"
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location: i64 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.buyback".locations"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = post(
        &h,
        &owner,
        "manage/program/new",
        &format!(
            "_form=program&name=Ore+buyback&owner={CHRIBBA}&expiration=2+Weeks&price_type=Buy&tax=10\
             &hauling_fuel_cost=0&allow_all_items=on&use_raw_ore_value=on&use_refined_value=on\
             &refining_rate=90&density_threshold=0&density_tax=0&t1_refining_rate=50\
             &loc_{location}=on&notify_manager=on&restricted_states="
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let program: i64 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.buyback".programs"#)
        .fetch_one(&h.db)
        .await
        .unwrap();

    // It's listed, with its terms.
    let index = page(&h, &format!("/plugins/{ID}"), &owner).await;
    let logs: Vec<String> = sqlx::query_scalar("SELECT message FROM core.plugin_logs ORDER BY id")
        .fetch_all(&h.db)
        .await
        .unwrap();
    assert_eq!(index.status, StatusCode::OK, "{logs:?}");
    assert!(index.body.contains("Ore buyback"), "{}", index.body);
    let calculator = page(&h, &format!("/plugins/{ID}/program/{program}"), &owner).await;
    let logs: Vec<String> = sqlx::query_scalar("SELECT message FROM core.plugin_logs ORDER BY id")
        .fetch_all(&h.db)
        .await
        .unwrap();
    assert_eq!(calculator.status, StatusCode::OK, "{logs:?}");
    assert!(
        calculator.body.contains("refined at 90%"),
        "{}",
        calculator.body
    );

    // A paste that isn't from the inventory is refused, nothing kept.
    let res = post(
        &h,
        &owner,
        &format!("program/{program}"),
        "_form=calculate&items=Tritanium+1000&donation=0",
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("only accepts copy pasted item formats"),
        "{}",
        res.body
    );
    let kept: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.buyback".trackings"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(kept, 0);

    // 1000 Tritanium at 4 less 10%; 100 Veldspar refined: 400 × 0.9 ×
    // 4 = 1440 beats raw 1500? No: raw is 100 × 15 = 1500, so raw wins.
    let paste = "Tritanium%091%2C000%0AVeldspar%09100%0ANo+Such+Thing%095";
    let res = post(
        &h,
        &owner,
        &format!("program/{program}"),
        &format!("_form=calculate&items={paste}&donation=0&notes=thanks"),
    )
    .await;
    let logs: Vec<String> = sqlx::query_scalar("SELECT message FROM core.plugin_logs ORDER BY id")
        .fetch_all(&h.db)
        .await
        .unwrap();
    assert_eq!(res.status, StatusCode::OK, "{logs:?}");
    assert!(res.body.contains("No Such Thing not found"), "{}", res.body);
    let (number, net): (String, f64) = sqlx::query_as(
        r#"SELECT tracking_number, net_price::float8 FROM "plugin_tether.buyback".trackings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(number.starts_with("aa-bbp-0-"), "{number}");
    assert!(res.body.contains(&number), "{}", res.body);
    // (4000 + 1500) × 0.9.
    assert!((net - 4950.0).abs() < 0.01, "{net}");
    let items: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.buyback".tracking_items"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(items, 2, "the unknown line isn't part of the contract");

    // The contract, made with the tracking number at the right price
    // (its Tritanium in two stacks): read, checked, the manager told.
    mount_esi(
        &h,
        serde_json::json!([contract(700, "outstanding", 4950.0, &number)]),
    )
    .await;
    run(&h, "contracts").await;
    let (status, location_name): (String, Option<String>) = sqlx::query_as(
        r#"SELECT status, location_name FROM "plugin_tether.buyback".contracts WHERE contract_id = 700"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(status, "outstanding");
    assert_eq!(
        location_name.as_deref(),
        Some("Jita IV - Moon 4 - Caldari Navy Assembly Plant")
    );
    let flags: Vec<String> = sqlx::query_scalar(
        r#"SELECT header FROM "plugin_tether.buyback".contract_flags WHERE contract_id = 700 ORDER BY id"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    // Matching items (stacks merged), price, location and receiver: only
    // the seller's note.
    assert_eq!(flags, vec!["Note from seller".to_owned()]);
    let notices: Vec<String> =
        sqlx::query_scalar("SELECT title FROM core.notifications ORDER BY id")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert!(
        notices
            .iter()
            .any(|t| t.contains("New buyback contract assigned for program Ore buyback")),
        "{notices:?}"
    );

    // Accepted: the seller hears so.
    h.esi_server.reset().await;
    mount_esi(
        &h,
        serde_json::json!([contract(700, "finished", 4950.0, &number)]),
    )
    .await;
    run(&h, "contracts").await;
    let notices: Vec<String> =
        sqlx::query_scalar("SELECT title FROM core.notifications ORDER BY id")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert!(
        notices
            .iter()
            .any(|t| t.contains("Your buyback contract has been accepted")),
        "{notices:?}"
    );

    // A type pasted twice is one row; a pilot keeps at most 20
    // calculations without a contract, the contracted one apart.
    for _ in 0..21 {
        let res = post(
            &h,
            &owner,
            &format!("program/{program}"),
            "_form=calculate&items=Tritanium%095%0ATritanium%095&donation=0",
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    }
    let open: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.buyback".trackings WHERE contract_id IS NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(open, 20);
    let rows: Vec<i64> = sqlx::query_scalar(
        r#"SELECT i.quantity FROM "plugin_tether.buyback".tracking_items i
           JOIN "plugin_tether.buyback".trackings t ON t.id = i.tracking_id
           WHERE t.contract_id IS NULL"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(
        rows.len() == 20 && rows.iter().all(|q| *q == 10),
        "{rows:?}"
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            r#"SELECT exists(SELECT 1 FROM "plugin_tether.buyback".trackings WHERE tracking_number = $1)"#
        )
        .bind(&number)
        .fetch_one(&h.db)
        .await
        .unwrap()
    );
}

/// A program restricted to a state the viewer isn't in: not listed, and
/// its calculator not found (AA's calculator didn't check, B2).
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn restricted_programs_are_only_for_whom_they_are(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let owner = add_manager(&h, &owner).await;
    let res = post(
        &h,
        &owner,
        "manage/locations",
        "_form=location&owner=196379789&name=Anywhere",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location: i64 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.buyback".locations"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = post(
        &h,
        &owner,
        "manage/program/new",
        &format!(
            "_form=program&name=Officers&owner={CHRIBBA}&expiration=2+Weeks&price_type=Buy&tax=0\
             &hauling_fuel_cost=0&allow_all_items=on&use_raw_ore_value=on&refining_rate=0\
             &density_threshold=0&density_tax=0&t1_refining_rate=50&loc_{location}=on\
             &restricted_states=Some+Other+State"
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let program: i64 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.buyback".programs"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let pilot = log_in_as(&h, "443630591:Pilot A", None).await;
    let account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = 443630591")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind(format!("plugin.{ID}.basic_access"))
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
    let index = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert_eq!(index.status, StatusCode::OK, "{}", index.body);
    assert!(!index.body.contains("Officers"), "{}", index.body);
    let calculator = page(&h, &format!("/plugins/{ID}/program/{program}"), &pilot).await;
    assert_eq!(calculator.status, StatusCode::NOT_FOUND);
    // Its manager still uses it.
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/program/{program}"), &owner)
            .await
            .status,
        StatusCode::OK
    );
}
