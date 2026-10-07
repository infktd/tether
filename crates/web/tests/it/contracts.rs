//! The Contracts app end to end: installed from its real component and
//! migration; a data source added through Add data source; the first read taken as
//! the backlog; then new contracts assigned to the corporation posted as
//! cards, their price checked against the Janice appraisal their
//! description links (read with the admin's key), one asking no ISK said
//! so, a completed one posted, each once.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "tether.contracts";
const CHRIBBA: i64 = 196379789;
const PILOT_A: i64 = 443630591;
const CORP: i64 = 1164409536;
const OTHER_CORP: i64 = 98000001;
const JITA: i64 = 60003760;
const AMARR: i64 = 60008494;
const TOWER: i64 = 1_022_734_985_679;
const BITUMENS: i64 = 62516;
const ZEOLITES: i64 = 62517;
const KEY: &str = "janice-test-key";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("contracts")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/contracts/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(12);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_contracts.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_contracts.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Adds Chribba as an owner (the SSO round trip).
async fn add_owner(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(asked.contains(&"esi-contracts.read_corporation_contracts.v1".to_owned()));
    assert!(asked.contains(&"esi-universe.read_structures.v1".to_owned()));
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

async fn sync(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:sync"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

fn at(offset: Duration) -> String {
    (Utc::now() + offset).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn contract(
    id: i64,
    assignee: i64,
    location: i64,
    status: &str,
    price: f64,
    title: &str,
) -> serde_json::Value {
    let finished = status == "finished";
    serde_json::json!({
        "contract_id": id,
        "type": "item_exchange",
        "assignee_id": assignee,
        "acceptor_id": if finished { CORP } else { 0 },
        "availability": "personal",
        "for_corporation": false,
        "issuer_id": PILOT_A,
        "issuer_corporation_id": OTHER_CORP,
        "start_location_id": location,
        "end_location_id": location,
        "status": status,
        "title": title,
        "volume": 3198.3,
        "collateral": 0.0,
        "reward": 0.0,
        "price": price,
        "date_issued": at(Duration::hours(-1)),
        "date_expired": at(Duration::days(28)),
        "date_completed": if finished { Some(at(Duration::minutes(-10))) } else { None },
    })
}

async fn contracts_answer(h: &Harness, contracts: serde_json::Value, times: Option<u64>) {
    let mock = Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contracts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(contracts),
        );
    match times {
        Some(n) => {
            mock.up_to_n_times(n)
                .with_priority(1)
                .mount(&h.esi_server)
                .await
        }
        None => mock.with_priority(2).mount(&h.esi_server).await,
    }
}

async fn mount(h: &Harness) {
    for id in [201, 202, 203, 205] {
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORP}/contracts/{id}/items")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "record_id": 1, "type_id": BITUMENS, "quantity": 13803, "is_included": true, "is_singleton": false },
                { "record_id": 2, "type_id": ZEOLITES, "quantity": 6359, "is_included": true, "is_singleton": false },
            ])))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{TOWER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "Jita - Union Terminal",
            "owner_id": OTHER_CORP,
            "solar_system_id": 30000142,
            "type_id": 35832,
            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
        })))
        .mount(&h.esi_server)
        .await;
    for (id, name) in [
        (JITA, "Jita IV - Moon 4 - Caldari Navy Assembly Plant"),
        (AMARR, "Amarr VIII (Oris) - Emperor Family Academy"),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/stations/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "station_id": id,
                "name": name,
                "system_id": 30000142,
                "type_id": 1531,
                "owner": 1000035,
                "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
                "max_dockable_ship_volume": 50000000.0,
                "office_rental_cost": 10000.0,
                "reprocessing_efficiency": 0.5,
                "reprocessing_stations_take": 0.05,
                "services": ["courier-missions"],
            })))
            .mount(&h.esi_server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": PILOT_A, "name": "Pilot A", "category": "character" },
            { "id": CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": OTHER_CORP, "name": "Customer Corp", "category": "corporation" },
            { "id": 30000142, "name": "Jita", "category": "solar_system" },
            { "id": BITUMENS, "name": "Compressed Bitumens", "category": "inventory_type" },
            { "id": ZEOLITES, "name": "Compressed Zeolites", "category": "inventory_type" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

async fn mount_janice(janice: &MockServer) {
    let ore = serde_json::json!([
        { "itemType": { "eid": BITUMENS, "name": "Compressed Bitumens" }, "amount": 13803 },
        { "itemType": { "eid": ZEOLITES, "name": "Compressed Zeolites" }, "amount": 6359 },
    ]);
    let other = serde_json::json!([
        { "itemType": { "eid": BITUMENS, "name": "Compressed Bitumens" }, "amount": 900000 },
    ]);
    for (code, buy, items) in [
        ("GoodAb", 1_005_000_000.0, &ore),
        ("BadCd1", 1_200_000_000.0, &ore),
        ("OtherX", 1_000_000_000.0, &other),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/api/rest/v2/appraisal/{code}")))
            .and(header("x-tether-test-host", "janice.e-351.com"))
            .and(header("x-apikey", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": code,
                "created": at(Duration::hours(-2)),
                "effectivePrices": { "totalBuyPrice": buy, "totalSplitPrice": buy, "totalSellPrice": buy },
                "items": items,
            })))
            .expect(1)
            .mount(janice)
            .await;
    }
}

async fn cards(h: &Harness) -> Vec<serde_json::Value> {
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

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    let url = if at.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{at}")
    };
    send(&h.app, form(&url, body, token)).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn contracts_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let janice = MockServer::start().await;
    h.plugins.route_http_to(&janice.address().to_string());
    mount_janice(&janice).await;
    mount(&h).await;

    // The admin's Janice key, as the app's secret.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/secrets/janice_api_key"),
            &format!("value={KEY}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // Discord: one channel, every kind of notice.
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
            .contains("A channel an admin assigned this app (Administration › Apps › Contracts)."),
        "{}",
        settings.body
    );
    let res = post(
        &h,
        &owner,
        "settings",
        &format!(
            "_form=settings&channel={DISCORD_PING_CHANNEL}&notify_new=on&notify_completed=on\
             &notify_ended=on&tolerance_percent=1"
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

    // The first read is the backlog: nothing announced.
    contracts_answer(
        &h,
        serde_json::json!([
            contract(200, CORP, JITA, "outstanding", 5_000_000.0, ""),
            contract(199, CORP, AMARR, "finished", 0.0, ""),
        ]),
        Some(1),
    )
    .await;
    let owner = add_owner(&h, &owner).await;
    sync(&h).await;
    assert!(cards(&h).await.is_empty());
    // Listed with its place's name, backlog or not.
    let listed = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        listed
            .body
            .contains("Amarr VIII (Oris) - Emperor Family Academy"),
        "{}",
        listed.body
    );
    assert!(!listed.body.contains("Location 6000"), "{}", listed.body);

    // Then: the backlog's one completed, three new, one not the
    // corporation's.
    contracts_answer(
        &h,
        serde_json::json!([
            contract(200, CORP, JITA, "finished", 5_000_000.0, ""),
            contract(201, CORP, TOWER, "outstanding", 0.0, "Ore for the buyback"),
            contract(
                202,
                CORP,
                JITA,
                "outstanding",
                1_000_000_000.0,
                "Buyback https://janice.e-351.com/a/GoodAb"
            ),
            contract(
                203,
                CORP,
                JITA,
                "outstanding",
                1_000_000_000.0,
                "Buyback janice.e-351.com/a/BadCd1"
            ),
            contract(204, OTHER_CORP, JITA, "outstanding", 1.0, ""),
            // Priced as the appraisal says, but it's of other ore.
            contract(
                205,
                CORP,
                JITA,
                "outstanding",
                1_000_000_000.0,
                "Buyback https://janice.e-351.com/a/OtherX"
            ),
        ]),
        None,
    )
    .await;
    sync(&h).await;
    let sent = cards(&h).await;
    assert_eq!(sent.len(), 5, "{sent:#?}");
    let by = |needle: &str| {
        sent.iter()
            .find(|c| c.to_string().contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {sent:#?}"))
            .clone()
    };
    let free = by("Jita - Union Terminal");
    assert_eq!(
        free["title"],
        "Item exchange contract to Jita - Union Terminal"
    );
    let description = free["description"].as_str().unwrap();
    assert!(
        description.starts_with(
            "Pilot A assigned your corporation a contract at Jita - Union Terminal.\n\
             No ISK asked · no appraisal linked in the description\nAccept before "
        ),
        "{description}"
    );
    assert_eq!(free["author"]["name"], "Otherworld Enterprises");
    assert_eq!(
        free["author"]["icon_url"],
        format!("https://images.evetech.net/corporations/{CORP}/logo?size=64")
    );
    assert_eq!(
        free["thumbnail"]["url"],
        format!("https://images.evetech.net/types/{BITUMENS}/icon?size=64")
    );
    assert_eq!(free["footer"]["text"], "Contracts · contract_assigned");
    let items = free["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Included items")
        .unwrap()["value"]
        .clone();
    assert_eq!(
        items,
        "Compressed Bitumens x13,803\nCompressed Zeolites x6,359"
    );
    let good = by("GoodAb");
    assert!(
        good["description"].as_str().unwrap().contains(
            "1.00B ISK asked · [Janice](https://janice.e-351.com/a/GoodAb) buy 1.00B ISK: ✅ matches"
        ),
        "{good}"
    );
    let bad = by("BadCd1");
    assert!(
        bad["description"]
            .as_str()
            .unwrap()
            .contains("❌ 16.7% under"),
        "{bad}"
    );
    assert_eq!(bad["color"], 0xe7_4c3c);
    // The issuer picks the appraisal: one of other items vouches for
    // nothing, whatever it's worth.
    let other = by("OtherX");
    assert!(
        other["description"]
            .as_str()
            .unwrap()
            .contains("❌ the appraisal isn't of the contract's items"),
        "{other}"
    );
    assert_eq!(other["color"], 0xe7_4c3c);
    let done = by("completed the contract");
    assert!(
        done["description"]
            .as_str()
            .unwrap()
            .starts_with("Otherworld Enterprises completed the contract."),
        "{done}"
    );
    assert_eq!(done["footer"]["text"], "Contracts · contract_delivered");

    // Each once, and each appraisal read once (the mocks expect one).
    sync(&h).await;
    assert_eq!(cards(&h).await.len(), 5);

    // The page: every contract with its check, for view_contracts only.
    let listed = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    for text in [
        "Matches",
        "16.7% under",
        "No ISK",
        "Doesn&#39;t vouch",
        "Jita - Union Terminal",
    ] {
        assert!(listed.body.contains(text), "{text}: {}", listed.body);
    }
    assert!(
        !listed.body.contains("Customer Corp contract"),
        "{}",
        listed.body
    );
    let pilot = log_in_as(&h, "443630591:Pilot A", None).await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            &h,
            &pilot,
            "settings",
            "_form=settings&tolerance_percent=50"
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

/// A busy buyback corporation has thousands of contracts in ESI's 30
/// days: more than one statement may carry, so they're stored in parts.
/// Contracts that can't be stored are that corporation's problem, on the
/// page; the run goes on.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_busy_corporation_is_stored_whole(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    for page in 1..=3_i64 {
        let contracts: Vec<serde_json::Value> = (0..1000_i64)
            .map(|n| {
                contract(
                    10_000 + page * 1000 + n,
                    CORP,
                    JITA,
                    "outstanding",
                    5_000_000.0,
                    "Buyback https://janice.e-351.com/a/GoodAb",
                )
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORP}/contracts")))
            .and(wiremock::matchers::query_param("page", page.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "3")
                    .set_body_json(contracts),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&h.esi_server)
            .await;
    }
    let owner = add_owner(&h, &owner).await;
    sync(&h).await;
    let (stored, synced, error): (i64, bool, Option<String>) = sqlx::query_as(
        r#"SELECT (SELECT count(*) FROM "plugin_tether.contracts".contracts),
                  synced_at IS NOT NULL, sync_error
           FROM "plugin_tether.contracts".settings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!((stored, synced, error), (3000, true, None));

    // One that can't be stored: said on the page, and the run finishes.
    let mut broken = contract(99, CORP, JITA, "outstanding", 1.0, "");
    broken["date_issued"] = serde_json::json!("not a time");
    contracts_answer(&h, serde_json::json!([broken]), None).await;
    sqlx::query(r#"UPDATE "plugin_tether.contracts".settings SET synced_at = NULL"#)
        .execute(&h.db)
        .await
        .unwrap();
    sync(&h).await;
    let (synced, error): (bool, Option<String>) = sqlx::query_as(
        r#"SELECT synced_at IS NOT NULL, sync_error FROM "plugin_tether.contracts".settings"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(synced);
    assert_eq!(
        error.as_deref(),
        Some("Otherworld Enterprises: contracts not stored")
    );
    let listed = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(
        listed.body.contains("contracts not stored"),
        "{}",
        listed.body
    );
}
