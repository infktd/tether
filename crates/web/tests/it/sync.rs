use crate::common::*;
use serde_json::json;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{NewJob, Outcome, Registry, WorkerConfig, run_once};
use tether_web::sync::{AFFILIATION_SYNC_JOB, affiliation_sync};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const CHRIBBA: &str = "196379789:Chribba";
const OTHERWORLD: i64 = 159826257;

async fn member_rule(db: &PgPool) {
    cover(db, Builtin::Member, EntityKind::Alliance, OTHERWORLD).await;
}

/// Like ESI: 404 for the whole batch if any id is unknown; Chribba's
/// affiliation as given.
async fn strict_esi(h: &Harness, chribba_alliance: Option<i64>) {
    h.esi_server.reset().await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(move |req: &wiremock::Request| {
            let ids: Vec<i64> = serde_json::from_slice(&req.body).unwrap();
            if ids.iter().any(|id| *id != 196379789) {
                return ResponseTemplate::new(404)
                    .set_body_json(json!({"error": "Ensure all IDs are valid"}));
            }
            let mut row = json!({"character_id": 196379789, "corporation_id": 1164409536});
            match chribba_alliance {
                Some(a) => row["alliance_id"] = a.into(),
                None => row["corporation_id"] = 1000167.into(), // an NPC corp
            }
            ResponseTemplate::new(200).set_body_json(vec![row])
        })
        .mount(&h.esi_server)
        .await;
}

fn registry(h: &Harness) -> Registry {
    let mut registry = Registry::new();
    tether_web::sync::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    registry
}

async fn run_sync_job(h: &Harness) -> Outcome {
    tether_jobs::enqueue(&h.db, NewJob::new(AFFILIATION_SYNC_JOB, json!({})))
        .await
        .unwrap();
    run_once(&h.db, &registry(h), &WorkerConfig::default())
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_who_leaves_the_alliance_drops_to_guest_within_one_sync(db: PgPool) {
    member_rule(&db).await;
    let h = harness(db, true).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(me(&h, &token).await["state"], "Member");

    // Chribba leaves the alliance; nobody touches Tether.
    strict_esi(&h, None).await;
    assert!(matches!(run_sync_job(&h).await, Outcome::Succeeded(_)));

    assert_eq!(me(&h, &token).await["state"], "Guest");
    let change: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'state.change' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(change, json!({"from": "Member", "to": "Guest"}));
    let last = tether_db::settings::get(&h.db, tether_web::sync::LAST_RUN_SETTING)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last["state_changes"], 1);
    assert!(last["at"].is_string());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_invalid_id_is_isolated_and_skipped(db: PgPool) {
    member_rule(&db).await;
    let h = harness(db, true).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    for ghost in ["1:Ghost One", "2:Ghost Two", "3:Ghost Three"] {
        log_in_as(&h, ghost, None).await;
    }
    strict_esi(&h, None).await;

    let summary = affiliation_sync(&h.db, &h.esi).await.unwrap();

    assert_eq!(summary.characters, 4);
    assert_eq!(summary.skipped, [1, 2, 3]);
    assert_eq!(
        me(&h, &token).await["state"],
        "Guest",
        "Chribba was still updated"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_esi_outage_retries_and_changes_nothing(db: PgPool) {
    member_rule(&db).await;
    let h = harness(db, true).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    h.esi_server.reset().await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "down"})))
        .mount(&h.esi_server)
        .await;

    assert!(matches!(run_sync_job(&h).await, Outcome::Retrying(_)));
    assert_eq!(me(&h, &token).await["state"], "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_sync_is_scheduled_hourly(db: PgPool) {
    for spec in tether_web::sync::schedules() {
        tether_jobs::schedule::ensure(&db, &spec).await.unwrap();
    }
    let every: i32 =
        sqlx::query_scalar("SELECT every_secs FROM core.schedules WHERE kind = 'affiliation.sync'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(every, 3600);
}
