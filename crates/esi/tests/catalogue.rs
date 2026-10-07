#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The character viewer's catalogue entries against a mocked ESI: the host
//! fills in the character, a call reads only what it names, pages come
//! from `X-Pages`, and an enum value CCP added later doesn't break a read.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tether_core::Secret;
use tether_esi::asset_places::TokenSource;
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
        alliance_id: None,
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

/// Member Audit's assets and wallet journal (and Blueprints' personal
/// places): a hold or a journal ref type newer than Tether's ESI client
/// doesn't fail the page, which keeps its page count.
#[tokio::test]
async fn character_assets_and_journal_take_values_ccp_adds_later() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/assets")))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "3")
                .set_body_json(json!([{
                    "is_singleton": true, "item_id": 1_040_000_000_001_i64, "type_id": 691,
                    "location_flag": "SomeHoldCcpAddsNextYear", "location_id": 1_040_000_000_002_i64,
                    "location_type": "item", "quantity": 1
                }])),
        )
        .mount(&server)
        .await;
    let out = get(&esi, "character-assets", &[], Some(2)).await.unwrap();
    assert_eq!(out.pages, 3);
    assert_eq!(out.body[0]["location_flag"], "SomeHoldCcpAddsNextYear");
    assert_eq!(out.refetched, 1);
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/wallet/journal")))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(json!([{
                    "id": 77, "date": "2026-10-01T19:00:00Z", "description": "A new fee",
                    "ref_type": "some_fee_ccp_adds_next_year", "amount": -1000.0,
                    "context_id": 5, "context_id_type": "some_context_ccp_adds_later"
                }])),
        )
        .mount(&server)
        .await;
    let out = get(&esi, "character-wallet-journal", &[], None)
        .await
        .unwrap();
    assert_eq!(out.pages, 1);
    assert_eq!(out.body[0]["ref_type"], "some_fee_ccp_adds_next_year");
    assert_eq!(
        out.body[0]["context_id_type"],
        "some_context_ccp_adds_later"
    );
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
        ("corporation-contract-items", "contract_id"),
        ("source-structure", "structure_id"),
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

#[tokio::test]
async fn names_become_type_ids_and_a_type_has_its_dogma() {
    let (server, esi) = esi().await;
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .and(wiremock::matchers::body_json(json!([
            "Rifter",
            "Damage Control II"
        ])))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "inventory_types": [
                {"id": 587, "name": "Rifter"},
                {"id": 2048, "name": "Damage Control II"}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/types/2048"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type_id": 2048, "name": "Damage Control II", "description": "Hull repair",
            "group_id": 60, "published": true,
            "dogma_attributes": [{"attribute_id": 182, "value": 3318.0},
                                 {"attribute_id": 277, "value": 5.0}]
        })))
        .mount(&server)
        .await;
    let public = |name: &str, pairs: &[(&str, &str)]| {
        let endpoint = endpoint(name).unwrap();
        let params = params(pairs);
        let esi = esi.clone();
        async move { esi.plugin_get_public(endpoint, &params).await }
    };
    // Blank lines and repeats aren't sent.
    let ids = public(
        "universe-ids",
        &[("names", "Rifter\n\nDamage Control II\nRifter")],
    )
    .await
    .unwrap();
    assert_eq!(ids.body["inventory_types"][1]["id"], 2048);
    let item = public("universe-type", &[("type_id", "2048")])
        .await
        .unwrap();
    assert_eq!(item.body["group_id"], 60);
    assert_eq!(item.body["dogma_attributes"][0]["attribute_id"], 182);
    // Checked before ESI is asked.
    for bad in [
        public("universe-ids", &[("names", "")]).await,
        public("universe-ids", &[]).await,
        public("universe-type", &[("type_id", "-1")]).await,
    ] {
        assert!(matches!(bad, Err(EsiError::InvalidInput(_))), "{bad:?}");
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
}

const CORPORATION: i64 = 98000001;

async fn get_corporate(
    esi: &Esi,
    name: &str,
    pairs: &[(&str, &str)],
    page: Option<u32>,
) -> Result<Response, EsiError> {
    let endpoint = endpoint(name).unwrap();
    let target = Target {
        character_id: CHARACTER,
        corporation_id: CORPORATION,
        alliance_id: None,
    };
    esi.plugin_get(endpoint, &token(), target, &params(pairs), page)
        .await
}

/// Asks for places until the background read has finished (or failed).
async fn places(
    esi: &Esi,
    tokens: TokenSource,
    character: i64,
    ids: &str,
) -> Result<Response, EsiError> {
    let params = params(&[("item_ids", ids)]);
    for _ in 0..500 {
        match esi
            .corporation_asset_places(tokens.clone(), CORPORATION, character, &params)
            .await
        {
            Err(EsiError::Pending) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            other => return other,
        }
    }
    panic!("the assets were never read");
}

/// A token source handing out `tokens` in turn, the last one after that.
fn tokens(tokens: &[&str]) -> TokenSource {
    let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
    let next = Arc::new(AtomicUsize::new(0));
    Arc::new(move || {
        let i = next.fetch_add(1, Ordering::SeqCst).min(tokens.len() - 1);
        let token = Secret::new(tokens[i].clone());
        Box::pin(async move { Ok(token) })
    })
}

fn asset(item: i64, type_id: i64, flag: &str, at: i64, kind: &str) -> serde_json::Value {
    json!({
        "is_singleton": true, "item_id": item, "type_id": type_id, "quantity": 1,
        "location_flag": flag, "location_id": at, "location_type": kind
    })
}

#[tokio::test]
async fn an_items_place_is_its_station_and_the_containers_between_only() {
    let (server, esi) = esi().await;
    // The office in Jita 4-4, a container in its second hangar, a
    // blueprint in that; and something else of the corporation's.
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/assets")))
        .and(query_param("page", "1"))
        .and(header("authorization", "Bearer character-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "2")
                .set_body_json(json!([
                    asset(1001, 27, "OfficeFolder", 60003760, "station"),
                    asset(2001, 17366, "CorpSAG2", 1001, "item"),
                ])),
        )
        // The read, then once for each answer: ESI checks the character's
        // roles on every call.
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/assets")))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "2")
                .set_body_json(json!([
                    asset(3001, 1000, "Unlocked", 2001, "item"),
                    asset(4001, 34, "Hangar", 60008494, "station"),
                ])),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Bad ids are refused before anything is read.
    let err = get_corporate(&esi, "corporation-asset-places", &[("item_ids", "0")], None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::InvalidInput(_)), "{err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
    // The first call starts the read in the background, and says so.
    let asked = [("item_ids", "3001,1001,9999")];
    let err = get_corporate(&esi, "corporation-asset-places", &asked, None)
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::Pending), "{err:?}");
    let mut out = None;
    for _ in 0..500 {
        match get_corporate(&esi, "corporation-asset-places", &asked, None).await {
            Err(EsiError::Pending) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            other => {
                out = Some(other.unwrap());
                break;
            }
        }
    }
    let out = out.expect("the assets were read");
    assert_eq!(
        out.body,
        // In id order; 9999 isn't the corporation's, and 4001 wasn't asked.
        json!([
            {
                "item_id": 1001, "type_id": 27, "location_flag": "OfficeFolder", "within": [],
                "place_id": 60003760, "place_type": "station"
            },
            {
                "item_id": 3001, "type_id": 1000, "location_flag": "Unlocked",
                "within": [
                    {"type_id": 17366, "location_flag": "CorpSAG2"},
                    {"type_id": 27, "location_flag": "OfficeFolder"}
                ],
                "place_id": 60003760, "place_type": "station"
            }
        ])
    );
    // The pages were read in the background: the call isn't charged them.
    assert_eq!(out.refetched, 0);
    // Within the hour, from the same read: only the first page is asked
    // again (the second expects one request).
    let again = get_corporate(
        &esi,
        "corporation-asset-places",
        &[("item_ids", "2001")],
        None,
    )
    .await
    .unwrap();
    assert_eq!(again.body[0]["place_id"], 60003760);
}

/// More than 50 pages, the old limit: the asked container is on the last.
#[tokio::test]
async fn every_page_of_a_large_corporations_assets_is_read() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/assets")))
        .and(query_param("page", "120"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "120")
                .set_body_json(json!([
                    asset(1_040_000_000_101, 27, "OfficeFolder", 60003760, "station"),
                    asset(
                        1_040_000_000_201,
                        17366,
                        "CorpSAG2",
                        1_040_000_000_101,
                        "item"
                    ),
                ])),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/assets")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "120")
                .set_body_json(json!([])),
        )
        .with_priority(2)
        // Pages 1 to 119, then the first again for the answer.
        .expect(120)
        .mount(&server)
        .await;
    let out = places(
        &esi,
        tokens(&["character-token"]),
        CHARACTER,
        "1040000000201",
    )
    .await
    .unwrap();
    assert_eq!(out.body[0]["place_id"], 60003760);
    assert_eq!(out.body[0]["within"][0]["location_flag"], "OfficeFolder");
}

#[tokio::test]
async fn a_failed_read_answers_its_error_without_reading_again() {
    let (server, esi) = esi().await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/assets")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": "Forbidden"})))
        .expect(1)
        .mount(&server)
        .await;
    let source = tokens(&["character-token"]);
    let err = places(&esi, source.clone(), CHARACTER, "1001")
        .await
        .unwrap_err();
    assert!(matches!(err, EsiError::Status(403)), "{err:?}");
    let err = places(&esi, source, CHARACTER, "1001").await.unwrap_err();
    assert!(matches!(err, EsiError::Status(403)), "{err:?}");
}

/// A read of many pages outlives one access token: each page asks the
/// vault (here, a list) for the token to send, and so does the answer's
/// check of the first page.
#[tokio::test]
async fn each_page_is_read_with_a_fresh_token() {
    let (server, esi) = esi().await;
    for (page, token) in [("1", "t1"), ("2", "t2"), ("1", "t3")] {
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .and(query_param("page", page))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "2")
                    .set_body_json(json!([asset(
                        1001 + page.parse::<i64>().unwrap(),
                        17366,
                        "CorpSAG1",
                        60003760,
                        "station"
                    )])),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    let out = places(&esi, tokens(&["t1", "t2", "t3"]), CHARACTER, "1002,1003")
        .await
        .unwrap();
    assert_eq!(out.body.as_array().unwrap().len(), 2);
}

/// A read answers only calls with the same character's token: another
/// data source of the same corporation has its own read (and ESI checks
/// each one's roles on its own calls).
#[tokio::test]
async fn another_characters_call_has_its_own_read() {
    let (server, esi) = esi().await;
    for (token, flag) in [("director", "CorpSAG1"), ("other", "CorpSAG2")] {
        Mock::given(method("GET"))
            .and(path(format!("/corporations/{CORPORATION}/assets")))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-pages", "1")
                    .set_body_json(json!([asset(1001, 17366, flag, 60003760, "station")])),
            )
            // The read, then the answer's check.
            .expect(2)
            .mount(&server)
            .await;
    }
    let first = places(&esi, tokens(&["director"]), CHARACTER, "1001")
        .await
        .unwrap();
    assert_eq!(first.body[0]["location_flag"], "CorpSAG1");
    let second = places(&esi, tokens(&["other"]), CHARACTER + 1, "1001")
        .await
        .unwrap();
    assert_eq!(second.body[0]["location_flag"], "CorpSAG2");
}

#[tokio::test]
async fn corporation_blueprints_and_running_jobs_are_esis() {
    let (server, esi) = esi().await;
    let blueprint = json!({
        "item_id": 3001, "type_id": 1000, "location_id": 2001, "location_flag": "CorpSAG2",
        "material_efficiency": 10, "time_efficiency": 20, "quantity": -1, "runs": -1
    });
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/blueprints")))
        .and(header("authorization", "Bearer character-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(json!([blueprint])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let out = get_corporate(&esi, "corporation-blueprints", &[], None)
        .await
        .unwrap();
    assert_eq!(out.body, json!([blueprint]));
    let job = json!({
        "activity_id": 5, "blueprint_id": 3001, "blueprint_location_id": 2001,
        "blueprint_type_id": 1000, "duration": 3600, "end_date": "2026-10-05T12:00:00Z",
        "facility_id": 60003760, "installer_id": CHARACTER, "job_id": 77,
        "location_id": 60003760, "output_location_id": 2001, "runs": 10,
        "start_date": "2026-10-05T11:00:00Z", "status": "active"
    });
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORPORATION}/industry/jobs")))
        .and(query_param("include_completed", "false"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(json!([job])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let out = get_corporate(&esi, "corporation-industry-jobs", &[], None)
        .await
        .unwrap();
    assert_eq!(out.body[0]["job_id"], 77);
    assert_eq!(out.body[0]["activity_id"], 5);
}

#[tokio::test]
async fn room_for_work_on_demand_follows_the_error_budget() {
    let (server, esi) = esi().await;
    // Nothing stated yet: plenty.
    assert!(esi.has_room());
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHARACTER}/skillqueue")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-esi-error-limit-remain", "20")
                .insert_header("x-esi-error-limit-reset", "30")
                .set_body_json(json!([])),
        )
        .mount(&server)
        .await;
    get(&esi, "character-skillqueue", &[], None).await.unwrap();
    // 20 errors left is under the bulk reserve: work waits for its turn.
    assert!(!esi.has_room());
}
