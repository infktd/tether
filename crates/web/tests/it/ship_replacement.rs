//! The Ship Replacement plugin end to end: installed from its real
//! component and migration; SRP fleets added by permission; pilots
//! requesting SRP by zKillboard or ESI link, the loss read from ESI (mock)
//! and its value from zKillboard (mock, over the plugin HTTP capability),
//! the victim checked against the pilot's own characters; reviewers
//! approving, rejecting, adjusting payouts and marking paid; totals.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "tether.ship-replacement";
const CHRIBBA: i64 = 196379789;
const PILOT_A: i64 = 443630591;
const NPC_CORP: i64 = 1000167;
const BLUE_CORP: i64 = 98133756;
const RIFTER: i64 = 587;
const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";
const H3: &str = "3333333333333333333333333333333333333333";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("ship-replacement"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/ship-replacement/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(11);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_ship_replacement.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_ship_replacement.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{state}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

fn at(uri: &str) -> String {
    if uri.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{uri}")
    }
}

async fn post(h: &Harness, token: &str, uri: &str, body: &str) -> Res {
    send(&h.app, form(&at(uri), body, token)).await
}

async fn open(h: &Harness, token: &str, uri: &str) -> Res {
    page(h, &at(uri), token).await
}

fn esi_killmail(id: i64, victim: i64) -> serde_json::Value {
    serde_json::json!({
        "attackers": [],
        "killmail_id": id,
        "killmail_time": "2026-09-20T19:04:05Z",
        "solar_system_id": 30000142,
        "victim": {
            "character_id": victim,
            "corporation_id": NPC_CORP,
            "damage_taken": 1234,
            "ship_type_id": RIFTER
        }
    })
}

async fn mount_esi(h: &Harness) {
    for (id, hash, victim) in [
        (1001, H1, PILOT_A),
        (1002, H2, CHRIBBA),
        (1003, H3, PILOT_A),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/killmails/{id}/{hash}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(esi_killmail(id, victim)))
            .mount(&h.esi_server)
            .await;
    }
    // A made-up hash: ESI refuses it, as it does.
    Mock::given(method("GET"))
        .and(path(format!("/killmails/1005/{H1}")))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "error": "Invalid killmail_id and/or killmail_hash"
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": RIFTER, "name": "Rifter", "category": "inventory_type" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

async fn mount_zkill(zkill: &MockServer) {
    for (id, hash, value) in [(1001, H1, 12_500_000.0), (1002, H2, 90_000_000.0)] {
        Mock::given(method("GET"))
            .and(path(format!("/api/killID/{id}/")))
            .and(header("x-tether-test-host", "zkillboard.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "killmail_id": id, "zkb": { "hash": hash, "totalValue": value, "points": 1 } }
            ])))
            .mount(zkill)
            .await;
    }
    // zKillboard hasn't seen this one yet.
    Mock::given(method("GET"))
        .and(path("/api/killID/1003/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(zkill)
        .await;
}

async fn one<T>(h: &Harness, sql: &'static str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sql).fetch_one(&h.db).await.unwrap()
}

async fn request_of(h: &Harness, kill: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.ship-replacement\".requests WHERE killmail_id = $1",
    )
    .bind(kill)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn status_of(h: &Harness, request: i64) -> (String, Option<f64>, bool) {
    sqlx::query_as("SELECT status, payout, paid FROM \"plugin_tether.ship-replacement\".requests WHERE id = $1")
    .bind(request)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn ship_replacement_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, BLUE_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let zkill = MockServer::start().await;
    h.plugins.route_http_to(&zkill.address().to_string());
    mount_esi(&h).await;
    mount_zkill(&zkill).await;
    // zKillboard is the one host it may call, as approved at install.
    let hosts: Vec<String> =
        sqlx::query_scalar("SELECT host FROM core.plugin_http_hosts WHERE plugin_id = $1")
            .bind(ID)
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(hosts, vec!["zkillboard.com".to_owned()]);

    let pilot = log_in_as(&h, "443630591:Pilot A", None).await;
    let adjuster = log_in_as(&h, "1887431749:gigX", None).await;
    // Nothing without access_srp.
    assert_eq!(open(&h, &pilot, "").await.status, StatusCode::NOT_FOUND);
    grant(&h, &owner, "access_srp", MEMBER_STATE).await;
    for permission in ["access_srp", "change_srpuserrequest"] {
        grant(&h, &owner, permission, BLUE_STATE).await;
    }
    let home = open(&h, &pilot, "").await;
    assert_eq!(home.status, StatusCode::OK, "{}", home.body);
    assert!(home.body.contains("No SRP fleets yet."), "{}", home.body);
    assert!(!home.body.contains("Add SRP Fleet"));
    // Adding fleets takes add_srpfleetmain.
    assert_eq!(open(&h, &pilot, "add").await.status, StatusCode::NOT_FOUND);

    // SRP staff add a fleet; the time is EVE time.
    let bad = post(
        &h,
        &owner,
        "add",
        "_form=add_fleet&name=Op+Rock&doctrine=Ferox&fleet_commander=Chribba&fleet_time=soon&aar=",
    )
    .await;
    assert!(bad.body.contains("YYYY-MM-DD HH:MM"), "{}", bad.body);
    let res = post(
        &h,
        &owner,
        "add",
        "_form=add_fleet&name=Op+Rock&doctrine=Ferox&fleet_commander=Chribba\
         &fleet_time=2026-09-20+19%3A00&aar=Held+the+grid",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let fleet: i64 = one(
        &h,
        "SELECT id FROM \"plugin_tether.ship-replacement\".fleets",
    )
    .await;
    assert_eq!(res.location(), at(&format!("fleet/{fleet}")));
    let code: String = one(
        &h,
        "SELECT srp_code FROM \"plugin_tether.ship-replacement\".fleets",
    )
    .await;
    assert_eq!(code.len(), 16);
    assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()), "{code}");

    // Pilots see it and its request link; only staff see its requests.
    let home = open(&h, &pilot, "").await;
    assert!(home.body.contains("Op Rock"), "{}", home.body);
    assert!(
        home.body.contains(&format!("request/{code}")),
        "{}",
        home.body
    );
    assert_ne!(
        open(&h, &pilot, &format!("fleet/{fleet}")).await.status,
        StatusCode::OK
    );
    let form = open(&h, &pilot, &format!("request/{code}")).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    assert!(form.body.contains("Killboard Link"), "{}", form.body);
    let request = |link: &str, character: i64| {
        format!(
            "_form=request&killboard_link={}&character={character}&additional_info=Tackled+first",
            link.replace(':', "%3A").replace('/', "%2F")
        )
    };
    let uri = format!("request/{code}");

    // Not a killmail link.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://example.com/kill/1/", PILOT_A),
    )
    .await;
    assert!(
        res.body.contains("isn&#39;t a killmail link"),
        "{}",
        res.body
    );
    // A character that isn't theirs.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1001/", CHRIBBA),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // A loss of theirs, by zKillboard link: read from ESI, valued by zKillboard.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1001/", PILOT_A),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let r1 = request_of(&h, 1001).await;
    let (ship, value, character): (String, Option<f64>, i64) = sqlx::query_as("SELECT ship_name, kb_total_loss, character_id FROM \"plugin_tether.ship-replacement\".requests WHERE id = $1")
    .bind(r1)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(
        (ship.as_str(), value, character),
        ("Rifter", Some(12_500_000.0), PILOT_A)
    );
    let asked = zkill.received_requests().await.unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].headers.get("user-agent").unwrap(),
        "tether (app tether.ship-replacement)"
    );

    // Once per loss; and zKillboard isn't asked again (the value is kept).
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1001/", PILOT_A),
    )
    .await;
    assert!(res.body.contains("already been requested"), "{}", res.body);
    // Someone else's loss (Chribba's) isn't theirs to claim.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1002/", PILOT_A),
    )
    .await;
    assert!(
        res.body.contains("isn&#39;t one of your characters"),
        "{}",
        res.body
    );
    // An ESI link whose hash zKillboard contradicts isn't sent to EVE.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request(
            &format!("https://esi.evetech.net/latest/killmails/1002/{H1}/"),
            PILOT_A,
        ),
    )
    .await;
    assert!(
        res.body.contains("doesn&#39;t match the kill"),
        "{}",
        res.body
    );
    // zKillboard doesn't know 1003 yet: the ESI link still works, with no value.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1003/", PILOT_A),
    )
    .await;
    assert!(res.body.contains("zKillboard doesn"), "{}", res.body);
    let res = post(
        &h,
        &pilot,
        &uri,
        &request(
            &format!("https://esi.evetech.net/latest/killmails/1003/{H3}/"),
            PILOT_A,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let r3 = request_of(&h, 1003).await;
    assert_eq!(status_of(&h, r3).await, ("pending".to_owned(), None, false));
    let count: i64 = one(
        &h,
        "SELECT count(*) FROM \"plugin_tether.ship-replacement\".requests",
    )
    .await;
    assert_eq!(count, 2);
    let mine = open(&h, &pilot, "").await;
    assert!(mine.body.contains("My SRP Requests"), "{}", mine.body);
    assert!(mine.body.contains("Pending"), "{}", mine.body);

    // An adjuster (change_srpuserrequest) sees the fleet's requests and sets
    // payouts, but can't decide.
    let fleet_page = open(&h, &adjuster, &format!("fleet/{fleet}")).await;
    assert_eq!(fleet_page.status, StatusCode::OK, "{}", fleet_page.body);
    assert!(fleet_page.body.contains("Pilot A"), "{}", fleet_page.body);
    assert!(!fleet_page.body.contains("Mark Completed"));
    let review = format!("review/{r1}");
    let res = post(
        &h,
        &adjuster,
        &review,
        "_form=payout&payout=10000000&comment=Fit+was+off-doctrine",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("pending".to_owned(), Some(10_000_000.0), false)
    );
    let res = post(
        &h,
        &adjuster,
        &review,
        "_form=decide&decision=approve&comment=",
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(status_of(&h, r1).await.0, "pending");
    // Pilots can't review at all.
    assert_ne!(open(&h, &pilot, &review).await.status, StatusCode::OK);

    // A manager approves (keeping the adjusted payout) and rejects.
    let res = post(
        &h,
        &owner,
        &review,
        "_form=decide&decision=approve&comment=o7",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(10_000_000.0), false)
    );
    let res = post(
        &h,
        &owner,
        &format!("review/{r3}"),
        "_form=decide&decision=reject&comment=Not+on+grid",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(status_of(&h, r3).await.0, "rejected");
    let comments: Vec<String> = sqlx::query_scalar(
        "SELECT body FROM \"plugin_tether.ship-replacement\".comments ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        comments,
        vec![
            "Payout set to 10000000 ISK: Fit was off-doctrine".to_owned(),
            "Approved: o7".to_owned(),
            "Rejected: Not on grid".to_owned()
        ]
    );
    let seen = open(&h, &owner, &review).await;
    assert!(seen.body.contains("Fit was off-doctrine"), "{}", seen.body);
    // Changing an approved payout sends it back for approval.
    let res = post(
        &h,
        &adjuster,
        &review,
        "_form=payout&payout=11000000&comment=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("pending".to_owned(), Some(11_000_000.0), false)
    );
    let res = post(&h, &owner, &review, "_form=paid&confirm=on").await;
    assert_ne!(res.status, StatusCode::SEE_OTHER);
    let res = post(
        &h,
        &owner,
        &review,
        "_form=decide&decision=approve&comment=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // Nobody decides, prices or pays their own request: the owner's loss.
    let res = post(
        &h,
        &owner,
        &uri,
        &request("https://zkillboard.com/kill/1002/", CHRIBBA),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let r2 = request_of(&h, 1002).await;
    let own = format!("review/{r2}");
    let seen = open(&h, &owner, &own).await;
    assert!(seen.body.contains("your own request"), "{}", seen.body);
    for body in [
        "_form=decide&decision=approve&comment=",
        "_form=payout&payout=1&comment=",
        "_form=paid&confirm=on",
    ] {
        let res = post(&h, &owner, &own, body).await;
        assert_ne!(res.status, StatusCode::SEE_OTHER, "{body}");
    }
    assert_eq!(status_of(&h, r2).await, ("pending".to_owned(), None, false));

    // Paid: then the payout is settled.
    let res = post(&h, &owner, &review, "_form=paid&confirm=on").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(11_000_000.0), true)
    );
    let res = post(&h, &adjuster, &review, "_form=payout&payout=1&comment=").await;
    assert_ne!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(status_of(&h, r1).await.1, Some(11_000_000.0));
    let mine = open(&h, &pilot, "").await;
    assert!(mine.body.contains("Paid"), "{}", mine.body);

    // Totals per fleet and overall.
    let fleet_page = open(&h, &owner, &format!("fleet/{fleet}")).await;
    assert!(
        fleet_page.body.contains("Total ISK Cost"),
        "{}",
        fleet_page.body
    );
    assert!(fleet_page.body.contains("11m"), "{}", fleet_page.body);
    assert!(fleet_page.body.contains("12.5m"), "{}", fleet_page.body);
    let home = open(&h, &owner, "").await;
    assert!(home.body.contains("Outstanding"), "{}", home.body);

    // Completed: no more requests.
    let res = post(
        &h,
        &owner,
        &format!("fleet/{fleet}"),
        "_form=complete&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // A second click changes nothing (set, not toggled): the form is gone.
    let again = post(
        &h,
        &owner,
        &format!("fleet/{fleet}"),
        "_form=complete&confirm=on",
    )
    .await;
    assert_ne!(again.status, StatusCode::SEE_OTHER);
    let completed: bool = one(
        &h,
        "SELECT completed FROM \"plugin_tether.ship-replacement\".fleets",
    )
    .await;
    assert!(completed);
    let closed = open(&h, &pilot, &uri).await;
    assert!(
        closed.body.contains("takes no more requests"),
        "{}",
        closed.body
    );
    let res = post(
        &h,
        &pilot,
        &uri,
        &request(
            &format!("https://esi.evetech.net/latest/killmails/1003/{H3}/"),
            PILOT_A,
        ),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER);

    // Removing the fleet takes its requests, but their losses stay claimed.
    let res = post(
        &h,
        &owner,
        &format!("fleet/{fleet}"),
        "_form=remove&confirm=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = one(
        &h,
        "SELECT count(*) FROM \"plugin_tether.ship-replacement\".requests",
    )
    .await;
    assert_eq!(left, 0);
    let claimed: i64 = one(
        &h,
        "SELECT count(*) FROM \"plugin_tether.ship-replacement\".claimed_kills",
    )
    .await;
    assert_eq!(claimed, 3);
    let res = post(
        &h,
        &owner,
        "add",
        "_form=add_fleet&name=Op+Two&doctrine=Ferox&fleet_commander=Chribba\
         &fleet_time=2026-09-21+19%3A00&aar=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let code2: String = one(
        &h,
        "SELECT srp_code FROM \"plugin_tether.ship-replacement\".fleets",
    )
    .await;
    let res = post(
        &h,
        &pilot,
        &format!("request/{code2}"),
        &request("https://zkillboard.com/kill/1001/", PILOT_A),
    )
    .await;
    assert!(res.body.contains("already been requested"), "{}", res.body);

    // Links that don't check out: a few, then the pilot waits (the
    // mismatched hash, kills nobody knows and someone else's loss count).
    for _ in 0..2 {
        let res = post(
            &h,
            &pilot,
            &format!("request/{code2}"),
            &request(
                &format!("https://esi.evetech.net/latest/killmails/1005/{H1}/"),
                PILOT_A,
            ),
        )
        .await;
        assert!(res.body.contains("EVE doesn&#39;t know"), "{}", res.body);
    }
    let res = post(
        &h,
        &pilot,
        &format!("request/{code2}"),
        &request("https://zkillboard.com/kill/1004/", PILOT_A),
    )
    .await;
    assert!(res.body.contains("wait a few minutes"), "{}", res.body);

    // Every zKillboard call is in the app's HTTP log.
    let logged: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM core.plugin_http_log WHERE plugin_id = $1 ORDER BY id",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        logged,
        vec![
            "/api/killID/1001/".to_owned(),
            "/api/killID/1002/".to_owned(),
            "/api/killID/1003/".to_owned(),
            "/api/killID/1005/".to_owned(),
        ]
    );
}
