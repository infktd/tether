//! The Fleet Operations plugin end to end: installed from its real
//! component and migration, operations created, edited and deleted through
//! its forms and row buttons, AA's two permissions, operation types made as
//! they're used, and the Dashboard's Upcoming Fleets.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_plugins::testing::{self, Key};

const ID: &str = "tether.fleet-operations";
/// A Member pilot (an NPC corporation the test covers).
const PILOT_A: &str = "443630591:Pilot A";
const NPC_CORP: i64 = 1000167;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("fleet-operations"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/fleet-operations/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(11);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_fleet_operations.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_fleet_operations.sql", migration.as_bytes()),
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

/// An operation form's body starting at `start` (EVE time, form-encoded);
/// `extra` sets the type fields.
fn op_body(name: &str, start: &str, extra: &str) -> String {
    format!(
        "_form=op&operation_name={name}&doctrine=Ferox+Fleet&system=1DQ1-A&start={start}\
         &duration=2h&fc=Example+FC&description=Bring+drones{extra}"
    )
}

fn eve(offset: Duration) -> String {
    (Utc::now() + offset).format("%Y-%m-%d+%H:%M").to_string()
}

async fn create(h: &Harness, token: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/add"), body, token)).await
}

async fn op_id(h: &Harness, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT id FROM \"plugin_tether.fleet-operations\".ops WHERE operation_name = $1",
    )
    .bind(name)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

async fn type_id(h: &Harness, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM \"plugin_tether.fleet-operations\".op_types WHERE name = $1")
        .bind(name)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn ops_page(h: &Harness, token: &str, tab: u32) -> Res {
    page(h, &format!("/plugins/{ID}?_tab={tab}"), token).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fleet_operations_end_to_end(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Corporation, NPC_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;

    // The first operation: no types yet, so the type is typed and made.
    let add = page(&h, &format!("/plugins/{ID}/add"), &owner).await;
    assert_eq!(add.status, StatusCode::OK, "{}", add.body);
    assert!(!add.body.contains("name=\"type\""), "{}", add.body);
    let res = create(
        &h,
        &owner,
        &op_body(
            "Home+Defence",
            &eve(Duration::minutes(26 * 60 + 30)),
            "&new_type=CTA",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let cta = type_id(&h, "CTA").await;

    // After that it's offered, and typing it again in another case finds
    // the same type.
    let add = page(&h, &format!("/plugins/{ID}/add"), &owner).await;
    assert!(
        add.body
            .contains(&format!("<option value=\"{cta}\">CTA</option>")),
        "{}",
        add.body
    );
    let res = create(
        &h,
        &owner,
        &op_body(
            "Second+Fleet",
            &eve(Duration::hours(50)),
            "&type=&new_type=cta",
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = create(
        &h,
        &owner,
        &op_body(
            "Old+Roam",
            &eve(-Duration::hours(3)),
            &format!("&type={cta}&new_type="),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let types: i64 =
        sqlx::query_scalar("SELECT count(*) FROM \"plugin_tether.fleet-operations\".op_types")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(types, 1);
    let second: Option<i64> = sqlx::query_scalar("SELECT type_id FROM \"plugin_tether.fleet-operations\".ops WHERE operation_name = 'Second Fleet'")
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(second, Some(cta));

    // Mistakes come back with the form as typed.
    let typo = create(
        &h,
        &owner,
        &op_body("Typo+Op", "tomorrow", &format!("&type={cta}&new_type=")),
    )
    .await;
    assert_eq!(typo.status, StatusCode::OK, "{}", typo.body);
    assert!(
        typo.body.contains("such as 2026-09-30 18:00"),
        "{}",
        typo.body
    );
    assert!(typo.body.contains("value=\"Typo Op\""), "{}", typo.body);
    let far = create(
        &h,
        &owner,
        &op_body(
            "Far+Op",
            &eve(Duration::days(400)),
            &format!("&type={cta}&new_type="),
        ),
    )
    .await;
    // Any start, as AA's form takes.
    assert_eq!(far.status, StatusCode::SEE_OTHER, "{}", far.body);

    // Upcoming: AA's columns, the countdown ticking in the browser, the
    // creator's portrait, Create Operation as the header's button, and
    // Edit and a Delete that asks first in each row. Past on its own tab.
    let list = ops_page(&h, &owner, 0).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    for text in [
        "Home Defence",
        "Second Fleet",
        "Ferox Fleet",
        "1DQ1-A",
        "Example FC",
        "Bring drones",
        "CTA",
        "1d 2h",
        "data-countdown",
        "images.evetech.net/characters/196379789/portrait",
        "The fleet operation Home Defence is deleted for everyone.",
    ] {
        assert!(list.body.contains(text), "{text}: {}", list.body);
    }
    assert!(
        list.body
            .contains(&format!("href=\"/plugins/{ID}/add\">Create Operation</a>")),
        "{}",
        list.body
    );
    assert!(!list.body.contains("Old Roam"), "{}", list.body);
    let old = ops_page(&h, &owner, 1).await;
    assert!(old.body.contains("Old Roam"), "{}", old.body);
    assert!(old.body.contains("ago"), "{}", old.body);

    // Members see operations once granted optimer_view; FCs get
    // optimer_management too.
    let a = log_in_as(&h, PILOT_A, None).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(ops_page(&h, &blue, 0).await.status, StatusCode::NOT_FOUND);
    let widget = format!("/dashboard/widgets/{ID}/0");
    assert!(!page(&h, "/dashboard", &blue).await.body.contains(&widget));
    assert_eq!(page(&h, &widget, &blue).await.status, StatusCode::NOT_FOUND);
    grant(&h, &owner, "optimer_view", BLUE_STATE).await;
    grant(&h, &owner, "optimer_view", MEMBER_STATE).await;
    grant(&h, &owner, "optimer_management", MEMBER_STATE).await;

    // Viewing only: no creating, editing or deleting.
    let seen = ops_page(&h, &blue, 0).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body.contains("Home Defence"));
    assert!(!seen.body.contains("Create Operation"), "{}", seen.body);
    assert!(!seen.body.contains("name=\"op\""), "{}", seen.body);
    let home = op_id(&h, "Home Defence").await;
    for uri in ["add".to_owned(), format!("op/{home}")] {
        assert_eq!(
            page(&h, &format!("/plugins/{ID}/{uri}"), &blue)
                .await
                .status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let refused = create(
        &h,
        &blue,
        &op_body("Sneaky", &eve(Duration::hours(5)), "&new_type="),
    )
    .await;
    assert!(refused.status.is_client_error(), "{}", refused.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}"),
            &format!("_form=delete&op={home}"),
            &blue,
        ),
    )
    .await;
    assert!(res.status.is_client_error(), "{}", res.body);

    // An FC edits an operation, and becomes its character, as AA's
    // eve_character.
    let edit = page(&h, &format!("/plugins/{ID}/op/{home}"), &a).await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    assert!(
        edit.body.contains("value=\"Home Defence\""),
        "{}",
        edit.body
    );
    assert!(
        edit.body
            .contains(&format!("<option value=\"{cta}\" selected>CTA</option>")),
        "{}",
        edit.body
    );
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/op/{home}"),
            &op_body(
                "Home+Defence+(moved)",
                &eve(Duration::hours(30)),
                "&type=&new_type=Stratop",
            ),
            &a,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(op_id(&h, "Home Defence (moved)").await, home);
    let edited = page(&h, &format!("/plugins/{ID}/op/{home}"), &a).await;
    assert!(edited.body.contains("Posted"), "{}", edited.body);
    assert!(
        edited
            .body
            .contains("images.evetech.net/characters/443630591/portrait"),
        "{}",
        edited.body
    );
    assert!(
        !edited
            .body
            .contains("images.evetech.net/characters/196379789/portrait"),
        "{}",
        edited.body
    );
    type_id(&h, "Stratop").await;

    // The Dashboard's Upcoming Fleets: the next five, soonest first, not
    // past ones.
    for hours in 60..64 {
        let res = create(
            &h,
            &a,
            &op_body(
                &format!("Later+{hours}"),
                &eve(Duration::hours(hours)),
                "&type=&new_type=",
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let dashboard = page(&h, "/dashboard", &blue).await;
    assert!(
        dashboard.body.contains(&format!(r#"hx-get="{widget}""#)),
        "{}",
        dashboard.body
    );
    assert!(
        dashboard.body.contains("Upcoming Fleets"),
        "{}",
        dashboard.body
    );
    let fleets = page(&h, &widget, &blue).await;
    assert_eq!(fleets.status, StatusCode::OK, "{}", fleets.body);
    let first = fleets
        .body
        .find("Home Defence (moved)")
        .expect("the next op");
    let second = fleets.body.find("Second Fleet").expect("then this one");
    assert!(first < second, "{}", fleets.body);
    for shown in [
        "Later 60",
        "Later 61",
        "Later 62",
        "Stratop",
        "data-countdown",
    ] {
        assert!(fleets.body.contains(shown), "{shown}: {}", fleets.body);
    }
    assert!(
        !fleets.body.contains("Later 63"),
        "five only: {}",
        fleets.body
    );
    assert!(!fleets.body.contains("Old Roam"), "{}", fleets.body);
    assert!(
        fleets
            .body
            .contains(&format!(r#"href="/plugins/{ID}/widget""#)),
        "{}",
        fleets.body
    );

    // Deleting: from the row on the list, and from the operation's page.
    let second = op_id(&h, "Second Fleet").await;
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}"),
            &format!("_form=delete&op={second}"),
            &a,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/plugins/{ID}/op/{home}"),
            &format!("_form=delete&op={home}"),
            &a,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM \"plugin_tether.fleet-operations\".ops WHERE id IN ($1, $2)",
    )
    .bind(second)
    .bind(home)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/op/{home}"), &a)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

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

/// However long the operations' texts (fields are capped in characters,
/// the host's page limit counts bytes), the list still renders: cut short,
/// and saying so.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_board_of_long_operations_still_renders(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let long = "漢".repeat(254);
    let type_id: i64 = sqlx::query_scalar(
        "INSERT INTO \"plugin_tether.fleet-operations\".op_types (name) VALUES ($1) RETURNING id",
    )
    .bind(&long)
    .fetch_one(&h.db)
    .await
    .unwrap();
    // Well past what fits in 1 MiB at these lengths, upcoming and past.
    sqlx::query(
        "INSERT INTO \"plugin_tether.fleet-operations\".ops (operation_name, doctrine, system, \
         start_time, duration, fc, description, type_id, account_id, character_id, \
         character_name) \
         SELECT left($1 || n, 254), $1, $1, now() + n * interval '1 minute', $2, $1, $3, $4, 1, \
         196379789, 'Chribba' FROM generate_series(-200, 300) AS n",
    )
    .bind(&long)
    .bind("漢".repeat(25))
    .bind("漢".repeat(500))
    .bind(type_id)
    .execute(&h.db)
    .await
    .unwrap();

    for tab in [0, 1] {
        let list = ops_page(&h, &owner, tab).await;
        assert_eq!(list.status, StatusCode::OK, "{}", list.body);
        assert!(list.body.contains("all that fit"), "{}", list.body);
    }
    // The Dashboard's five, as ever.
    let widget = page(&h, &format!("/dashboard/widgets/{ID}/0"), &owner).await;
    assert_eq!(widget.status, StatusCode::OK, "{}", widget.body);
    assert!(widget.body.contains("data-countdown"), "{}", widget.body);

    // Past the 100 types the select holds, an operation's own type is
    // written in by name, so saving it keeps it.
    sqlx::query(
        "INSERT INTO \"plugin_tether.fleet-operations\".op_types (name) \
         SELECT 'A' || n FROM generate_series(1, 100) AS n",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let any: i64 = sqlx::query_scalar("SELECT min(id) FROM \"plugin_tether.fleet-operations\".ops")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let edit = page(&h, &format!("/plugins/{ID}/op/{any}"), &owner).await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    assert!(!edit.body.contains(" selected>"), "{}", edit.body);
    assert!(
        edit.body
            .contains(&format!("name=\"new_type\" value=\"{long}\"")),
        "{}",
        edit.body
    );
}
