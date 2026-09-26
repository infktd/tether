#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Auto Groups (AA's): a group per main's corporation and alliance for the
//! states a config covers, kept by Tether, never edited by hand.

mod common;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536, alliance 159826257
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456

async fn mount_tickers(h: &Harness) {
    for (route, file) in [
        ("/alliances/159826257", "alliances_159826257.json"),
        ("/corporations/1164409536", "corporations_1164409536.json"),
    ] {
        let body = std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/esi/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&h.esi_server)
            .await;
    }
}

async fn groups_of(h: &Harness, token: &str) -> Vec<String> {
    let mut groups: Vec<String> = me(h, token).await["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap().to_owned())
        .filter(|g| g != "Compliant")
        .collect();
    groups.sort();
    groups
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn members_get_their_corporation_and_alliance_groups(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    mount_tickers(&h).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let blue = log_in_as(&h, GIGX, None).await;

    // Needs a state, and at least one kind.
    let bad = send(
        &h.app,
        form(
            "/admin/autogroups",
            "corp_prefix=Corp+&corp_source=name&alliance_prefix=&alliance_source=name",
            &owner,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let res = send(
        &h.app,
        form(
            "/admin/autogroups",
            &format!(
                "states={MEMBER_STATE}&corp_groups=on&corp_prefix=Corp+&corp_source=name\
                 &alliance_groups=on&alliance_prefix=Alliance+&alliance_source=ticker"
            ),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/autogroups", "{}", res.body);

    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    assert_eq!(
        groups_of(&h, &owner).await,
        ["Alliance OTHER", "Corp Otherworld Enterprises"]
    );
    // Blue isn't covered.
    assert!(groups_of(&h, &blue).await.is_empty());
    let listed = page(&h, "/admin/autogroups", &owner).await;
    assert!(
        listed.body.contains("Corp Otherworld Enterprises"),
        "{}",
        listed.body
    );

    // Hands off: they're Tether's.
    let group: i64 = sqlx::query_scalar("SELECT group_id FROM core.autogroup_groups LIMIT 1")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let blue_account = me(&h, &blue).await["account_id"].as_i64().unwrap();
    let add = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{group}/members"),
            &owner,
            &format!(r#"{{"account_id":{blue_account}}}"#),
        ),
    )
    .await;
    assert_eq!(add.status, StatusCode::BAD_REQUEST);
    assert!(add.body.contains("Auto Group"), "{}", add.body);
    let delete = send(
        &h.app,
        form(&format!("/admin/groups/{group}/delete"), "", &owner),
    )
    .await;
    assert_eq!(delete.status, StatusCode::BAD_REQUEST);

    // Leaving the covered state takes the groups away at once.
    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, 159826257)
        .await
        .unwrap();
    let account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
    assert!(groups_of(&h, &owner).await.is_empty());
    // Empty groups go at the next sync.
    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.autogroup_groups")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_auto_group_never_takes_over_an_existing_group(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // Someone's group already has the name, with its own grants.
    let res = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Corp Otherworld Enterprises"}"#,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    send(
        &h.app,
        form(
            "/admin/autogroups",
            &format!(
                "states={MEMBER_STATE}&corp_groups=on&corp_prefix=Corp+&corp_source=name\
                 &alliance_prefix=Alliance+&alliance_source=name"
            ),
            &owner,
        ),
    )
    .await;
    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    assert!(groups_of(&h, &owner).await.is_empty());
    let auto: i64 = sqlx::query_scalar("SELECT count(*) FROM core.autogroup_groups")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(auto, 0);
    // And the sync it queued for the missing group waits for the hour.
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'autogroups.sync' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(queued <= 1, "{queued}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deleting_a_config_deletes_its_groups(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    send(
        &h.app,
        form(
            "/admin/autogroups",
            &format!(
                "states={MEMBER_STATE}&corp_groups=on&corp_prefix=Corp+&corp_source=name\
                 &alliance_prefix=Alliance+&alliance_source=name"
            ),
            &owner,
        ),
    )
    .await;
    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    assert_eq!(groups_of(&h, &owner).await, ["Corp Otherworld Enterprises"]);
    let config: i64 = sqlx::query_scalar("SELECT id FROM core.autogroup_configs")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = send(
        &h.app,
        form(&format!("/admin/autogroups/{config}/delete"), "", &owner),
    )
    .await;
    assert_eq!(res.location(), "/admin/autogroups");
    assert!(groups_of(&h, &owner).await.is_empty());
    let groups: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.groups WHERE name = 'Corp Otherworld Enterprises'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(groups, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn guest_is_never_covered_and_configured_groups_stay(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let guest = send(
        &h.app,
        form(
            "/admin/autogroups",
            &format!(
                "states={GUEST_STATE}&corp_groups=on&corp_prefix=Corp+&corp_source=name\
                 &alliance_prefix=&alliance_source=name"
            ),
            &owner,
        ),
    )
    .await;
    assert_eq!(guest.status, StatusCode::BAD_REQUEST);
    assert!(guest.body.contains("Guest"), "{}", guest.body);

    send(
        &h.app,
        form(
            "/admin/autogroups",
            &format!(
                "states={MEMBER_STATE}&corp_groups=on&corp_prefix=Corp+&corp_source=name\
                 &alliance_prefix=&alliance_source=name"
            ),
            &owner,
        ),
    )
    .await;
    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    let group: i64 = sqlx::query_scalar("SELECT group_id FROM core.autogroup_groups")
        .fetch_one(&h.db)
        .await
        .unwrap();
    // An Auto Group can't lead others: anyone in the corporation is in it.
    let officers = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Officers"}"#),
    )
    .await;
    let officers: serde_json::Value = serde_json::from_str(&officers.body).unwrap();
    let res = send(
        &h.app,
        axum::http::Request::put(format!(
            "/api/admin/groups/{}/leader-groups/{group}",
            officers["id"]
        ))
        .header(axum::http::header::ORIGIN, SITE)
        .header(axum::http::header::COOKIE, format!("{SESSION}={owner}"))
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    // Granted something, then emptied: kept (with its grant) for when
    // members come back.
    let grant = format!(r#"{{"permission":"fleet.ping","group_id":{group}}}"#);
    let res = send(
        &h.app,
        post_json("/api/admin/permissions/grants", &owner, &grant),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, 159826257)
        .await
        .unwrap();
    tether_web::autogroups::sync(&h.db, &h.esi).await.unwrap();
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM core.autogroup_groups")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(kept, 1);
}
