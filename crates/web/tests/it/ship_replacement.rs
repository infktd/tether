//! The Ship Replacement plugin end to end: installed from its real
//! component and migrations; AA's srp permissions and rules: SRP fleets
//! added with add_srpfleetmain or srp_management; pilots requesting SRP by
//! zKillboard link, the loss read from ESI (mock) and its value from
//! zKillboard (mock, over the plugin HTTP capability), the victim checked
//! against the pilot's own characters; every access_srp holder seeing
//! fleets' requests and totals; managers approving, rejecting, setting
//! payouts at any time and marking paid, their own requests included.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path, path_regex};
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
const H4: &str = "4444444444444444444444444444444444444444";

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
    let second = plugin_file("migrations/0002_claims_follow_requests.sql");
    let third = plugin_file("migrations/0003_srp_team_channel.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_ship_replacement.sql", migration.as_bytes()),
        (
            "migrations/0002_claims_follow_requests.sql",
            second.as_bytes(),
        ),
        ("migrations/0003_srp_team_channel.sql", third.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
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
        (1004, H4, PILOT_A),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/killmails/{id}/{hash}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(esi_killmail(id, victim)))
            .mount(&h.esi_server)
            .await;
    }
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
    for (id, hash, value) in [
        (1001, H1, 12_500_000.0),
        (1002, H2, 90_000_000.0),
        (1004, H4, 5_000_000.0),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/api/killID/{id}/")))
            .and(header("x-tether-test-host", "zkillboard.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "killmail_id": id, "zkb": { "hash": hash, "totalValue": value, "points": 1 } }
            ])))
            .mount(zkill)
            .await;
    }
    // zKillboard hasn't seen these yet.
    for id in [1003, 1005] {
        Mock::given(method("GET"))
            .and(path(format!("/api/killID/{id}/")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(zkill)
            .await;
    }
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

/// The newest fleet's id and SRP code.
async fn newest_fleet(h: &Harness) -> (i64, String) {
    sqlx::query_as(
        "SELECT id, srp_code FROM \"plugin_tether.ship-replacement\".fleets ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// A Request SRP form's body.
fn request(link: &str) -> String {
    format!(
        "_form=request&killboard_link={}&additional_info=Tackled+first",
        link.replace(':', "%3A").replace('/', "%2F")
    )
}

fn add_fleet(name: &str) -> String {
    format!(
        "_form=add_fleet&name={name}&doctrine=Ferox&fleet_commander=Chribba\
         &fleet_time=2026-09-20+19%3A00&aar=Held+the+grid"
    )
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
    // gigX manages SRP (AA's auth.srp_management) without add_srpfleetmain.
    let manager = log_in_as(&h, "1887431749:gigX", None).await;
    // Nothing without access_srp.
    assert_eq!(open(&h, &pilot, "").await.status, StatusCode::NOT_FOUND);
    for state in [MEMBER_STATE, BLUE_STATE] {
        grant(&h, &owner, "access_srp", state).await;
    }
    grant(&h, &owner, "srp_management", BLUE_STATE).await;
    let home = open(&h, &pilot, "").await;
    assert_eq!(home.status, StatusCode::OK, "{}", home.body);
    assert!(home.body.contains("No open SRP fleets."), "{}", home.body);
    assert!(!home.body.contains("New SRP fleet"));
    // Adding fleets takes add_srpfleetmain or srp_management.
    assert_ne!(open(&h, &pilot, "add").await.status, StatusCode::OK);
    let res = post(&h, &pilot, "add", &add_fleet("Mine")).await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // A manager adds a fleet (AA: srp_management alone may); the time is
    // EVE time.
    let bad = post(
        &h,
        &manager,
        "add",
        "_form=add_fleet&name=Op+Rock&doctrine=Ferox&fleet_commander=Chribba&fleet_time=soon&aar=",
    )
    .await;
    assert!(bad.body.contains("YYYY-MM-DD HH:MM"), "{}", bad.body);
    let res = post(&h, &manager, "add", &add_fleet("Op+Rock")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (fleet, code) = newest_fleet(&h).await;
    assert_eq!(res.location(), at(&format!("fleet/{fleet}")));
    assert_eq!(code.len(), 16);
    assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()), "{code}");
    // FCs with add_srpfleetmain add them too.
    grant(&h, &owner, "add_srpfleetmain", MEMBER_STATE).await;
    let res = post(&h, &pilot, "add", &add_fleet("Pilot%27s+roam")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // Pilots see it, its request link and, as in AA, its requests.
    let home = open(&h, &pilot, "").await;
    assert!(home.body.contains("Op Rock"), "{}", home.body);
    assert!(home.body.contains("Total ISK Cost"), "{}", home.body);
    assert!(
        home.body.contains(&format!("request/{code}")),
        "{}",
        home.body
    );
    let form = open(&h, &pilot, &format!("request/{code}")).await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    assert!(form.body.contains("Killboard Link"), "{}", form.body);
    let uri = format!("request/{code}");

    // Not a zKillboard link: ESI's aren't taken, as in AA.
    for link in [
        "https://example.com/kill/1/".to_owned(),
        format!("https://esi.evetech.net/latest/killmails/1001/{H1}/"),
    ] {
        let res = post(&h, &pilot, &uri, &request(&link)).await;
        assert!(
            res.body.contains("isn&#39;t a zKillboard link"),
            "{}",
            res.body
        );
    }

    // A loss of theirs: read from ESI, valued by zKillboard; the victim is
    // whichever of their characters lost it.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1001/"),
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
        &request("https://zkillboard.com/kill/1001/"),
    )
    .await;
    assert!(res.body.contains("already been requested"), "{}", res.body);
    // Someone else's loss (Chribba's) isn't theirs to claim.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1002/"),
    )
    .await;
    assert!(
        res.body.contains("isn&#39;t one of your characters"),
        "{}",
        res.body
    );
    // zKillboard doesn't know 1003 yet.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1003/"),
    )
    .await;
    assert!(res.body.contains("zKillboard doesn"), "{}", res.body);
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1004/"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let r4 = request_of(&h, 1004).await;
    assert_eq!(status_of(&h, r4).await, ("pending".to_owned(), None, false));
    let mine = open(&h, &pilot, "").await;
    assert!(mine.body.contains("My SRP requests"), "{}", mine.body);
    assert!(mine.body.contains("Pending"), "{}", mine.body);

    // Every access_srp holder opens the fleet's requests, as AA's fleet
    // view: pilots, ships, amounts, additional info. Not the managers'
    // buttons or the requests' own pages.
    let fleet_url = format!("fleet/{fleet}");
    let seen = open(&h, &pilot, &fleet_url).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Pilot A"), "{}", seen.body);
    assert!(seen.body.contains("Tackled first"), "{}", seen.body);
    assert!(seen.body.contains("12.5m"), "{}", seen.body);
    assert!(!seen.body.contains("Mark Completed"));
    assert!(!seen.body.contains(&format!("review/{r1}")));
    // The request link to share, as the site's full address.
    assert!(
        seen.body.contains(&format!(
            "value=\"{SITE}/plugins/{ID}/request/{code}\" readonly"
        )),
        "{}",
        seen.body
    );
    let review = format!("review/{r1}");
    assert_ne!(open(&h, &pilot, &review).await.status, StatusCode::OK);
    let res = post(
        &h,
        &pilot,
        &fleet_url,
        &format!("_form=decide&request={r1}&decision=approve"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // A manager sets the payout (the status stays), approves (keeping it)
    // and rejects, with comments.
    let res = post(
        &h,
        &manager,
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
        &manager,
        &review,
        "_form=decide&decision=approve&comment=o7",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(10_000_000.0), false)
    );
    // Repricing an approved request keeps it approved (AA's update amount).
    let res = post(
        &h,
        &manager,
        &review,
        "_form=payout&payout=11000000&comment=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(11_000_000.0), false)
    );
    let res = post(
        &h,
        &manager,
        &format!("review/{r4}"),
        "_form=decide&decision=reject&comment=Not+on+grid",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(status_of(&h, r4).await.0, "rejected");
    // ...and a rejected one can be repriced too.
    let res = post(
        &h,
        &manager,
        &format!("review/{r4}"),
        "_form=payout&payout=4000000&comment=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r4).await,
        ("rejected".to_owned(), Some(4_000_000.0), false)
    );
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
            "Payout set to 11000000 ISK.".to_owned(),
            "Rejected: Not on grid".to_owned(),
            "Payout set to 4000000 ISK.".to_owned(),
        ]
    );

    // aa-srp's row buttons on the fleet's page: a rejected request offers
    // Approve, an approved one Reject (asking first) and Mark Paid.
    let rows = open(&h, &manager, &fleet_url).await;
    assert!(
        rows.body
            .contains("Pilot A&#39;s request for their Rifter is rejected"),
        "{}",
        rows.body
    );
    for (decision, status) in [("approve", "approved"), ("reject", "rejected")] {
        let res = post(
            &h,
            &manager,
            &fleet_url,
            &format!("_form=decide&request={r4}&decision={decision}"),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::SEE_OTHER,
            "{decision}: {}",
            res.body
        );
        assert_eq!(res.location(), at(&fleet_url));
        assert_eq!(status_of(&h, r4).await.0, status);
    }
    // The same button twice: a rejected request has no Reject.
    let res = post(
        &h,
        &manager,
        &fleet_url,
        &format!("_form=decide&request={r4}&decision=reject"),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);

    // As AA, a manager decides their own request too: the owner's loss.
    let res = post(
        &h,
        &owner,
        &uri,
        &request("https://zkillboard.com/kill/1002/"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let r2 = request_of(&h, 1002).await;
    let own = format!("review/{r2}");
    for body in [
        "_form=payout&payout=80000000&comment=",
        "_form=decide&decision=approve&comment=",
    ] {
        let res = post(&h, &owner, &own, body).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{body}: {}", res.body);
    }
    assert_eq!(
        status_of(&h, r2).await,
        ("approved".to_owned(), Some(80_000_000.0), false)
    );

    // Paid (aa-srp's): repricing still works; rejecting unmarks it.
    let res = post(&h, &manager, &review, "_form=paid").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(11_000_000.0), true)
    );
    let mine = open(&h, &pilot, "").await;
    assert!(mine.body.contains("Paid"), "{}", mine.body);
    let res = post(
        &h,
        &manager,
        &review,
        "_form=payout&payout=12000000&comment=",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        status_of(&h, r1).await,
        ("approved".to_owned(), Some(12_000_000.0), true)
    );
    let res = post(&h, &owner, &fleet_url, "_form=pay_all").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        status_of(&h, r2).await.2,
        "the owner's own, paid by pay_all"
    );

    // Totals per fleet and overall: AA's Total ISK Cost is every payout
    // set (12m + 4m + 80m), for everyone with access_srp.
    let fleet_page = open(&h, &pilot, &fleet_url).await;
    assert!(fleet_page.body.contains("96m"), "{}", fleet_page.body);
    let home = open(&h, &pilot, "").await;
    assert!(home.body.contains("96m"), "{}", home.body);
    assert!(home.body.contains("Outstanding"), "{}", home.body);
    // New SRP fleet (AA's Add SRP Fleet) is the header's button, for those
    // who may add one.
    let home = open(&h, &manager, "").await;
    assert!(
        home.body
            .contains(&format!("href=\"{}\">New SRP fleet</a>", at("add"))),
        "{}",
        home.body
    );

    // Completed: no more requests, and off the list but for All fleets.
    let res = post(&h, &manager, &fleet_url, "_form=complete").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // A second click changes nothing (set, not toggled): the form is gone.
    let again = post(&h, &manager, &fleet_url, "_form=complete").await;
    assert_ne!(again.status, StatusCode::SEE_OTHER);
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
        &request("https://zkillboard.com/kill/1003/"),
    )
    .await;
    assert_ne!(res.status, StatusCode::SEE_OTHER);
    let home = open(&h, &pilot, "").await;
    assert!(!home.body.contains("Op Rock</a>"), "{}", home.body);
    // All fleets is AA's View All, open to access_srp: a view of the app
    // for everyone who may open it.
    assert_eq!(open(&h, &pilot, "all").await.status, StatusCode::OK);
    let all = open(&h, &manager, "all").await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert!(all.body.contains("All SRP fleets"), "{}", all.body);
    assert!(all.body.contains("Op Rock"), "{}", all.body);
    assert!(
        home.body
            .contains(&format!("href=\"{}\">All fleets</a>", at("all"))),
        "{}",
        home.body
    );

    // Removing the fleet takes its requests, and their losses can be
    // requested again (AA).
    let res = post(&h, &manager, &fleet_url, "_form=remove").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = one(
        &h,
        "SELECT count(*) FROM \"plugin_tether.ship-replacement\".requests",
    )
    .await;
    assert_eq!(left, 0);
    let (_, code2) = newest_fleet(&h).await;
    let res = post(
        &h,
        &pilot,
        &format!("request/{code2}"),
        &request("https://zkillboard.com/kill/1001/"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Losses of fleets removed before 0.3 stay claimed (legacy claims).
    sqlx::query(
        "INSERT INTO \"plugin_tether.ship-replacement\".legacy_claims (killmail_id) VALUES (1004)",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let res = post(
        &h,
        &pilot,
        &format!("request/{code2}"),
        &request("https://zkillboard.com/kill/1004/"),
    )
    .await;
    assert!(res.body.contains("already been requested"), "{}", res.body);

    // Links that don't check out: a few, then the pilot waits (someone
    // else's loss and kills zKillboard doesn't know count).
    for _ in 0..3 {
        let res = post(
            &h,
            &pilot,
            &format!("request/{code2}"),
            &request("https://zkillboard.com/kill/1005/"),
        )
        .await;
        assert!(res.body.contains("zKillboard doesn"), "{}", res.body);
    }
    let res = post(
        &h,
        &pilot,
        &format!("request/{code2}"),
        &request("https://zkillboard.com/kill/1004/"),
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
            "/api/killID/1004/".to_owned(),
            "/api/killID/1005/".to_owned(),
        ]
    );
}

/// Runs the app's queued jobs that are due (the relay).
async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// What was posted to Discord's channels, in order.
async fn posts(h: &Harness) -> Vec<serde_json::Value> {
    h.discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

async fn team_channel(h: &Harness) -> Option<String> {
    sqlx::query_scalar("SELECT channel FROM \"plugin_tether.ship-replacement\".settings")
        .fetch_one(&h.db)
        .await
        .unwrap()
}

/// aa-srp's SRP team channel: none by default (nothing posted, Discord
/// ready or not); Settings, for `manage` alone, picks one of the app's
/// channels; then each new request is posted there as a card pinging
/// nobody, queued with the request so the pilot's request never waits on
/// Discord, and posted later if Discord is down.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn srp_team_channel_gets_new_requests(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, BLUE_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let zkill = MockServer::start().await;
    h.plugins.route_http_to(&zkill.address().to_string());
    mount_esi(&h).await;
    mount_zkill(&zkill).await;
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
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    for state in [MEMBER_STATE, BLUE_STATE] {
        grant(&h, &owner, "access_srp", state).await;
    }
    grant(&h, &owner, "srp_management", BLUE_STATE).await;
    let pilot = log_in_as(&h, "443630591:Pilot A", None).await;
    let manager = log_in_as(&h, "1887431749:gigX", None).await;

    // Settings is manage's (aa-srp's Django admin): not the SRP managers'
    // or the pilots'.
    for who in [&manager, &pilot] {
        assert_ne!(open(&h, who, "settings").await.status, StatusCode::OK);
        let res = post(
            &h,
            who,
            "settings",
            &format!("_form=settings&channel={DISCORD_PING_CHANNEL}"),
        )
        .await;
        assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let settings = open(&h, &owner, "settings").await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(settings.body.contains("Not posted"), "{}", settings.body);
    assert!(settings.body.contains("#fleet-pings"), "{}", settings.body);
    assert!(
        !settings.body.contains("New SRP fleet"),
        "{}",
        settings.body
    );
    assert_eq!(team_channel(&h).await, None);

    // None by default, as aa-srp's: a request posts nothing.
    let res = post(&h, &manager, "add", &add_fleet("Op+Rock")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (_, code) = newest_fleet(&h).await;
    let uri = format!("request/{code}");
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1001/"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    assert!(posts(&h).await.is_empty());

    // Only one of the app's channels is kept.
    post(&h, &owner, "settings", "_form=settings&channel=999").await;
    assert_eq!(team_channel(&h).await, None);
    let res = post(
        &h,
        &owner,
        "settings",
        &format!("_form=settings&channel={DISCORD_PING_CHANNEL}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        team_channel(&h).await.as_deref(),
        Some(DISCORD_PING_CHANNEL)
    );

    // A new request: saved at once, posted by the relay, pinging nobody.
    let res = post(
        &h,
        &pilot,
        &uri,
        "_form=request&killboard_link=https%3A%2F%2Fzkillboard.com%2Fkill%2F1004%2F\
         &additional_info=%40everyone+**look**",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    request_of(&h, 1004).await;
    assert!(posts(&h).await.is_empty());
    work(&h).await;
    let sent = posts(&h).await;
    assert_eq!(sent.len(), 1, "{sent:#?}");
    let card = &sent[0]["embeds"][0];
    assert_eq!(card["title"], "New SRP request: Rifter");
    assert_eq!(card["author"]["name"], "Pilot A");
    let fields = card["fields"].to_string();
    for text in [
        "Op Rock",
        &code,
        "[zKillboard](https://zkillboard.com/kill/1004/)",
    ] {
        assert!(fields.contains(text), "{text}: {fields}");
    }
    // Escaped by the app, and @everyone defused by Tether besides.
    assert_eq!(card["description"], "\\@\u{200B}everyone \\*\\*look\\*\\*");
    assert_eq!(sent[0]["allowed_mentions"]["parse"], serde_json::json!([]));
    assert!(
        !sent[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("<@"),
        "{}",
        sent[0]
    );
    // A request refused (a loss requested before) posts nothing more.
    let res = post(
        &h,
        &pilot,
        &uri,
        &request("https://zkillboard.com/kill/1004/"),
    )
    .await;
    assert!(res.body.contains("already been requested"), "{}", res.body);
    work(&h).await;
    assert_eq!(posts(&h).await.len(), 1);

    // Discord down: the request stands, and its card goes once it's back.
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.discord_server)
        .await;
    let res = post(
        &h,
        &owner,
        &uri,
        &request("https://zkillboard.com/kill/1002/"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    request_of(&h, 1002).await;
    work(&h).await;
    assert_eq!(posts(&h).await.len(), 2, "the refused try");
    sqlx::query("UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND state = 'queued'")
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap();
    work(&h).await;
    let sent = posts(&h).await;
    assert_eq!(sent.len(), 3, "{sent:#?}");
    assert_eq!(sent[2]["embeds"][0]["author"]["name"], "Chribba");

    // A channel no longer the app's: Settings says so, with the fix.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/channels/{DISCORD_PING_CHANNEL}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let settings = open(&h, &owner, "settings").await;
    assert!(
        settings
            .body
            .contains("No Discord channel is assigned to this app yet"),
        "{}",
        settings.body
    );
}

/// AA lists every request of a fleet: past one page, the rest are on the
/// next pages, and a manager's buttons there come back to the same page.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_request_of_a_big_fleet_can_be_opened(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, BLUE_CORP).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let manager = log_in_as(&h, "1887431749:gigX", None).await;
    for state in [MEMBER_STATE, BLUE_STATE] {
        grant(&h, &owner, "access_srp", state).await;
    }
    grant(&h, &owner, "srp_management", BLUE_STATE).await;
    let res = post(&h, &manager, "add", &add_fleet("Big+fight")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (fleet, _) = newest_fleet(&h).await;
    // 401 losses after a big fight, the newest last.
    sqlx::query(
        "INSERT INTO \"plugin_tether.ship-replacement\".requests (fleet_id, account_id, \
             character_id, character_name, killmail_id, killmail_hash, killboard_link, \
             ship_type_id, ship_name, killmail_time, created_at) \
         SELECT $1, 1, 90000000 + n, 'Pilot ' || n, 5000 + n, 'h', \
             'https://zkillboard.com/kill/' || (5000 + n) || '/', 587, 'Rifter', now(), \
             now() - make_interval(secs => 1000 - n) \
         FROM generate_series(1, 401) n",
    )
    .bind(fleet)
    .execute(&h.db)
    .await
    .unwrap();
    let newest = request_of(&h, 5401).await;

    let first = open(&h, &manager, &format!("fleet/{fleet}")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert!(
        first.body.contains("SRP Requests 1 to 400 of 401"),
        "{}",
        first.body
    );
    assert!(!first.body.contains(&format!("review/{newest}\"")));
    let second = format!("fleet/{fleet}/page/2");
    assert!(
        first
            .body
            .contains(&format!("href=\"{}\">Page 2 of 2</a>", at(&second))),
        "{}",
        first.body
    );
    let page2 = open(&h, &manager, &second).await;
    assert_eq!(page2.status, StatusCode::OK, "{}", page2.body);
    assert!(
        page2.body.contains("SRP Requests 401 to 401 of 401"),
        "{}",
        page2.body
    );
    assert!(
        page2.body.contains(&format!("review/{newest}\"")),
        "{}",
        page2.body
    );
    assert_eq!(
        open(&h, &manager, &format!("fleet/{fleet}/page/3"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Approved from its row on page 2, back to page 2.
    let res = post(
        &h,
        &manager,
        &second,
        &format!("_form=decide&request={newest}&decision=approve"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), at(&second));
    assert_eq!(status_of(&h, newest).await.0, "approved");
}
