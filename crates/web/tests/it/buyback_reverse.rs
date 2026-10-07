//! Buyback's reverse buyback (aa-buybackprogram's reverse programs) end
//! to end: installed from its real component and migration, a manager
//! added through Add data source, a reverse program made in its editor,
//! the hangar stock read through the host's background asset read, a
//! pilot's cart checked out into a request (lowered to what's free after
//! another's reservation), released, contracted, matched and flagged, and
//! the statistics.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "tether.buyback";
const CHRIBBA: i64 = 196379789;
const MITTANI: i64 = 443630591;
const OUTSIDER: i64 = 1887431749;
const CORP: i64 = 1164409536;
const JITA: i64 = 60003760;
const OFFICE: i64 = 1_040_000_000_101;
const BOX: i64 = 1_040_000_000_201;
const TRITANIUM: i64 = 34;
const PYERITE: i64 = 35;
const MEXALLON: i64 = 36;
const RIFTER: i64 = 587;
const STATION_CONTAINER: i64 = 17366;
const CONTRACT: i64 = 9001;

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
    let key = Key::new(21);
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

/// Adds Chribba, a Director, as a manager (the SSO round trip).
async fn add_owner(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(asked.contains(&"esi-assets.read_corporation_assets.v1".to_owned()));
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

async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

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

/// The stock read's follow-ups, each released once the corporation's
/// assets were read in the background, until none is left.
async fn follow_ups(h: &Harness) {
    for _ in 0..100 {
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'hangars_again' \
             AND state = 'queued'",
        )
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap();
        if queued == 0 {
            return;
        }
        assets_read(h).await;
        sqlx::query(
            "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 \
             AND job_key = 'hangars_again' AND state = 'queued'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
        work(h).await;
    }
    panic!("the stock read's follow-ups never ended");
}

/// Waits until the corporation's assets' page was asked for.
async fn assets_read(h: &Harness) {
    let at = format!("/corporations/{CORP}/assets");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let asked = h
            .esi_server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == at);
        if asked {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the assets were never read"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn at(offset: Duration) -> String {
    (Utc::now() + offset).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn asset(item: i64, type_id: i64, flag: &str, location: i64, quantity: i64) -> serde_json::Value {
    serde_json::json!({
        "is_singleton": quantity == 0, "item_id": item, "type_id": type_id,
        "quantity": quantity.max(1), "location_flag": flag, "location_id": location,
        "location_type": if location == JITA { "station" } else { "item" }
    })
}

fn paged(body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("x-pages", "1")
        .set_body_json(body)
}

/// The corporation's Jita office: Tritanium in two stacks, an assembled
/// Rifter and a named container in its seventh division, Pyerite in that
/// container, and Mexallon in the first division; its division names,
/// wallets, Fuzzwork's prices and no contracts yet.
async fn mount(h: &Harness) -> MockServer {
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/assets")))
        .respond_with(paged(serde_json::json!([
            asset(OFFICE, 27, "OfficeFolder", JITA, 0),
            asset(2001, TRITANIUM, "CorpSAG7", OFFICE, 1200),
            asset(2002, TRITANIUM, "CorpSAG7", OFFICE, 800),
            asset(2003, RIFTER, "CorpSAG7", OFFICE, 0),
            asset(BOX, STATION_CONTAINER, "CorpSAG7", OFFICE, 0),
            asset(2004, PYERITE, "Unlocked", BOX, 500),
            asset(2005, MEXALLON, "CorpSAG1", OFFICE, 100),
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/corporations/{CORP}/assets/names")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "item_id": BOX, "name": "Ore box" }
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/divisions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hangar": [{ "division": 7, "name": "Sales" }],
            "wallet": [{ "division": 1, "name": "Master" }]
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/wallets")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{ "division": 1, "balance": 1000.0 }])),
        )
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CHRIBBA, "name": "Chribba", "category": "character" },
            { "id": MITTANI, "name": "The Mittani", "category": "character" },
            { "id": CORP, "name": "Otherworld Enterprises", "category": "corporation" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_contracts(h, serde_json::json!([]), 9).await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contracts")))
        .respond_with(paged(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    let fuzzwork = MockServer::start().await;
    h.plugins.route_http_to(&fuzzwork.address().to_string());
    Mock::given(method("GET"))
        .and(path("/aggregates/"))
        .and(header("x-tether-test-host", "market.fuzzwork.co.uk"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "34": { "buy": { "percentile": "4.00" }, "sell": { "percentile": "5.00" } },
            "35": { "buy": { "percentile": "8.00" }, "sell": { "percentile": "10.00" } },
            "587": { "buy": { "percentile": "400000" }, "sell": { "percentile": "500000" } }
        })))
        .mount(&fuzzwork)
        .await;
    fuzzwork
}

/// The manager's character's contracts (lower `priority` wins).
async fn mount_contracts(h: &Harness, list: serde_json::Value, priority: u8) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/contracts")))
        .respond_with(paged(list))
        .with_priority(priority)
        .mount(&h.esi_server)
        .await;
}

/// The pilot's contract to the manager: paying `reward` and asking for
/// Tritanium.
fn contract(title: &str, status: &str, reward: f64) -> serde_json::Value {
    serde_json::json!({
        "contract_id": CONTRACT,
        "type": "item_exchange",
        "assignee_id": CHRIBBA,
        "acceptor_id": 0,
        "availability": "personal",
        "for_corporation": false,
        "issuer_id": MITTANI,
        "issuer_corporation_id": 1000167,
        "start_location_id": JITA,
        "end_location_id": JITA,
        "status": status,
        "title": title,
        "volume": 15.0,
        "collateral": 0.0,
        "reward": reward,
        "price": 0.0,
        "date_issued": at(Duration::hours(-1)),
        "date_expired": at(Duration::days(13)),
    })
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
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

async fn schema(h: &Harness) -> String {
    sqlx::query_scalar("SELECT schema_name FROM core.plugin_storage WHERE plugin_id = $1")
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// A location at the Jita station, as Manage › Locations adds it.
async fn add_location(h: &Harness, account: i64) -> i64 {
    let schema = schema(h).await;
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"INSERT INTO "{schema}".locations (owner_character, name, system_id, structure_id, created_by)
           VALUES ($1, 'Jita 4-4', 30000142, $2, $3) RETURNING id"#
    )))
    .bind(CHRIBBA)
    .bind(JITA)
    .bind(account)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn stock(h: &Harness, program: i64) -> Vec<(i64, i64)> {
    let schema = schema(h).await;
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT type_id, quantity FROM "{schema}".hangar_stock WHERE program_id = $1 ORDER BY type_id"#
    )))
    .bind(program)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// The request a pilot's account made last: its id and number.
async fn request_of(h: &Harness, account: i64) -> (i64, String) {
    let schema = schema(h).await;
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT id, tracking_number FROM "{schema}".reverse_trackings
           WHERE issuer_account = $1 ORDER BY id DESC LIMIT 1"#
    )))
    .bind(account)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn notices(h: &Harness, account: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT title FROM core.notifications WHERE account_id = $1 AND plugin_id = $2 ORDER BY id",
    )
    .bind(account)
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

/// Installs the app with Chribba as its manager and a Jita location;
/// the owner's session, account and the location.
async fn set_up(h: &Harness) -> (String, i64, i64, MockServer) {
    cover(
        &h.db,
        tether_core::states::Builtin::Member,
        tether_core::states::EntityKind::Corporation,
        CORP,
    )
    .await;
    let owner = log_in_owner(h, "196379789:Chribba").await;
    let account = grant(h, CHRIBBA, &[]).await;
    install(h, &owner).await;
    let fuzzwork = mount(h).await;
    let owner = add_owner(h, &owner).await;
    // Adding a data source runs the app's schedules.
    work(h).await;
    let location = add_location(h, account).await;
    (owner, account, location, fuzzwork)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reverse_buyback_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, owner_account, location, _fuzzwork) = set_up(&h).await;

    // The editor: the divisions by number until their names are read
    // (with the wallets, for managers of programs), and AA's checks.
    let editor = page(&h, &format!("/plugins/{ID}/manage/reverse/new"), &owner).await;
    assert_eq!(editor.status, StatusCode::OK, "{}", editor.body);
    assert!(editor.body.contains("7th Division"), "{}", editor.body);
    assert!(editor.body.contains("Jita: Jita 4-4"), "{}", editor.body);
    let loc = format!("loc_{location}");
    let body = form_body(
        &editor.body,
        "reverse_program",
        &[
            ("name", "Hangar sales"),
            ("stock_source", "containers"),
            (&loc, "on"),
        ],
    );
    let res = post(&h, &owner, "manage/reverse/new", &body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Not saved"), "{}", res.body);
    assert!(
        res.body.contains(
            "Select at least one container, or change the stock source to a whole hangar division."
        ),
        "{}",
        res.body
    );
    // A whole division: the seventh, with a 10% markup; the manager hears
    // of new requests.
    let body = form_body(
        &editor.body,
        "reverse_program",
        &[
            ("name", "Hangar sales"),
            ("hangar_division", "7"),
            ("markup", "10"),
            ("notify_manager", "on"),
            (&loc, "on"),
        ],
    );
    let res = post(&h, &owner, "manage/reverse/new", &body).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let schema = schema(&h).await;
    let program: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT id FROM "{schema}".reverse_programs WHERE name = 'Hangar sales'"#
    )))
    .fetch_one(&h.db)
    .await
    .unwrap();

    // Saving queued the stock read: the assets are read in the background
    // first, then the stock is what's loose in the seventh division (two
    // stacks merged, the assembled Rifter included), never the container,
    // what's in it, or the first division.
    work(&h).await;
    follow_ups(&h).await;
    assert_eq!(
        stock(&h, program).await,
        vec![(TRITANIUM, 2000), (RIFTER, 1)]
    );
    let (name, held): (String, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT name, structure_id FROM "{schema}".containers WHERE item_id = $1"#
    )))
    .bind(BOX)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!((name.as_str(), held), ("Ore box", JITA));

    // The manager's list shows it, read, by its division's name once the
    // wallets' read brought it.
    run_schedule(&h, "wallets").await;
    let list = page(&h, &format!("/plugins/{ID}/manage/reverse"), &owner).await;
    assert!(list.body.contains("Hangar sales"), "{}", list.body);
    assert!(list.body.contains("Sales"), "{}", list.body);

    // A pilot with basic access buys: the stock priced at Jita sell plus
    // the markup, with Add opening the quantity in a popup.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let pilot_account = grant(&h, MITTANI, &["basic_access"]).await;
    let index = page(&h, &format!("/plugins/{ID}/reverse"), &pilot).await;
    assert!(index.body.contains("Hangar sales"), "{}", index.body);
    assert!(index.body.contains("Sell price +10%"), "{}", index.body);
    let picker = page(&h, &format!("/plugins/{ID}/reverse/{program}"), &pilot).await;
    assert_eq!(picker.status, StatusCode::OK, "{}", picker.body);
    for want in [
        "Tritanium",
        "Rifter",
        "5.50",
        "How many Tritanium? 2000 available",
    ] {
        assert!(picker.body.contains(want), "{want}: {}", picker.body);
    }
    for unwanted in ["Pyerite", "Mexallon", "Station Container"] {
        assert!(
            !picker.body.contains(unwanted),
            "{unwanted}: {}",
            picker.body
        );
    }
    // The toolbar's search and category filter are the page's own.
    let found = page(
        &h,
        &format!("/plugins/{ID}/reverse/{program}?category=Ship"),
        &pilot,
    )
    .await;
    assert!(found.body.contains("Rifter"), "{}", found.body);
    assert!(!found.body.contains(">Tritanium<"), "{}", found.body);

    // Into the cart: all the Tritanium (asking for more than there is
    // stops at what's free) and the Rifter, which they take out again.
    let picker_at = format!("reverse/{program}");
    for body in [
        format!("_form=add&type_id={TRITANIUM}&quantity=1500"),
        format!("_form=add&type_id={TRITANIUM}&quantity=900"),
        format!("_form=add&type_id={RIFTER}&quantity=1"),
        format!("_form=remove_cart&type_id={RIFTER}"),
    ] {
        let res = post(&h, &pilot, &picker_at, &body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{body}: {}", res.body);
    }
    let picker = page(&h, &format!("/plugins/{ID}/{picker_at}"), &pilot).await;
    assert!(picker.body.contains("Your cart"), "{}", picker.body);
    assert!(picker.body.contains("Check total price"), "{}", picker.body);
    let cart: Vec<(i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT type_id, quantity FROM "{schema}".reverse_carts WHERE account_id = $1"#
    )))
    .bind(pilot_account)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(cart, vec![(TRITANIUM, 2000)]);

    // Meanwhile the manager reserves 500 of it.
    let res = post(
        &h,
        &owner,
        &picker_at,
        &format!("_form=add&type_id={TRITANIUM}&quantity=500"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &owner, &picker_at, "_form=checkout&notes=").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (owners_request, owners_number) = request_of(&h, owner_account).await;

    // So the pilot's request is lowered to what's free, with their notes.
    let res = post(
        &h,
        &pilot,
        &picker_at,
        "_form=checkout&notes=Deliver+to+the+Jita+office",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (request, number) = request_of(&h, pilot_account).await;
    assert!(number.starts_with("aa-bbp-R-"), "{number}");
    assert_eq!(
        res.location(),
        format!("/plugins/{ID}/reverse/tracking/{number}?lowered=34.2000")
    );
    let shown = page(&h, res.location(), &pilot).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    for want in [
        "Requested 2000 but only 1500 available after reservations.",
        "I will pay",
        "The items below are reserved for you for 48 hours.",
        "Tick Also request items from buyer",
        "On the price page set I will pay to 8 250 ISK",
        "Tritanium 1500",
        "Copy item list",
        "Release reserved items",
        "Deliver to the Jita office",
    ] {
        assert!(shown.body.contains(want), "{want}: {}", shown.body);
    }
    let items: Vec<(i64, i64, f64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        r#"SELECT type_id, quantity, buy_value::float8 FROM "{schema}".reverse_tracking_items
           WHERE tracking_id = $1"#
    )))
    .bind(request)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(items, vec![(TRITANIUM, 1500, 5.5)]);
    let cart_left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT count(*) FROM "{schema}".reverse_carts"#
    )))
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(cart_left, 0);

    // Someone else's request isn't the pilot's to release.
    let res = post(
        &h,
        &pilot,
        &format!("reverse/tracking/{owners_number}"),
        &format!("_form=release&request={owners_request}"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // The manager releases theirs: its 500 are free again.
    let res = post(
        &h,
        &owner,
        &format!("reverse/tracking/{owners_number}"),
        &format!("_form=release&request={owners_request}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/plugins/{ID}/reverse/{program}"));
    let picker = page(&h, &format!("/plugins/{ID}/{picker_at}"), &pilot).await;
    assert!(
        picker.body.contains("How many Tritanium? 500 available"),
        "{}",
        picker.body
    );

    // The pilot's statistics list it as pending, with Release.
    let mine = page(&h, &format!("/plugins/{ID}/reverse/stats"), &pilot).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert!(mine.body.contains(&number), "{}", mine.body);
    assert!(mine.body.contains("Tritanium × 1500"), "{}", mine.body);

    // The pilot contracts it, paying a little less: matched, its asked-for
    // items read, flagged, and the manager told.
    mount_contracts(
        &h,
        serde_json::json!([contract(&number, "outstanding", 8000.0)]),
        5,
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/contracts/{CONTRACT}/items")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "record_id": 1, "type_id": TRITANIUM, "quantity": 1500, "is_included": false, "is_singleton": false }
        ])))
        .mount(&h.esi_server)
        .await;
    run_schedule(&h, "contracts").await;
    let linked: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT contract_id FROM "{schema}".reverse_trackings WHERE id = $1"#
    )))
    .bind(request)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(linked, Some(CONTRACT));
    let flags: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT header FROM "{schema}".contract_flags WHERE contract_id = $1 ORDER BY header"#
    )))
    .bind(CONTRACT)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(flags, ["Low payment", "Note from buyer"]);
    assert!(
        notices(&h, owner_account)
            .await
            .contains(&"Buyback: New reverse buyback request for program Hangar sales".to_owned()),
        "{:?}",
        notices(&h, owner_account).await
    );
    let details = page(
        &h,
        &format!("/plugins/{ID}/reverse/tracking/{number}"),
        &pilot,
    )
    .await;
    for want in [
        "Contract",
        "Low payment",
        "Contract items",
        "Requested items",
    ] {
        assert!(details.body.contains(want), "{want}: {}", details.body);
    }
    assert!(
        !details.body.contains("Release reserved items"),
        "{}",
        details.body
    );
    // Still reserved while the contract is outstanding.
    let picker = page(&h, &format!("/plugins/{ID}/{picker_at}"), &pilot).await;
    assert!(
        picker.body.contains("How many Tritanium? 500 available"),
        "{}",
        picker.body
    );
    let stats = page(&h, &format!("/plugins/{ID}/reverse/program-stats"), &owner).await;
    assert_eq!(stats.status, StatusCode::OK, "{}", stats.body);
    assert!(stats.body.contains(&number), "{}", stats.body);
    assert!(stats.body.contains("The Mittani"), "{}", stats.body);

    // Finished: no longer reserved, and never read again (B23).
    mount_contracts(
        &h,
        serde_json::json!([contract(&number, "finished", 8000.0)]),
        3,
    )
    .await;
    run_schedule(&h, "contracts").await;
    mount_contracts(
        &h,
        serde_json::json!([contract(&number, "deleted", 8000.0)]),
        1,
    )
    .await;
    run_schedule(&h, "contracts").await;
    let status: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT status FROM "{schema}".contracts WHERE contract_id = $1"#
    )))
    .bind(CONTRACT)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(status, "finished");
    let picker = page(&h, &format!("/plugins/{ID}/{picker_at}"), &pilot).await;
    assert!(
        picker.body.contains("How many Tritanium? 2000 available"),
        "{}",
        picker.body
    );
    let mine = page(&h, &format!("/plugins/{ID}/reverse/stats"), &pilot).await;
    assert!(mine.body.contains("Total spent"), "{}", mine.body);
    assert!(mine.body.contains("No pending requests."), "{}", mine.body);

    // A manager removes a pending request of their program.
    let res = post(
        &h,
        &pilot,
        &picker_at,
        &format!("_form=add&type_id={RIFTER}&quantity=1"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &pilot, &picker_at, "_form=checkout&notes=").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (pending, _) = request_of(&h, pilot_account).await;
    let res = post(
        &h,
        &owner,
        "reverse/program-stats",
        &format!("_form=remove_request&request={pending}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT count(*) FROM "{schema}".reverse_trackings WHERE id = $1"#
    )))
    .bind(pending)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, 0);

    // Deleting the program keeps its matched request, unmatched.
    let res = post(
        &h,
        &owner,
        "manage/reverse",
        &format!("_form=delete_reverse&program={program}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let kept: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT program_id FROM "{schema}".reverse_trackings WHERE id = $1"#
    )))
    .bind(request)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(kept, None);
}

/// Who sees a reverse program, containers as its stock, and the Settings'
/// switch.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn reverse_programs_by_containers_visibility_and_the_switch(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, location, _fuzzwork) = set_up(&h).await;
    let schema = schema(&h).await;

    // Refresh stock reads the manager's hangars before any program, so
    // the editor offers the container.
    let res = post(&h, &owner, "manage/reverse", "_form=refresh_stock").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    follow_ups(&h).await;
    let editor = page(&h, &format!("/plugins/{ID}/manage/reverse/new"), &owner).await;
    assert!(
        editor
            .body
            .contains(&format!("Ore box (Station Container) #{BOX}")),
        "{}",
        editor.body
    );
    let loc = format!("loc_{location}");
    let boxed = format!("box_{BOX}");
    let body = form_body(
        &editor.body,
        "reverse_program",
        &[
            ("name", "Ore box sales"),
            ("stock_source", "containers"),
            ("is_public", "on"),
            (&loc, "on"),
            (&boxed, "on"),
        ],
    );
    let res = post(&h, &owner, "manage/reverse/new", &body).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let program: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        r#"SELECT id FROM "{schema}".reverse_programs"#
    )))
    .fetch_one(&h.db)
    .await
    .unwrap();
    work(&h).await;
    follow_ups(&h).await;
    assert_eq!(stock(&h, program).await, vec![(PYERITE, 500)]);

    // A public program: every pilot who can log in buys, without any
    // permission of the app's.
    let outsider = log_in_as(&h, "1887431749:Outsider", None).await;
    let picker = page(&h, &format!("/plugins/{ID}/reverse/{program}"), &outsider).await;
    assert_eq!(picker.status, StatusCode::OK, "{}", picker.body);
    assert!(picker.body.contains("Pyerite"), "{}", picker.body);
    assert!(picker.body.contains("10.00"), "{}", picker.body);
    // Restricted to a group they aren't in: gone for them.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"UPDATE "{schema}".reverse_programs SET is_public = false, restricted_groups = '{{12345}}'"#
    )))
    .execute(&h.db)
    .await
    .unwrap();
    let grantee = grant(&h, OUTSIDER, &["basic_access"]).await;
    assert!(grantee > 0);
    let picker = page(&h, &format!("/plugins/{ID}/reverse/{program}"), &outsider).await;
    assert_eq!(picker.status, StatusCode::NOT_FOUND, "{}", picker.body);
    let index = page(&h, &format!("/plugins/{ID}/reverse"), &outsider).await;
    assert!(!index.body.contains("Ore box sales"), "{}", index.body);
    // Their manager still sees it.
    let index = page(&h, &format!("/plugins/{ID}/reverse"), &owner).await;
    assert!(index.body.contains("Ore box sales"), "{}", index.body);

    // Reverse buyback turned off: its pages say so (AA only hid its menu).
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"UPDATE "{schema}".settings SET reverse_enabled = false"#
    )))
    .execute(&h.db)
    .await
    .unwrap();
    for at in ["reverse", &format!("reverse/{program}"), "reverse/stats"] {
        let off = page(&h, &format!("/plugins/{ID}/{at}"), &owner).await;
        assert_eq!(off.status, StatusCode::OK, "{at}: {}", off.body);
        assert!(
            off.body.contains("Reverse buyback is off"),
            "{at}: {}",
            off.body
        );
    }
    let res = post(
        &h,
        &owner,
        &format!("reverse/{program}"),
        &format!("_form=add&type_id={PYERITE}&quantity=1"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}
