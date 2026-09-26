use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// From tests/fixtures/esi/characters_affiliation.json.
const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536, alliance 159826257
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456
const MITTANI: &str = "443630591:The Mittani"; // NPC corp 1000167, no alliance

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn states_follow_the_main_at_login(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;

    let member = log_in_as(&h, CHRIBBA, None).await;
    let blue = log_in_as(&h, GIGX, None).await;
    let guest = log_in_as(&h, MITTANI, None).await;

    assert_eq!(state_of(&h, &member).await, "Member");
    assert_eq!(state_of(&h, &blue).await, "Blue");
    assert_eq!(state_of(&h, &guest).await, "Guest");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_character_can_be_covered_on_its_own_and_priority_decides(db: PgPool) {
    // The Mittani is in an NPC corporation: only listing the character
    // makes them Blue.
    cover(&db, Builtin::Blue, EntityKind::Character, 443630591).await;
    // Chribba's alliance is Blue, but Chribba is also listed under Member,
    // which is higher.
    cover(&db, Builtin::Blue, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Character, 196379789).await;
    let h = harness(db, true).await;

    let mittani = log_in_as(&h, MITTANI, None).await;
    let chribba = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(state_of(&h, &mittani).await, "Blue");
    assert_eq!(state_of(&h, &chribba).await, "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn switching_main_re_evaluates_the_state(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let token = log_in_as(&h, CHRIBBA, None).await;
    let token = log_in_as(&h, MITTANI, Some(&token)).await;
    assert_eq!(state_of(&h, &token).await, "Member");

    let res = send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":443630591}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert_eq!(state_of(&h, &token).await, "Guest");

    send(
        &h.app,
        post_json("/api/me/main", &token, r#"{"character_id":196379789}"#),
    )
    .await;
    assert_eq!(state_of(&h, &token).await, "Member");

    // Each change is in the audit log, attributed to the system.
    let changes: Vec<(Option<i64>, serde_json::Value)> = sqlx::query_as(
        "SELECT actor_account_id, details FROM core.audit_log WHERE action = 'state.change' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    let to: Vec<&str> = changes
        .iter()
        .map(|(_, d)| d["to"].as_str().unwrap())
        .collect();
    assert_eq!(to, ["Member", "Guest", "Member"]);
    assert!(changes.iter().all(|(actor, _)| actor.is_none()));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_outage_at_login_queues_a_retry_that_fixes_the_state(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let esi_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(ResponseTemplate::new(503).set_body_json(
            serde_json::json!({"error": "The datasource tranquility is temporarily unavailable"}),
        ))
        .mount(&esi_server)
        .await;
    let h = harness_with_esi(db, true, esi_server).await;

    // Login still works; the state stays at the default until ESI is back.
    let token = log_in_as(&h, CHRIBBA, None).await;
    assert_eq!(state_of(&h, &token).await, "Guest");
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'states.refresh_account' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);

    // ESI recovers; the queued job runs and fixes the state.
    h.esi_server.reset().await;
    mount_affiliations(&h.esi_server).await;
    let mut registry = tether_jobs::Registry::new();
    tether_web::states::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    let outcome = tether_jobs::run_once(&h.db, &registry, &tether_jobs::WorkerConfig::default())
        .await
        .unwrap();

    assert!(matches!(outcome, tether_jobs::Outcome::Succeeded(_)));
    assert_eq!(state_of(&h, &token).await, "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_state_can_cover_a_faction(db: PgPool) {
    const CALDARI_STATE: i64 = 500001;
    // The Mittani, enlisted in the Caldari militia (AA's Member Factions).
    cover(&db, Builtin::Blue, EntityKind::Faction, CALDARI_STATE).await;
    let h = harness(db, true).await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "character_id": 443630591,
                "corporation_id": 1000167,
                "faction_id": CALDARI_STATE,
            }])),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let mittani = log_in_as(&h, MITTANI, None).await;
    assert_eq!(state_of(&h, &mittani).await, "Blue");
    let faction: Option<i64> =
        sqlx::query_scalar("SELECT faction_id FROM core.characters WHERE id = 443630591")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(faction, Some(CALDARI_STATE));

    // Admins find factions by name, as alliances and corporations.
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "factions": [{ "id": CALDARI_STATE, "name": "Caldari State" }],
        })))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let res = send(
        &h.app,
        form(
            "/admin/states/search",
            &format!("state_id={BLUE_STATE}&name=Caldari+State"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Caldari State"), "{}", res.body);
    assert!(res.body.contains("faction"), "{}", res.body);
    let api = send(
        &h.app,
        post_json(
            "/api/admin/states/resolve",
            &owner,
            r#"{"names":["Caldari State"]}"#,
        ),
    )
    .await;
    let api: serde_json::Value = serde_json::from_str(&api.body).unwrap();
    assert_eq!(api["factions"][0]["id"], CALDARI_STATE);

    // Adding one always asks first, with a warning: anyone can enlist.
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CALDARI_STATE, "name": "Caldari State", "category": "faction" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let add = format!("/admin/states/{MEMBER_STATE}/covers");
    let asked = send(
        &h.app,
        form(&add, &format!("entity_id={CALDARI_STATE}"), &owner),
    )
    .await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert!(asked.body.contains("enlists"), "{}", asked.body);
    let listed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.state_entities WHERE state_id = $1 AND entity_kind = 'faction'",
    )
    .bind(MEMBER_STATE)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(listed, 0, "not until confirmed");
    let added = send(
        &h.app,
        form(
            &add,
            &format!("entity_id={CALDARI_STATE}&confirm=1"),
            &owner,
        ),
    )
    .await;
    assert_eq!(added.location(), "/admin/states", "{}", added.body);
    // Member outranks Blue once the accounts are re-evaluated.
    let account = me(&h, &mittani).await["account_id"].as_i64().unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
    assert_eq!(state_of(&h, &mittani).await, "Member");
}
