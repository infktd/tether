//! The Freight app end to end (aa-freight): installed from its real
//! component and migration; the contract handler added through Add data source;
//! its corporation's courier contracts read and kept as the operation mode
//! says; stations named from ESI and a structure added by hand; a priced
//! route; the calculator, the contracts checked against the pricing, My
//! contracts and the statistics; and the pilots' and customers' notices
//! on Discord, each once.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, Respond, ResponseTemplate};

const ID: &str = "tether.freight";
const CHRIBBA: i64 = 196379789;
const PILOT_A: i64 = 443630591;
const CORP: i64 = 1164409536;
const OTHER_CORP: i64 = 98000001;
const JITA: i64 = 60003760;
const AMARR: i64 = 60008494;
const TOWER: i64 = 1_022_734_985_679;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("freight")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/freight/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_freight.sql");
    let cards = plugin_file("migrations/0002_outbox_cards.sql");
    let mentions = plugin_file("migrations/0003_pilot_mentions.sql");
    let pages = plugin_file("migrations/0004_outbox_pages.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_freight.sql", migration.as_bytes()),
        ("migrations/0002_outbox_cards.sql", cards.as_bytes()),
        ("migrations/0003_pilot_mentions.sql", mentions.as_bytes()),
        ("migrations/0004_outbox_pages.sql", pages.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Adds Chribba as the contract handler (the SSO round trip).
async fn add_handler(h: &Harness, owner: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(asked.contains(&"esi-contracts.read_corporation_contracts.v1".to_owned()));
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

#[allow(clippy::too_many_arguments)] // a contract's fields, as ESI's
fn contract(
    id: i64,
    kind: &str,
    assignee: i64,
    issuer: i64,
    route: (i64, i64),
    status: &str,
    reward: f64,
    issued: Duration,
) -> serde_json::Value {
    serde_json::json!({
        "contract_id": id,
        "type": kind,
        "assignee_id": assignee,
        "acceptor_id": if status == "finished" { CHRIBBA } else { 0 },
        "availability": "corporation",
        "for_corporation": false,
        "issuer_id": issuer,
        "issuer_corporation_id": OTHER_CORP,
        "start_location_id": route.0,
        "end_location_id": route.1,
        "status": status,
        "title": "",
        "volume": 100000.0,
        "collateral": 1000000000.0,
        "reward": reward,
        "price": 0.0,
        "days_to_complete": 3,
        "date_issued": at(issued),
        "date_expired": at(issued + Duration::days(14)),
        "date_accepted": if status == "finished" { Some(at(issued + Duration::hours(1))) } else { None },
        "date_completed": if status == "finished" { Some(at(issued + Duration::hours(5))) } else { None },
    })
}

/// Delivered days ago by a corporation, which ESI names as its acceptor.
fn delivered_by_a_corporation() -> serde_json::Value {
    let mut c = contract(
        106,
        "courier",
        CORP,
        PILOT_A,
        (TOWER, JITA),
        "finished",
        50_000_000.0,
        Duration::days(-2),
    );
    c["acceptor_id"] = serde_json::json!(CORP);
    c
}

async fn mount(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contracts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([
                    // Priced, fresh: announced to pilots and its customer.
                    contract(
                        101,
                        "courier",
                        CORP,
                        PILOT_A,
                        (JITA, TOWER),
                        "outstanding",
                        110_000_000.0,
                        Duration::hours(-1)
                    ),
                    // Not a courier contract, or not the corporation's.
                    contract(
                        102,
                        "item_exchange",
                        CORP,
                        PILOT_A,
                        (JITA, TOWER),
                        "outstanding",
                        1.0,
                        Duration::hours(-1)
                    ),
                    contract(
                        103,
                        "courier",
                        OTHER_CORP,
                        PILOT_A,
                        (JITA, TOWER),
                        "outstanding",
                        1.0,
                        Duration::hours(-1)
                    ),
                    // Delivered days ago: statistics, but no news.
                    contract(
                        104,
                        "courier",
                        CORP,
                        PILOT_A,
                        (TOWER, JITA),
                        "finished",
                        110_000_000.0,
                        Duration::days(-3)
                    ),
                    // No pricing for its route, and below nothing.
                    contract(
                        105,
                        "courier",
                        CORP,
                        PILOT_A,
                        (JITA, AMARR),
                        "outstanding",
                        5_000_000.0,
                        Duration::hours(-2)
                    ),
                    delivered_by_a_corporation(),
                ])),
        )
        .mount(&h.esi_server)
        .await;
    for (id, name, system) in [
        (
            JITA,
            "Jita IV - Moon 4 - Caldari Navy Assembly Plant",
            30000142,
        ),
        (
            AMARR,
            "Amarr VIII (Oris) - Emperor Family Academy",
            30002187,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/universe/stations/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "station_id": id,
                "name": name,
                "system_id": system,
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
            { "id": 30002187, "name": "Amarr", "category": "solar_system" },
            { "id": JITA, "name": "Jita IV - Moon 4 - Caldari Navy Assembly Plant", "category": "station" },
            { "id": AMARR, "name": "Amarr VIII (Oris) - Emperor Family Academy", "category": "station" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

/// ESI's affiliation as it is: a batch with an id that isn't a
/// character's (a corporation that accepted a contract) is refused whole.
struct StrictAffiliation(AffiliationFixture);

impl Respond for StrictAffiliation {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let ids: Vec<i64> = serde_json::from_slice(&request.body).unwrap();
        if ids.contains(&CORP) {
            return ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({ "error": "Invalid character ID" }));
        }
        self.0.respond(request)
    }
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    let url = if at.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{at}")
    };
    send(&h.app, form(&url, body, token)).await
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
            // Every notice is a card: its parts, a line each, under the
            // message's text (the mention, if any).
            let card = &body["embeds"][0];
            let mut lines = vec![
                format!("content: {}", body["content"].as_str().unwrap_or_default()),
                card["title"].as_str().unwrap().to_owned(),
                card["author"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                card["description"].as_str().unwrap_or_default().to_owned(),
            ];
            for field in card["fields"].as_array().unwrap() {
                lines.push(format!(
                    "{}: {}",
                    field["name"].as_str().unwrap(),
                    field["value"].as_str().unwrap()
                ));
            }
            // What the title opens.
            lines.push(format!("url: {}", card["url"].as_str().unwrap_or_default()));
            lines.join("\n")
        })
        .collect()
}

async fn account_of(h: &Harness, character: i64) -> i64 {
    sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn grant(h: &Harness, account: i64, permission: &str) {
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind(format!("plugin.{ID}.{permission}"))
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn freight_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(StrictAffiliation(AffiliationFixture::load()))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    let about = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        about.body.contains("setup_contract_handler"),
        "{}",
        about.body
    );
    let index = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(index.status, StatusCode::OK, "{}", index.body);
    assert!(index.body.contains("None yet"), "{}", index.body);
    let owner = add_handler(&h, &owner).await;
    // The handler is chosen on its Manage page, not the calculator's.
    let mode_form = "name=\"_form\" value=\"mode\"";
    assert!(!index.body.contains(mode_form), "{}", index.body);
    let manage = page(&h, &format!("/plugins/{ID}/handler"), &owner).await;
    assert!(manage.body.contains(mode_form), "{}", manage.body);
    let res = post(
        &h,
        &owner,
        "handler",
        &format!("_form=mode&handler={CHRIBBA}&mode=corp_public"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    sync(&h).await;

    // The handler, and its courier contracts only.
    let index = page(&h, &format!("/plugins/{ID}"), &owner).await;
    for text in [
        "Corporation public",
        "Otherworld Enterprises",
        "No routes are priced yet",
    ] {
        assert!(index.body.contains(text), "{text}: {}", index.body);
    }
    let kept: Vec<i64> = sqlx::query_scalar(
        r#"SELECT contract_id FROM "plugin_tether.freight".contracts ORDER BY 1"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(kept, vec![101, 104, 105, 106]);
    // Pilots' corporations: Chribba's from ESI's affiliation, though a
    // corporation delivered another; the corporation's is itself, as
    // aa-freight's.
    let pilots: Vec<(i64, Option<i64>)> = sqlx::query_as(
        r#"SELECT contract_id, acceptor_corporation_id FROM "plugin_tether.freight".contracts
           WHERE acceptor_id IS NOT NULL ORDER BY 1"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(pilots, vec![(104, Some(CORP)), (106, Some(CORP))]);

    // Stations in contracts are named (through /universe/names); route
    // ends are added on Locations: a station by id, a structure by name.
    let contracts = page(&h, &format!("/plugins/{ID}/contracts"), &owner).await;
    assert!(
        contracts.body.contains("Caldari Navy Assembly Plant"),
        "{}",
        contracts.body
    );
    let res = post(
        &h,
        &owner,
        "locations",
        &format!("_form=add_location&location_id={JITA}&name=&system="),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let locations = page(&h, &format!("/plugins/{ID}/locations"), &owner).await;
    assert!(locations.body.contains("Jita"), "{}", locations.body);
    let res = post(
        &h,
        &owner,
        "locations",
        &format!("_form=add_location&location_id={TOWER}&name=&system="),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("A structure needs its name"),
        "{}",
        res.body
    );
    let res = post(
        &h,
        &owner,
        "locations",
        &format!("_form=add_location&location_id={TOWER}&name=Perimeter+-+Tranquility+Trading+Tower&system=Perimeter"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // A route: both ways, 50m + 500 per m3 + 1% of the collateral.
    let route = format!(
        "_form=add_pricing&start={JITA}&end={TOWER}&bidirectional=on&active=on\
         &price_base=50000000&price_min=&price_per_volume=500&price_per_collateral_percent=1\
         &collateral_min=&collateral_max=5000000000&volume_min=&volume_max=320000\
         &days_to_expire=3&days_to_complete=3&details=No+cans"
    );
    let res = post(&h, &owner, "pricing", &route).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // One pricing per route (either way when both ways).
    let back = route.replace(
        &format!("start={JITA}&end={TOWER}"),
        &format!("start={TOWER}&end={JITA}"),
    );
    let res = post(&h, &owner, "pricing", &back).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("the way back already"), "{}", res.body);

    // The calculator: 110m for 100,000 m3 with 1b collateral.
    let res = post(
        &h,
        &owner,
        "",
        "_form=calculate&pricing=1&volume=100000&collateral=1000000000",
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for text in [
        "Tranquility Trading Tower",
        "Pick up",
        "110,000,000",
        "No cans",
    ] {
        assert!(res.body.contains(text), "{text}: {}", res.body);
    }
    let res = post(
        &h,
        &owner,
        "",
        "_form=calculate&pricing=1&volume=400000&collateral=1000000000",
    )
    .await;
    assert!(
        res.body
            .contains("exceeds the maximum allowed volume of 320,000 m3"),
        "{}",
        res.body
    );

    // Discord: pilots and customers in one channel.
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
    // aa-freight's FREIGHT_DISCORD_MENTIONS: none by default.
    let ping: Option<String> =
        sqlx::query_scalar(r#"SELECT pilot_ping FROM "plugin_tether.freight".settings"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(ping, None);
    let pricing = page(&h, &format!("/plugins/{ID}/pricing"), &owner).await;
    assert!(
        pricing
            .body
            .contains("Pilot notices mention the role of state"),
        "{}",
        pricing.body
    );
    // Its help says what it does in pilots' terms, not AA's setting name.
    assert!(
        pricing
            .body
            .contains("New-contract notices mention the Discord role given to this state"),
        "{}",
        pricing.body
    );
    assert!(
        !pricing.body.contains("FREIGHT_DISCORD_MENTIONS"),
        "{}",
        pricing.body
    );
    // Pilot notices mention Member's role (Tether's stand-in for @here).
    let res = post(
        &h,
        &owner,
        "pricing",
        &format!(
            "_form=settings&modifier=&pilot_channel={c}&customer_channel={c}&pilot_ping=+Member+"
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let ping: Option<String> =
        sqlx::query_scalar(r#"SELECT pilot_ping FROM "plugin_tether.freight".settings"#)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(ping.as_deref(), Some("Member"));
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    sync(&h).await;
    let sent = discord_messages(&h).await;
    // 101 to pilots (priced) and its customer; 105 to nobody (no pricing,
    // not every contract announced, as aa-freight's customer notices too);
    // 104's delivery is old.
    assert_eq!(sent.len(), 2, "{sent:#?}");
    let pilots: Vec<&String> = sent
        .iter()
        .filter(|m| m.contains("New courier contract"))
        .collect();
    assert_eq!(pilots.len(), 1, "{sent:#?}");
    assert!(pilots[0].contains("Contract check: OK"), "{}", pilots[0]);
    assert!(pilots[0].contains("Pilot A"), "{}", pilots[0]);
    assert!(
        pilots[0].starts_with(&format!("content: <@&{DISCORD_MEMBER_ROLE}>\n")),
        "{}",
        pilots[0]
    );
    // Each card's title opens the app, as aa-freight's: the contracts for
    // pilots, My contracts for customers.
    assert!(
        pilots[0].ends_with(&format!("url: {SITE}/plugins/{ID}/contracts")),
        "{}",
        pilots[0]
    );
    assert!(
        sent.iter()
            .filter(|m| m.contains("your courier contract"))
            .all(|m| m.ends_with(&format!("url: {SITE}/plugins/{ID}/mine"))),
        "{sent:#?}"
    );
    // Customers' notices mention nobody (aa-freight's mention is pilots').
    assert!(
        sent.iter()
            .filter(|m| m.contains("your courier contract"))
            .all(|m| m.starts_with("content: \n")),
        "{sent:#?}"
    );
    assert!(!sent.iter().any(|m| m.contains("No pricing")), "{sent:#?}");
    // The customers' channel is shared: no collateral or cargo there.
    assert!(
        sent.iter()
            .filter(|m| m.contains("your courier contract"))
            .all(|m| !m.contains("Collateral")),
        "{sent:#?}"
    );
    // Each once.
    sync(&h).await;
    assert_eq!(discord_messages(&h).await.len(), 2);

    // A state without a Discord role (Blue here): the notice goes out
    // unmentioned, not lost, and Pricing says why, with the fix. Every
    // contract announced now: 105, unpriced, to pilots and its customer.
    let res = post(
        &h,
        &owner,
        "pricing",
        &format!(
            "_form=settings&modifier=&pilot_channel={c}&customer_channel={c}&notify_all=on\
             &pilot_ping=Blue"
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    sync(&h).await;
    let sent = discord_messages(&h).await;
    assert_eq!(sent.len(), 4, "{sent:#?}");
    assert!(
        sent[2].starts_with("content: \nNew courier contract"),
        "{}",
        sent[2]
    );
    assert!(sent[2].contains("No pricing for this route"), "{}", sent[2]);
    assert!(
        sent[3].contains("waiting to be picked up") && sent[3].contains("No pricing"),
        "{}",
        sent[3]
    );
    let failed: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.freight".outbox WHERE failed IS NOT NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(failed, 0);
    let pricing = page(&h, &format!("/plugins/{ID}/pricing"), &owner).await;
    assert!(
        pricing
            .body
            .contains("went out without its mention: no Discord role is mapped to that state"),
        "{}",
        pricing.body
    );
    // Cleared: no mention, and nothing to say.
    let res = post(
        &h,
        &owner,
        "pricing",
        &format!(
            "_form=settings&modifier=&pilot_channel={c}&customer_channel={c}&notify_all=on\
             &pilot_ping="
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let pricing = page(&h, &format!("/plugins/{ID}/pricing"), &owner).await;
    assert!(
        !pricing.body.contains("went out without its mention"),
        "{}",
        pricing.body
    );

    // The contracts, checked.
    let contracts = page(&h, &format!("/plugins/{ID}/contracts"), &owner).await;
    assert_eq!(contracts.status, StatusCode::OK, "{}", contracts.body);
    assert!(contracts.body.contains("No pricing"), "{}", contracts.body);
    assert!(
        contracts.body.contains("Emperor Family Academy"),
        "{}",
        contracts.body
    );
    // Active and All (aa-freight's two lists): delivered ones only in All.
    assert!(
        contracts.body.contains("Active contracts") && !contracts.body.contains("Finished"),
        "{}",
        contracts.body
    );
    let all = format!("/plugins/{ID}/contracts?_tab=1");
    let contracts = page(&h, &all, &owner).await;
    for text in ["All contracts", "Finished", "Emperor Family Academy"] {
        assert!(contracts.body.contains(text), "{text}: {}", contracts.body);
    }
    // Past the newest 400, All says how many there are.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.freight".contracts (contract_id, issuer_id,
               issuer_corporation_id, start_location, end_location, status, date_issued)
           SELECT 1000 + n, $1, $2, $3, $4, 'finished', now() - interval '20 days'
           FROM generate_series(1, 400) n"#,
    )
    .bind(PILOT_A)
    .bind(OTHER_CORP)
    .bind(JITA)
    .bind(AMARR)
    .execute(&h.db)
    .await
    .unwrap();
    let contracts = page(&h, &all, &owner).await;
    assert_eq!(contracts.status, StatusCode::OK, "{}", contracts.body);
    assert!(
        contracts.body.contains("The newest 400 of 404 contracts."),
        "{}",
        contracts.body
    );
    sqlx::query(r#"DELETE FROM "plugin_tether.freight".contracts WHERE contract_id > 1000"#)
        .execute(&h.db)
        .await
        .unwrap();
    // Statistics: the delivered one, by route and pilot.
    let stats = page(&h, &format!("/plugins/{ID}/statistics"), &owner).await;
    for text in [
        "Tranquility Trading Tower",
        "Chribba",
        "Pilot corporations",
        "Otherworld Enterprises",
    ] {
        assert!(stats.body.contains(text), "{text}: {}", stats.body);
    }

    // A pilot with basic access: the app, not My contracts (aa-freight's
    // is use_calculator's) or the others.
    let pilot = log_in_as(&h, "443630591:Pilot A", None).await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}"), &pilot).await.status,
        StatusCode::NOT_FOUND
    );
    grant(&h, account_of(&h, PILOT_A).await, "basic_access").await;
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/mine"), &pilot)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let index = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert!(!index.body.contains("My contracts"), "{}", index.body);
    assert!(!index.body.contains("Reward calculator"), "{}", index.body);
    assert!(
        !index.body.contains("name=\"_form\" value=\"mode\""),
        "{}",
        index.body
    );
    // The contract handler is setup_contract_handler's (as aa-freight).
    for at in ["contracts", "statistics", "pricing", "locations", "handler"] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/{at}"), &pilot)
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{at}"
        );
    }
    let res = post(
        &h,
        &pilot,
        "handler",
        &format!("_form=mode&handler={CHRIBBA}&mode=corp_public"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &pilot,
        "",
        "_form=calculate&pricing=1&volume=1&collateral=1",
    )
    .await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.body);
    // With use_calculator, their own contracts in aa-freight's statuses:
    // outstanding, in progress, finished and failed, not cancelled ones.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.freight".contracts (contract_id, issuer_id,
               issuer_corporation_id, start_location, end_location, status, date_issued, title)
           VALUES (107, $1, $2, $3, $4, 'cancelled', now(), 'Called off')"#,
    )
    .bind(PILOT_A)
    .bind(OTHER_CORP)
    .bind(JITA)
    .bind(AMARR)
    .execute(&h.db)
    .await
    .unwrap();
    grant(&h, account_of(&h, PILOT_A).await, "use_calculator").await;
    let mine = page(&h, &format!("/plugins/{ID}/mine"), &pilot).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    for text in ["Emperor Family Academy", "Finished"] {
        assert!(mine.body.contains(text), "{text}: {}", mine.body);
    }
    assert!(!mine.body.contains("Called off"), "{}", mine.body);

    // At most 100 locations, so the route selects always draw.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.freight".locations (id, name, category)
           SELECT 1000000000000 + n, 'Structure ' || n, 'structure' FROM generate_series(1, 98) n"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let pricing = page(&h, &format!("/plugins/{ID}/pricing"), &owner).await;
    assert_eq!(pricing.status, StatusCode::OK, "{}", pricing.body);
    let res = post(
        &h,
        &owner,
        "locations",
        "_form=add_location&location_id=1000000000999&name=One+more&system=",
    )
    .await;
    assert!(
        res.body.contains("as many locations as there can be"),
        "{}",
        res.body
    );
    // A location in no route can be removed; one in a route can't.
    let res = post(
        &h,
        &owner,
        "locations",
        "_form=delete_location&id=1000000000001",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &owner,
        "locations",
        &format!("_form=delete_location&id={JITA}"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.freight".locations"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 99);
}

/// My Alliance keeps the contracts assigned to the alliance by its
/// members only, as aa-freight (`freight/models/contract_handlers.py:343`);
/// one kept already is followed to its end though its issuer has left.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn my_alliance_keeps_its_members_contracts(db: PgPool) {
    const ALLIANCE: i64 = 159826257;
    let h = harness(db, true).await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(AffiliationFixture::load())
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let owner = add_handler(&h, &owner).await;
    sqlx::query("UPDATE core.characters SET alliance_id = $1 WHERE id = $2")
        .bind(ALLIANCE)
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    let res = post(
        &h,
        &owner,
        "handler",
        &format!("_form=mode&handler={CHRIBBA}&mode=my_alliance"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let assigned = |id, issuer| {
        contract(
            id,
            "courier",
            ALLIANCE,
            issuer,
            (JITA, AMARR),
            "outstanding",
            1.0,
            Duration::hours(-1),
        )
    };
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contracts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                // Chribba is in the alliance; Pilot A isn't.
                .set_body_json(serde_json::json!([
                    assigned(201, CHRIBBA),
                    assigned(202, PILOT_A)
                ])),
        )
        .mount(&h.esi_server)
        .await;
    sync(&h).await;
    let kept = || async {
        sqlx::query_scalar::<_, i64>(
            r#"SELECT contract_id FROM "plugin_tether.freight".contracts ORDER BY 1"#,
        )
        .fetch_all(&h.db)
        .await
        .unwrap()
    };
    assert_eq!(kept().await, vec![201]);
    // One kept already stays, though its issuer has left the alliance.
    sqlx::query(
        r#"INSERT INTO "plugin_tether.freight".contracts (contract_id, issuer_id,
               issuer_corporation_id, start_location, end_location, status, date_issued)
           VALUES (202, $1, $2, $3, $4, 'outstanding', now())"#,
    )
    .bind(PILOT_A)
    .bind(OTHER_CORP)
    .bind(JITA)
    .bind(AMARR)
    .execute(&h.db)
    .await
    .unwrap();
    sync(&h).await;
    assert_eq!(kept().await, vec![201, 202]);
}
