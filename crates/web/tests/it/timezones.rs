//! The Time Zones app end to end (aa-timezones): installed from its real
//! component and migration; open to anyone signed in; aa-timezones'
//! default panels until an admin sets their own; a pilot's own zone; and
//! times adjusted for a timer or a planned fleet, on a page to share.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, TimeZone, Utc};
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};

const ID: &str = "tether.timezones";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("timezones")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/timezones/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_timezones.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_timezones.sql", migration.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn time_zones_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;

    // Anyone signed in, with no permission (a Guest here), as aa-timezones.
    let pilot = log_in(&h, None).await;
    let res = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for text in [
        "EVE time",
        "Pick your time zone",
        "US / Pacific",
        "Australia / Sydney",
        "UTC+",
    ] {
        assert!(res.body.contains(text), "{text}: {}", res.body);
    }
    // Panels are for managers.
    assert!(!res.body.contains(&format!("/plugins/{ID}/panels")));
    let res = page(&h, &format!("/plugins/{ID}/panels"), &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // Their own zone, by its IANA name.
    let res = post(&h, &pilot, "mine", "_form=mine&zone=Not%2FAZone").await;
    assert!(
        res.body.contains("isn&#39;t a time zone Tether knows"),
        "{}",
        res.body
    );
    let res = post(&h, &pilot, "mine", "_form=mine&zone=Europe%2FBerlin").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert!(res.body.contains("Your time"), "{}", res.body);
    assert!(!res.body.contains("Pick your time zone"), "{}", res.body);

    // A planned fleet: 19:30 in Berlin on a summer day is 17:30 EVE.
    let res = post(
        &h,
        &pilot,
        "adjust",
        "_form=fixed&date=2026-07-01&time=19%3A30&zone=Europe%2FBerlin",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let at = Utc.with_ymd_and_hms(2026, 7, 1, 17, 30, 0).unwrap();
    assert_eq!(
        res.location(),
        format!("/plugins/{ID}/at/{}", at.timestamp())
    );
    let fleet = page(&h, res.location(), &pilot).await;
    assert_eq!(fleet.status, StatusCode::OK, "{}", fleet.body);
    assert!(
        fleet
            .body
            .contains("Every time zone at 2026-07-01 17:30:00 EVE"),
        "{}",
        fleet.body
    );
    // That's over by now: aa-timezones says so instead of a time left.
    assert!(fleet.body.contains("Time left"), "{}", fleet.body);
    assert!(fleet.body.contains("Already over"), "{}", fleet.body);
    assert!(!fleet.body.contains("data-countdown"), "{}", fleet.body);
    // US / Eastern is UTC-4 in summer.
    assert!(fleet.body.contains("13:30"), "{}", fleet.body);
    assert!(fleet.body.contains("UTC-04:00"), "{}", fleet.body);
    assert!(
        fleet
            .body
            .contains(&format!("/plugins/{ID}/at/{}", at.timestamp())),
        "{}",
        fleet.body
    );
    // A timer: in 1 day and 2 hours.
    let before = Utc::now();
    let res = post(
        &h,
        &pilot,
        "adjust",
        "_form=timer&days=1&hours=2&minutes=0&seconds=0",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let when: i64 = res.location().rsplit('/').next().unwrap().parse().unwrap();
    let expected = (before + Duration::hours(26)).timestamp();
    assert!((when - expected).abs() < 60, "{when} {expected}");
    // Its page counts down to it, kept current while open.
    let timer = page(&h, res.location(), &pilot).await;
    assert!(timer.body.contains("Time left"), "{}", timer.body);
    assert!(timer.body.contains("data-countdown"), "{}", timer.body);
    assert!(!timer.body.contains("Already over"), "{}", timer.body);
    let res = post(
        &h,
        &pilot,
        "adjust",
        "_form=timer&days=8&hours=0&minutes=0&seconds=0",
    )
    .await;
    assert!(res.status != StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/at/12"), &pilot)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // The owner (every permission) sets the panels: adding one replaces
    // the defaults, as aa-timezones.
    let panels = page(&h, &format!("/plugins/{ID}/panels"), &owner).await;
    assert_eq!(panels.status, StatusCode::OK, "{}", panels.body);
    assert!(
        panels.body.contains("aa-timezones&#39; defaults"),
        "{}",
        panels.body
    );
    let res = post(
        &h,
        &owner,
        "panels",
        "_form=add&name=Home&zone=America%2FNew_York",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &owner,
        "panels",
        "_form=add&name=Bad&zone=Mars%2FOlympus",
    )
    .await;
    assert!(res.body.contains("isn&#39;t a time zone"), "{}", res.body);
    let res = page(&h, &format!("/plugins/{ID}"), &pilot).await;
    assert!(res.body.contains("Home"), "{}", res.body);
    assert!(!res.body.contains("US / Pacific"), "{}", res.body);
    let id: i32 = sqlx::query_scalar(r#"SELECT id FROM "plugin_tether.timezones".panels"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = post(&h, &owner, "panels", &format!("_form=delete&panel={id}")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &owner, "panels", "_form=defaults").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let count: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "plugin_tether.timezones".panels"#)
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(count, 10);
}
