//! The Structure Timers plugin end to end: installed from its real
//! component and migration, timers created, edited and deleted through its
//! forms, AA's two permissions, and corporation-only timers seen and
//! edited only by the creator's corporation.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_plugins::testing::{self, Key};

const ID: &str = "tether.structure-timers";
/// Two pilots of the same (NPC) corporation, from the affiliation fixture.
const PILOT_A: &str = "443630591:Pilot A";
const PILOT_B: &str = "406944591:Pilot B";
const NPC_CORP: i64 = 1000167;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("structure-timers"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/structure-timers/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_structure_timers.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_structure_timers.sql", migration.as_bytes()),
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

/// A timer form's body: `extra` sets the time and flags.
fn timer_body(details: &str, extra: &str) -> String {
    format!(
        "_form=timer&details={details}&system=Jita&planet_moon=Jita+IV+-+Moon+4\
         &structure=Fortizar&timer_type=Armor&objective=Hostile{extra}"
    )
}

async fn create(h: &Harness, token: &str, details: &str, extra: &str) -> Res {
    send(
        &h.app,
        form(
            &format!("/plugins/{ID}/add"),
            &timer_body(details, extra),
            token,
        ),
    )
    .await
}

async fn timer_id(h: &Harness, details: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.structure-timers\".timers WHERE details = $1",
    )
    .bind(details)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn timers_page(h: &Harness, token: &str, tab: u32) -> Res {
    page(h, &format!("/plugins/{ID}?_tab={tab}"), token).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn structure_timers_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;

    // The owner holds everything: a timer from the time left, and one
    // from the EVE time as the game shows it.
    let res = create(
        &h,
        &owner,
        "Hostile+Fortizar",
        "&eve_time=&days=1&hours=2&minutes=30&important=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let past = (Utc::now() - Duration::hours(3)).format("%Y.%m.%d+%H:%M");
    let res = create(
        &h,
        &owner,
        "Old+Astrahus",
        &format!("&eve_time={past}&days=&hours=&minutes="),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // Mistakes come back with the form as typed.
    let both = create(
        &h,
        &owner,
        "Typo",
        "&eve_time=2026-09-30+18:00&days=1&hours=&minutes=",
    )
    .await;
    assert_eq!(both.status, StatusCode::OK, "{}", both.body);
    assert!(both.body.contains("not both"), "{}", both.body);
    assert!(both.body.contains("value=\"Typo\""), "{}", both.body);
    let neither = create(&h, &owner, "Typo", "&eve_time=&days=&hours=&minutes=").await;
    assert!(
        neither
            .body
            .contains("Give the EVE time, or the time left."),
        "{}",
        neither.body
    );

    // Upcoming, with its countdown and flags; past on its own tab.
    let list = timers_page(&h, &owner, 0).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert!(list.body.contains("Hostile Fortizar"), "{}", list.body);
    assert!(list.body.contains("1d 2h"), "{}", list.body);
    assert!(list.body.contains("Important"));
    assert!(list.body.contains("Jita IV - Moon 4"));
    assert!(list.body.contains("Create Timer"));
    assert!(!list.body.contains("Old Astrahus"), "{}", list.body);
    let old = timers_page(&h, &owner, 1).await;
    assert!(old.body.contains("Old Astrahus"), "{}", old.body);
    assert!(old.body.contains("ago"), "{}", old.body);

    // Members see timers once granted timer_view; managers get
    // timer_management too.
    let a = log_in_as(&h, PILOT_A, None).await;
    let b = log_in_as(&h, PILOT_B, None).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(
        timers_page(&h, &blue, 0).await.status,
        StatusCode::NOT_FOUND
    );
    grant(&h, &owner, "timer_view", MEMBER_STATE).await;
    grant(&h, &owner, "timer_view", BLUE_STATE).await;
    grant(&h, &owner, "timer_management", MEMBER_STATE).await;

    // Viewing only: no editing, no creating.
    let seen = timers_page(&h, &blue, 0).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Hostile Fortizar"));
    assert!(!seen.body.contains("Create Timer"), "{}", seen.body);
    let public = timer_id(&h, "Hostile Fortizar").await;
    for uri in ["add".to_owned(), format!("timer/{public}")] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/{uri}"), &blue)
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let refused = create(&h, &blue, "Sneaky", "&eve_time=&days=1&hours=&minutes=").await;
    assert!(refused.status.is_client_error(), "{}", refused.body);

    // A corporation timer: its corporation sees and edits it; nobody else
    // does, not even the owner.
    let res = create(
        &h,
        &a,
        "Our+Raitaru",
        "&eve_time=&days=2&hours=&minutes=&corp_timer=on",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let corp = timer_id(&h, "Our Raitaru").await;
    let stored: (i64, bool) = sqlx::query_as(
        "SELECT corporation_id, corp_timer FROM \"plugin_tether.structure-timers\".timers WHERE id = $1",
    )
    .bind(corp)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(stored, (NPC_CORP, true));
    let for_b = timers_page(&h, &b, 0).await;
    assert!(for_b.body.contains("Our Raitaru"), "{}", for_b.body);
    for other in [&owner, &blue] {
        let list = timers_page(&h, other, 0).await;
        assert!(!list.body.contains("Our Raitaru"), "{}", list.body);
    }
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/timer/{corp}"), &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let edit = page(&h, &format!("/plugins/{ID}/timer/{corp}"), &b).await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    assert!(edit.body.contains("value=\"Our Raitaru\""), "{}", edit.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/timer/{corp}"),
            &timer_body(
                "Our+Raitaru+(armor)",
                "&eve_time=2030-01-01+18:00&days=&hours=&minutes=&corp_timer=on",
            ),
            &b,
        ),
    )
    .await;
    // Over a year out: refused.
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("more than a year"), "{}", res.body);
    let soon = (Utc::now() + Duration::days(3)).format("%Y-%m-%d+%H:%M");
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/timer/{corp}"),
            &timer_body(
                "Our+Raitaru+(armor)",
                &format!("&eve_time={soon}&days=&hours=&minutes=&corp_timer=on"),
            ),
            &b,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(
        timers_page(&h, &a, 0)
            .await
            .body
            .contains("Our Raitaru (armor)")
    );
    // Posting to it from outside the corporation finds nothing.
    for body in [
        "_form=delete&confirm=on".to_owned(),
        timer_body("Hijacked", "&eve_time=&days=1&hours=&minutes="),
    ] {
        let res = send(
            &h.app,
            form(&format!("/plugins/{ID}/timer/{corp}"), &body, &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    }
    assert_eq!(timer_id(&h, "Our Raitaru (armor)").await, corp);

    let edited = page(&h, &format!("/plugins/{ID}/timer/{corp}"), &a).await;
    assert!(edited.body.contains("Last edited by"), "{}", edited.body);
    assert!(edited.body.contains("Pilot B"), "{}", edited.body);

    // A manager in another corporation can't hide a public timer by making
    // it corporation-only.
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/timer/{public}"),
            &timer_body(
                "Hostile+Fortizar",
                "&eve_time=&days=1&hours=&minutes=&corp_timer=on",
            ),
            &a,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("Only the creator&#39;s corporation"),
        "{}",
        res.body
    );
    assert!(
        timers_page(&h, &blue, 0)
            .await
            .body
            .contains("Hostile Fortizar")
    );

    // A manager in another corporation deletes a public timer; delete needs
    // the confirmation.
    let res = send(
        &h.app,
        form(&format!("/plugins/{ID}/timer/{public}"), "_form=delete", &a),
    )
    .await;
    assert!(res.status.is_client_error(), "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/timer/{public}"),
            "_form=delete&confirm=on",
            &a,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let list = timers_page(&h, &owner, 0).await;
    assert!(!list.body.contains("Hostile Fortizar"), "{}", list.body);

    // No warnings or errors in the plugin's log.
    let problems: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(problems.is_empty(), "{problems:?}");
}
