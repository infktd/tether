#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The character viewer's catalogue entries against a mocked ESI: the host
//! fills in the character, a call reads only what it names, pages come
//! from `X-Pages`, and an enum value CCP added later doesn't break a read.

use serde_json::json;
use tether_core::Secret;
use tether_esi::plugin::{Response, Target, endpoint};
use tether_esi::{Esi, EsiError};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHARACTER: i64 = 2112000001;

async fn esi() -> (MockServer, Esi) {
    let server = MockServer::start().await;
    let esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
    (server, esi)
}

fn target() -> Target {
    Target {
        character_id: CHARACTER,
        corporation_id: 0,
    }
}

fn token() -> Secret<String> {
    Secret::new("character-token".to_owned())
}

fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

async fn get(
    esi: &Esi,
    name: &str,
    pairs: &[(&str, &str)],
    page: Option<u32>,
) -> Result<Response, EsiError> {
    let endpoint = endpoint(name).unwrap();
    esi.plugin_get(endpoint, &token(), target(), &params(pairs), page)
        .await
}

#[tokio::test]
async fn a_mail_body_is_the_one_mail_asked_for_with_the_characters_token() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/mail/77")))
        .and(header("authorization", "Bearer character-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "body": "Fleet at 19:00", "from": 90000001, "labels": [1], "read": true,
            "recipients": [{"recipient_id": CHARACTER, "recipient_type": "character"}],
            "subject": "Ops", "timestamp": "2026-09-20T19:04:05Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&esi, "character-mail-body", &[("mail_id", "77")], None)
        .await
        .unwrap();
    assert_eq!(out.body["body"], "Fleet at 19:00");
    assert_eq!(out.pages, 1);
    // No id, no call.
    let err = get(&esi, "character-mail-body", &[], None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::InvalidInput(_)), "{err:?}");
    // Headers only take an optional, numeric `last_mail_id`.
    let err = get(&esi, "character-mail", &[("last_mail_id", "x")], None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::InvalidInput(_)), "{err:?}");
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/mail")))
        .and(query_param("last_mail_id", "76"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&esi, "character-mail", &[("last_mail_id", "76")], None)
        .await
        .unwrap();
    assert_eq!(out.body, json!([]));
}

#[tokio::test]
async fn paged_reads_pass_the_page_and_its_count() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/mining")))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "3")
                .set_body_json(json!([{
                    "date": "2026-09-20", "quantity": 1000,
                    "solar_system_id": 30000142, "type_id": 1230
                }])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let out = get(&esi, "character-mining", &[], Some(2)).await.unwrap();
    assert_eq!(out.pages, 3);
    assert_eq!(out.body[0]["quantity"], 1000);
}

#[tokio::test]
async fn an_enum_value_ccp_adds_later_does_not_break_a_read() {
    let (server, esi) = esi().await;
    // A paged endpoint: read again as it is, pages included.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/blueprints")))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "2")
                .set_body_json(json!([{
                    "item_id": 1, "location_flag": "SomeBayCcpAddsNextYear",
                    "location_id": 60003760, "material_efficiency": 10,
                    "quantity": -2, "runs": 10, "time_efficiency": 20, "type_id": 1000
                }])),
        )
        .mount(&server)
        .await;
    let out = get(&esi, "character-blueprints", &[], None).await.unwrap();
    assert_eq!(out.pages, 2);
    assert_eq!(out.body[0]["location_flag"], "SomeBayCcpAddsNextYear");
    // Read twice: the plugin's budget counts the second request.
    assert_eq!(out.refetched, 1);

    // Not paged, with its query: read again the same way, every
    // notification kept.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/notifications")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"notification_id": 1, "sender_id": 1000125, "sender_type": "corporation",
             "timestamp": "2026-09-20T19:04:05Z", "type": "SomethingNewFromCcp", "text": "a: 1"},
            {"notification_id": 2, "sender_id": 1000125, "sender_type": "corporation",
             "timestamp": "2026-09-20T19:05:05Z", "type": "StructureUnderAttack", "text": "b: 2"}
        ])))
        .mount(&server)
        .await;
    let out = get(&esi, "character-notifications", &[], None)
        .await
        .unwrap();
    assert_eq!(out.body.as_array().unwrap().len(), 2);
    assert_eq!(out.body[0]["type"], "SomethingNewFromCcp");
    assert_eq!(out.refetched, 1);
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/calendar")))
        .and(query_param("from_event", "9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"event_date": "2026-10-01T19:00:00Z", "event_id": 10,
             "event_response": "maybe_later_ccp", "importance": 0, "title": "Ops"}
        ])))
        .mount(&server)
        .await;
    let out = get(&esi, "character-calendar", &[("from_event", "9")], None)
        .await
        .unwrap();
    assert_eq!(out.body[0]["event_response"], "maybe_later_ccp");

    // Every request, the second reads too, carries ESI's compatibility
    // date, and the token.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 6);
    for request in requests {
        assert!(request.headers.contains_key("x-compatibility-date"));
        assert_eq!(request.headers["authorization"], "Bearer character-token");
    }
}

#[tokio::test]
async fn an_error_answer_is_esis_status_never_data() {
    let (server, esi) = esi().await;
    // Not an ESI error body: the typed read fails on it, and the answer
    // is read again, raw, for its status.
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/mail/77")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "x"})))
        .mount(&server)
        .await;
    let err = get(&esi, "character-mail-body", &[("mail_id", "77")], None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::Status(403)), "{err:?}");
}

#[tokio::test]
async fn nothing_is_read_again_while_the_error_budget_is_low() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/contacts")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .insert_header("X-ESI-Error-Limit-Remain", "5")
                .insert_header("X-ESI-Error-Limit-Reset", "30")
                .set_body_json(json!([{
                    "contact_id": 90000002, "contact_type": "something_new", "standing": 5.0
                }])),
        )
        .mount(&server)
        .await;
    let err = get(&esi, "character-contacts", &[], None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::Unavailable(_)), "{err:?}");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ids_are_positive_numbers() {
    let (server, esi) = esi().await;
    for (name, param) in [
        ("character-mail-body", "mail_id"),
        ("character-contract-items", "contract_id"),
        ("character-planet", "planet_id"),
        ("character-calendar-event", "event_id"),
        ("universe-structure", "structure_id"),
    ] {
        for bad in ["0", "-5", "x", ""] {
            let err = get(&esi, name, &[(param, bad)], None).await.unwrap_err();
            assert!(
                matches!(err, EsiError::InvalidInput(_)),
                "{name} {bad}: {err:?}"
            );
        }
    }
    for (name, param) in [
        ("character-mail", "last_mail_id"),
        ("character-wallet-transactions", "from_id"),
        ("character-calendar", "from_event"),
    ] {
        let err = get(&esi, name, &[(param, "0")], None).await.unwrap_err();
        assert!(matches!(err, EsiError::InvalidInput(_)), "{name}: {err:?}");
    }
    // None reached ESI.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_structure_is_its_name_system_and_type_only() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path("/universe/structures/1030000000001"))
        .and(header("authorization", "Bearer character-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "Home Keepstar", "owner_id": 98000001, "solar_system_id": 30000142,
            "type_id": 35834, "position": {"x": 1.0, "y": 2.0, "z": 3.0}
        })))
        .mount(&server)
        .await;
    let out = get(
        &esi,
        "universe-structure",
        &[("structure_id", "1030000000001")],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        out.body,
        json!({
            "structure_id": 1030000000001_i64, "name": "Home Keepstar",
            "solar_system_id": 30000142, "type_id": 35834
        })
    );
}

#[tokio::test]
async fn public_entries_take_ids_and_no_token() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/corporationhistory")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"corporation_id": 98000001, "record_id": 2, "start_date": "2026-01-01T00:00:00Z"},
            {"corporation_id": 1000167, "record_id": 1, "start_date": "2025-01-01T00:00:00Z"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/stations/60003760"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "max_dockable_ship_volume": 50000000.0, "name": "Some Station",
            "office_rental_cost": 10000.0, "owner": 1000035, "position": {"x": 1.0, "y": 2.0, "z": 3.0},
            "race_id": 1, "reprocessing_efficiency": 0.5, "reprocessing_stations_take": 0.05,
            "services": ["market", "a-service-ccp-adds-later"], "station_id": 60003760,
            "system_id": 30000142, "type_id": 1531
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "achievement_score": 10, "birthday": "2008-05-02T12:00:00Z", "bloodline_id": 5,
            "corporation_id": 98000001, "description": "o7", "gender": "male",
            "name": "Some Pilot", "race_id": 2, "security_status": -1.25
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/categories/16"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "category_id": 16, "groups": [255, 256], "name": "Skill", "published": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/groups/255"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "category_id": 16, "group_id": 255, "name": "Gunnery", "published": true,
            "types": [3300, 3301]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/markets/prices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"adjusted_price": 5210.4, "average_price": 5423.1, "type_id": 45492},
            {"adjusted_price": 12.5, "type_id": 34}
        ])))
        .mount(&server)
        .await;
    let hash = "b".repeat(40);
    Mock::given(method("GET"))
        .and(path(format!("/killmails/1002/{hash}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attackers": [{"character_id": 90000002, "damage_done": 500, "final_blow": true,
                           "security_status": 0.5, "ship_type_id": 587}],
            "killmail_id": 1002, "killmail_time": "2026-09-20T19:04:05Z", "solar_system_id": 30000142,
            "victim": {"character_id": CHARACTER, "damage_taken": 500, "ship_type_id": 670,
                       "items": [{"flag": 5, "item_type_id": 34, "quantity_destroyed": 10, "singleton": 0}]}
        })))
        .mount(&server)
        .await;

    let public = |name: &str, pairs: &[(&str, &str)]| {
        let endpoint = endpoint(name).unwrap();
        let params = params(pairs);
        let esi = esi.clone();
        async move { esi.plugin_get_public(endpoint, &params).await }
    };
    let history = public(
        "character-corporation-history",
        &[("character_id", &CHARACTER.to_string())],
    )
    .await
    .unwrap();
    assert_eq!(history.body[0]["corporation_id"], 98000001);
    let sheet = public(
        "character-public",
        &[("character_id", &CHARACTER.to_string())],
    )
    .await
    .unwrap();
    assert_eq!(sheet.body["security_status"], -1.25);
    assert_eq!(sheet.body["birthday"], "2008-05-02T12:00:00Z");
    // What a token reads isn't in it.
    assert!(sheet.body.get("corporation_id").is_none());
    let skills = public("universe-category", &[("category_id", "16")])
        .await
        .unwrap();
    assert_eq!(skills.body["groups"], json!([255, 256]));
    let gunnery = public("universe-group", &[("group_id", "255")])
        .await
        .unwrap();
    assert_eq!(gunnery.body["name"], "Gunnery");
    assert_eq!(gunnery.body["types"], json!([3300, 3301]));
    let station = public("universe-station", &[("station_id", "60003760")])
        .await
        .unwrap();
    assert_eq!(station.body["name"], "Some Station");
    let prices = public("markets-prices", &[]).await.unwrap();
    assert_eq!(prices.body[0]["type_id"], 45492);
    assert_eq!(prices.body[0]["average_price"], 5423.1);
    // A type without an average price has none, not zero.
    assert!(prices.body[1]["average_price"].is_null());
    let killmail = public(
        "killmail-detail",
        &[("killmail_id", "1002"), ("killmail_hash", &hash)],
    )
    .await
    .unwrap();
    assert_eq!(killmail.body["attackers"][0]["damage_done"], 500);
    assert_eq!(killmail.body["victim"]["items"][0]["item_type_id"], 34);
    let err = public("killmail-detail", &[("killmail_id", "1002")])
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::InvalidInput(_)), "{err:?}");
    // None of them sent a token.
    for request in server.received_requests().await.unwrap() {
        assert!(!request.headers.contains_key("authorization"));
    }
}
