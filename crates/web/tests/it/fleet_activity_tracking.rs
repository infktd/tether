//! The Fleet Activity Tracking plugin end to end: installed from its real
//! component and migration; FCs create FAT links with a fleet type and an
//! expiry, members register their characters (once each, only while the
//! link is open), managers add, remove and delete, and statistics per
//! pilot, corporation, alliance and month behind aa-afat's permissions.
//! ESI-tracked fleets through an FC's approved data source, with mocked
//! `/characters/{id}/fleet` and `/fleets/{id}/members`.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Datelike, Utc};
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.fleet-activity-tracking";
const CHRIBBA: i64 = 196379789;
const CHRIBBA_CORP: i64 = 1164409536;
const CHRIBBA_ALLIANCE: i64 = 159826257;
/// Two characters in an NPC corporation, no alliance: one account.
const LINE: i64 = 443630591;
const ALT: i64 = 406944591;
const LINE_CORP: i64 = 1000167;
const GIGX: i64 = 1887431749;
const GIGX_CORP: i64 = 98133756;
const FLEET: i64 = 1_234_567_890_123;
const ROKH: i64 = 24688;
const JITA: i64 = 30000142;

/// SQL naming the plugin's schema (read from core, not input).
macro_rules! sql {
    ($($t:tt)*) => {
        sqlx::AssertSqlSafe(format!($($t)*))
    };
}

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("fleet-activity-tracking"))
        .clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/fleet-activity-tracking/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let first = plugin_file("migrations/0001_fleet_activity_tracking.sql");
    let second = plugin_file("migrations/0002_esi_fleet_tracking.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        (
            "migrations/0001_fleet_activity_tracking.sql",
            first.as_bytes(),
        ),
        ("migrations/0002_esi_fleet_tracking.sql", second.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Public names: corporations, alliances, and gigX (for a manual FAT by
/// id).
async fn mount_names(h: &Harness) {
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": CHRIBBA_ALLIANCE, "name": "Otherworld Empire", "category": "alliance" },
            { "id": CHRIBBA_CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": LINE_CORP, "name": "Science and Trade Institute", "category": "corporation" },
            { "id": GIGX, "name": "gigX", "category": "character" },
            { "id": LINE, "name": "Line Member", "category": "character" },
            { "id": ALT, "name": "Line Alt", "category": "character" },
            { "id": ROKH, "name": "Rokh", "category": "inventory_type" },
            { "id": JITA, "name": "Jita", "category": "solar_system" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

/// Members: Chribba's alliance and Line's corporation; gigX is Blue.
async fn setup(db: PgPool) -> (Harness, String, String) {
    cover(&db, Builtin::Member, EntityKind::Alliance, CHRIBBA_ALLIANCE).await;
    cover(&db, Builtin::Member, EntityKind::Corporation, LINE_CORP).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, &format!("{CHRIBBA}:Chribba")).await;
    install(&h, &owner).await;
    mount_names(&h).await;
    grant(&h, &owner, "basic_access", MEMBER_STATE).await;
    // Line brings an alt.
    let line = log_in_as(&h, &format!("{LINE}:Line Member"), None).await;
    let line = log_in_as(&h, &format!("{ALT}:Line Alt"), Some(&line)).await;
    (h, owner, line)
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

async fn post(h: &Harness, at: &str, body: &str, token: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

async fn open(h: &Harness, at: &str, token: &str) -> Res {
    let uri = if at.is_empty() {
        format!("/plugins/{ID}")
    } else {
        format!("/plugins/{ID}/{at}")
    };
    page(h, &uri, token).await
}

/// Creates a FAT link; returns its hash.
async fn create_link(h: &Harness, token: &str, fleet: &str, fleet_type: &str) -> String {
    let res = post(
        h,
        "links/create",
        &format!(
            "_form=create&fleet={}&fleet_type={fleet_type}&doctrine=Ferox&expiry=60",
            fleet.replace(' ', "+")
        ),
        token,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let hash = res
        .location()
        .strip_prefix(&format!("/plugins/{ID}/links/"))
        .unwrap()
        .to_owned();
    assert_eq!(hash.len(), 32, "{hash}");
    hash
}

/// The plugin's own schema, for looking behind its back.
async fn schema(h: &Harness) -> String {
    sqlx::query_scalar("SELECT schema_name FROM core.plugin_storage WHERE plugin_id = $1")
        .bind(ID)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn fats(h: &Harness, hash: &str) -> Vec<(i64, Option<String>)> {
    let schema = schema(h).await;
    sqlx::query_as(sql!(
        "SELECT f.character_id, f.added_by FROM \"{schema}\".fats f \
         JOIN \"{schema}\".links l ON l.id = f.link_id WHERE l.hash = $1 ORDER BY f.character_id"
    ))
    .bind(hash)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

async fn expire(h: &Harness, hash: &str) {
    let schema = schema(h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET expires_at = now() - interval '1 minute' WHERE hash = $1"
    ))
    .bind(hash)
    .execute(&h.db)
    .await
    .unwrap();
}

fn no_problems(logs: &[String]) {
    assert!(logs.is_empty(), "{logs:?}");
}

async fn plugin_problems(h: &Harness) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level IN ('warn', 'error')",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fat_links_clicks_expiry_and_managing(db: PgPool) {
    let (h, owner, line) = setup(db).await;

    // Managers keep the fleet types.
    let res = post(&h, "fleet-types", "_form=add_type&name=CTA", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, "fleet-types", "_form=add_type&name=cta", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("already a fleet type"), "{}", res.body);

    let hash = create_link(&h, &owner, "Home defense", "CTA").await;
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert_eq!(details.status, StatusCode::OK, "{}", details.body);
    assert!(details.body.contains("Home defense"));
    assert!(details.body.contains("CTA"));
    assert!(
        details
            .body
            .contains(&format!("/plugins/{ID}/links/{hash}/add"))
    );

    // A member opens the link and registers both characters.
    let register = open(&h, &format!("links/{hash}/add"), &line).await;
    assert_eq!(register.status, StatusCode::OK, "{}", register.body);
    assert!(register.body.contains("Line Member"), "{}", register.body);
    assert!(register.body.contains("Line Alt"));
    let res = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{LINE}=on&c_{ALT}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(fats(&h, &hash).await, vec![(ALT, None), (LINE, None)]);
    let done = open(&h, &format!("links/{hash}/add"), &line).await;
    assert!(done.body.contains("All your characters are registered"));

    // Clicking again adds nothing: the form is gone once every character
    // is registered.
    let again = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert_eq!(fats(&h, &hash).await.len(), 2);

    // Chribba registers his main; the FC sees everyone, with corporation
    // and alliance names.
    let res = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{CHRIBBA}=on"),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        details.body.contains("Otherworld Enterprises"),
        "{}",
        details.body
    );
    assert!(details.body.contains("Otherworld Empire"));
    assert!(details.body.contains("Science and Trade Institute"));

    // An expired link takes no more FATs.
    let late = create_link(&h, &owner, "Late fleet", "").await;
    expire(&h, &late).await;
    let closed = open(&h, &format!("links/{late}/add"), &line).await;
    assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
    assert!(
        closed.body.contains("This FAT link is closed"),
        "{}",
        closed.body
    );
    let res = post(
        &h,
        &format!("links/{late}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert!(fats(&h, &late).await.is_empty());
    // A link that doesn't exist, or isn't a hash, is nothing.
    for bad in [
        "links/00000000000000000000000000000000/add",
        "links/nope/add",
    ] {
        assert_eq!(open(&h, bad, &line).await.status, StatusCode::NOT_FOUND);
    }

    // Manual FATs: by name for a character the app knows, by id for any.
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=add_fat&character=line+alt",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=add_fat&character=Line+Alt",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("already has a FAT"), "{}", res.body);
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=add_fat&character=gigX",
        &owner,
    )
    .await;
    assert!(
        res.body.contains("Enter their character ID"),
        "{}",
        res.body
    );
    let res = post(
        &h,
        &format!("links/{late}"),
        &format!("_form=add_fat&character={GIGX}"),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        fats(&h, &late).await,
        vec![
            (ALT, Some("Chribba".to_owned())),
            (GIGX, Some("Chribba".to_owned()))
        ]
    );

    // Reopening lets members register again.
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=reopen&expiry=30",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &format!("links/{late}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(fats(&h, &late).await.len(), 3);

    // Managers remove FATs and delete links.
    let res = post(
        &h,
        &format!("links/{late}"),
        &format!("_form=remove_fat&character_id={GIGX}"),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(fats(&h, &late).await.len(), 2);
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=delete&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/plugins/{ID}/links"));
    assert_eq!(
        open(&h, &format!("links/{late}"), &owner).await.status,
        StatusCode::NOT_FOUND
    );
    let schema = schema(&h).await;
    let left: i64 = sqlx::query_scalar(sql!("SELECT count(*) FROM \"{schema}\".fats"))
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 3, "the deleted link's FATs go with it");

    // Everything managers did is in the logs.
    let logs = open(&h, "logs", &owner).await;
    assert_eq!(logs.status, StatusCode::OK, "{}", logs.body);
    for event in [
        "Create FAT Link",
        "Manual FAT Added",
        "Reopen FAT Link",
        "Delete FAT",
        "Delete FAT Link",
        "Fleet Type Added",
    ] {
        assert!(logs.body.contains(event), "{event}: {}", logs.body);
    }
    // The failed duplicate manual FAT wasn't logged.
    let manual: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM \"{schema}\".logs WHERE event = 'Manual FAT Added'"
    ))
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(manual, 2);

    // The dashboard shows members their FATs and the recent links.
    let dashboard = open(&h, "", &line).await;
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    assert!(dashboard.body.contains("Your most recent FATs"));
    assert!(dashboard.body.contains("Home defense"));
    // ...without links to the FC's page.
    assert!(!dashboard.body.contains(&format!("links/{hash}")));

    no_problems(&plugin_problems(&h).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permissions_follow_aa_afat(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let hash = create_link(&h, &owner, "Chribba's fleet", "").await;

    // Blue have no basic_access: nothing at all.
    let blue = log_in_as(&h, &format!("{GIGX}:gigX"), None).await;
    let register = format!("links/{hash}/add");
    for at in ["", "links", register.as_str(), "stats"] {
        assert_eq!(
            open(&h, at, &blue).await.status,
            StatusCode::NOT_FOUND,
            "{at}"
        );
    }

    // Members (basic_access) register, but can't create links, open the
    // FC's page, the fleet types or the logs.
    assert_eq!(open(&h, "links", &line).await.status, StatusCode::OK);
    assert_eq!(
        open(&h, "links/create", &line).await.status,
        StatusCode::FORBIDDEN
    );
    let res = post(
        &h,
        "links/create",
        "_form=create&fleet=Mine&expiry=60",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(
        open(&h, &format!("links/{hash}"), &line).await.status,
        StatusCode::FORBIDDEN
    );
    for at in ["fleet-types", "logs"] {
        assert_eq!(
            open(&h, at, &line).await.status,
            StatusCode::NOT_FOUND,
            "{at}"
        );
    }

    // FCs (add_fatlink) create links and change their own, not others'.
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;
    let own = create_link(&h, &line, "Line's roam", "").await;
    let res = post(
        &h,
        &format!("links/{own}"),
        "_form=edit&fleet=Line%27s+roam+2&fleet_type=&doctrine=",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let theirs = open(&h, &format!("links/{hash}"), &line).await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.body);
    assert!(theirs.body.contains("Only the FC who created this link"));
    // (The host refuses forms the page doesn't draw for that viewer.)
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=edit&fleet=Mine&fleet_type=&doctrine=",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // An FC adds a missed pilot to their own link, but only managers
    // remove FATs or delete links.
    let res = post(
        &h,
        &format!("links/{own}"),
        "_form=add_fat&character=Line+Alt",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &format!("links/{own}"),
        &format!("_form=remove_fat&character_id={ALT}"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let res = post(
        &h,
        &format!("links/{own}"),
        "_form=delete&confirm=on",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // An FC reopens their own link once; then only a manager can.
    expire(&h, &own).await;
    let res = post(&h, &format!("links/{own}"), "_form=reopen&expiry=5", &line).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &format!("links/{own}"), "_form=close&confirm=on", &line).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &format!("links/{own}"), "_form=reopen&expiry=5", &line).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let res = post(&h, &format!("links/{own}"), "_form=reopen&expiry=5", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    no_problems(&plugin_problems(&h).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn statistics_by_alliance_corporation_pilot_and_month(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let res = post(&h, "fleet-types", "_form=add_type&name=Mining", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for fleet in ["Ops one", "Ops two"] {
        let hash = create_link(&h, &owner, fleet, "Mining").await;
        for (token, body) in [
            (&line, format!("_form=register&c_{LINE}=on&c_{ALT}=on")),
            (&owner, format!("_form=register&c_{CHRIBBA}=on")),
        ] {
            let res = post(&h, &format!("links/{hash}/add"), &body, token).await;
            assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        }
    }
    // One fleet last year.
    let old = create_link(&h, &owner, "Last year", "").await;
    let res = post(
        &h,
        &format!("links/{old}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET created_at = created_at - interval '1 year' WHERE hash = $1"
    ))
    .bind(&old)
    .execute(&h.db)
    .await
    .unwrap();

    let now = Utc::now();
    let year = now.year();
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][now.month0() as usize];

    // Everyone sees their own characters.
    let stats = open(&h, "stats", &line).await;
    assert_eq!(stats.status, StatusCode::OK, "{}", stats.body);
    assert!(stats.body.contains("Line Member"), "{}", stats.body);
    assert!(stats.body.contains("Line Alt"));
    assert!(!stats.body.contains("Chribba</a>"), "{}", stats.body);
    assert!(!stats.body.contains("By alliance"));
    let mine = open(&h, &format!("stats/character/{LINE}"), &line).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert!(
        mine.body.contains(&format!("{month} {year}")),
        "{}",
        mine.body
    );
    assert!(mine.body.contains("Ops two"));
    assert!(!mine.body.contains("Last year"));
    let before = open(&h, &format!("stats/character/{LINE}/{}", year - 1), &line).await;
    assert!(before.body.contains("Last year"), "{}", before.body);
    // Not others' characters, corporations or alliances.
    for at in [
        format!("stats/character/{CHRIBBA}"),
        format!("stats/corporation/{LINE_CORP}"),
        format!("stats/corporation/{CHRIBBA_CORP}"),
        format!("stats/alliance/{CHRIBBA_ALLIANCE}"),
    ] {
        assert_eq!(
            open(&h, &at, &line).await.status,
            StatusCode::FORBIDDEN,
            "{at}"
        );
    }

    // Own corporation statistics: Line's corporation and its pilots.
    grant(&h, &owner, "stats_corporation_own", MEMBER_STATE).await;
    let corp = open(&h, &format!("stats/corporation/{LINE_CORP}"), &line).await;
    assert_eq!(corp.status, StatusCode::OK, "{}", corp.body);
    assert!(
        corp.body.contains("Science and Trade Institute"),
        "{}",
        corp.body
    );
    assert!(corp.body.contains("Line Alt"));
    assert!(corp.body.contains("Mining"));
    assert!(corp.body.contains(&format!("{month} {year}")));
    assert_eq!(
        open(&h, &format!("stats/character/{ALT}"), &line)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        open(&h, &format!("stats/corporation/{CHRIBBA_CORP}"), &line)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    // "Own" is the main's corporation: an alt parked in another
    // corporation doesn't open that corporation's statistics.
    let line = log_in_as(&h, &format!("{GIGX}:gigX"), Some(&line)).await;
    assert_eq!(
        open(&h, &format!("stats/corporation/{GIGX_CORP}"), &line)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        open(&h, &format!("stats/corporation/{LINE_CORP}"), &line)
            .await
            .status,
        StatusCode::OK
    );

    // Other corporations and alliances (leadership; the owner holds it).
    let all = open(&h, &format!("stats/{year}"), &owner).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert!(all.body.contains("By alliance"));
    assert!(all.body.contains("Otherworld Empire"), "{}", all.body);
    let corporations = open(&h, &format!("stats/{year}?_tab=1"), &owner).await;
    assert!(corporations.body.contains("Otherworld Enterprises"));
    assert!(corporations.body.contains("Science and Trade Institute"));
    let alliance = open(&h, &format!("stats/alliance/{CHRIBBA_ALLIANCE}"), &owner).await;
    assert_eq!(alliance.status, StatusCode::OK, "{}", alliance.body);
    assert!(alliance.body.contains("Otherworld Enterprises"));
    assert!(!alliance.body.contains("Science and Trade Institute"));
    let other = open(&h, &format!("stats/corporation/{LINE_CORP}"), &owner).await;
    assert_eq!(other.status, StatusCode::OK);
    assert!(other.body.contains("Line Member"));

    no_problems(&plugin_problems(&h).await);
}

fn registry(h: &Harness) -> Registry {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    registry
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn housekeeping_clears_old_logs(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    create_link(&h, &owner, "Recent", "").await;
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "INSERT INTO \"{schema}\".logs (at, event, actor_id, actor_name, description) \
         VALUES (now() - interval '61 days', 'Create FAT Link', 1, 'Old FC', 'old')"
    ))
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:housekeeping"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    let registry = registry(&h);
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
    let left: Vec<String> = sqlx::query_scalar(sql!("SELECT actor_name FROM \"{schema}\".logs"))
        .fetch_all(&h.db)
        .await
        .unwrap();
    assert_eq!(left, vec!["Chribba".to_owned()]);
}

// ---- ESI-tracked fleets ------------------------------------------------------

async fn work(h: &Harness) {
    let registry = registry(h);
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// Chribba (the owner, our FC) offers himself as the app's data source
/// and approves it; returns the owner's new session.
async fn approve_fc(h: &Harness, owner: &str) -> String {
    let owner = offer_source(h, owner, CHRIBBA, "Chribba").await;
    approve_source(h, &owner, CHRIBBA).await;
    owner
}

async fn approve_source(h: &Harness, owner: &str, character: i64) {
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/sources/{character}/approve"),
            "",
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

/// Offers a character of the session's account as the app's data source
/// (the SSO round trip); returns the session after it.
async fn offer_source(h: &Harness, session: &str, character: i64, name: &str) -> String {
    let res = send(
        &h.app,
        form(&format!("/profile/plugins/{ID}/offer"), "", session),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(
        asked.contains(&"esi-fleets.read_fleet.v1".to_owned()),
        "{asked:?}"
    );
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{character}:{}&state={state}",
                name.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, session)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

/// ESI's view of Chribba's fleet: `boss` runs it.
async fn mount_fleet(h: &Harness, boss: i64) {
    mount_fleet_of(h, CHRIBBA, boss).await;
}

/// ESI's view of `character`'s fleet: `boss` runs it.
async fn mount_fleet_of(h: &Harness, character: i64, boss: i64) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{character}/fleet")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "fleet_id": FLEET, "fleet_boss_id": boss, "role": "fleet_commander",
            "squad_id": -1, "wing_id": -1,
        })))
        .mount(&h.esi_server)
        .await;
}

fn member(character: i64) -> serde_json::Value {
    serde_json::json!({
        "character_id": character, "join_time": "2026-09-26T18:00:00Z",
        "role": "squad_member", "role_name": "Squad Member (Boss)",
        "ship_type_id": ROKH, "solar_system_id": JITA, "squad_id": 1,
        "takes_fleet_warp": true, "wing_id": 1,
    })
}

/// The members: Line and his alt on the first read, gigX joining after.
async fn mount_members(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([member(LINE), member(ALT)])),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            member(LINE),
            member(ALT),
            member(GIGX),
        ])))
        .mount(&h.esi_server)
        .await;
}

/// Creates a link tracking Chribba's fleet.
async fn tracked_link(h: &Harness, owner: &str) -> String {
    tracked_link_by(h, owner, CHRIBBA).await
}

async fn tracked_link_by(h: &Harness, session: &str, character: i64) -> String {
    let res = post(
        h,
        "links/create",
        &format!(
            "_form=create&fleet=Tracked+fleet&fleet_type=&doctrine=&expiry=120&track={character}"
        ),
        session,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.location()
        .strip_prefix(&format!("/plugins/{ID}/links/"))
        .unwrap()
        .to_owned()
}

/// Makes the link's last fleet read two minutes old (resume waits a
/// minute after one).
async fn age_poll(h: &Harness, hash: &str) {
    let schema = schema(h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET esi_polled_at = now() - interval '2 minutes' WHERE hash = $1"
    ))
    .bind(hash)
    .execute(&h.db)
    .await
    .unwrap();
}

async fn queued_polls(h: &Harness) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE plugin_id = $1 AND job_key = 'track_fleets' AND state = 'queued'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// Runs the queued poll now (it waits a minute otherwise).
async fn poll_now(h: &Harness) {
    sqlx::query(
        "UPDATE core.jobs SET run_at = now() WHERE plugin_id = $1 AND job_key = 'track_fleets' AND state = 'queued'",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    work(h).await;
}

async fn tracking(h: &Harness, hash: &str) -> (Option<String>, Option<String>) {
    let schema = schema(h).await;
    sqlx::query_as(sql!(
        "SELECT esi_state, esi_stop_reason FROM \"{schema}\".links WHERE hash = $1"
    ))
    .bind(hash)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

type EsiFat = (i64, Option<i64>, Option<i64>, bool);

async fn esi_fats(h: &Harness, hash: &str) -> Vec<EsiFat> {
    let schema = schema(h).await;
    sqlx::query_as(sql!(
        "SELECT f.character_id, f.ship_type_id, f.system_id, f.esi FROM \"{schema}\".fats f \
         JOIN \"{schema}\".links l ON l.id = f.link_id WHERE l.hash = $1 ORDER BY f.character_id"
    ))
    .bind(hash)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_fleet_tracking_adds_members_and_stops(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = approve_fc(&h, &owner).await;
    mount_fleet(&h, CHRIBBA).await;
    mount_members(&h).await;

    let hash = tracked_link(&h, &owner).await;
    assert_eq!(queued_polls(&h).await, 1);
    // A character tracks one fleet at a time.
    let again = post(
        &h,
        "links/create",
        &format!("_form=create&fleet=Twice&fleet_type=&doctrine=&expiry=60&track={CHRIBBA}"),
        &owner,
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert!(again.body.contains("already tracked"), "{}", again.body);
    work(&h).await;
    let rokh = (Some(ROKH), Some(JITA), true);
    assert_eq!(
        esi_fats(&h, &hash).await,
        vec![(ALT, rokh.0, rokh.1, true), (LINE, rokh.0, rokh.1, true)]
    );
    // Polling again a minute later: gigX joined; nobody twice.
    assert_eq!(queued_polls(&h).await, 1);
    poll_now(&h).await;
    let fats = esi_fats(&h, &hash).await;
    assert_eq!(
        fats.iter().map(|f| f.0).collect::<Vec<_>>(),
        vec![ALT, LINE, GIGX]
    );
    poll_now(&h).await;
    assert_eq!(esi_fats(&h, &hash).await.len(), 3);

    // The FC sees who, in what, where.
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert_eq!(details.status, StatusCode::OK, "{}", details.body);
    assert!(details.body.contains("Rokh"), "{}", details.body);
    assert!(details.body.contains("Jita"));
    assert!(details.body.contains("ESI fleet"));
    assert!(details.body.contains("Tracking"));
    assert!(details.body.contains("Line Alt"));
    // Other FCs see who, not where: ships and systems are intel.
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;
    let other_fc = open(&h, &format!("links/{hash}"), &line).await;
    assert_eq!(other_fc.status, StatusCode::OK, "{}", other_fc.body);
    assert!(other_fc.body.contains("Line Alt"));
    assert!(!other_fc.body.contains("Rokh"), "{}", other_fc.body);

    // Removing a FAT while tracking would be undone by the next read.
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=remove_fat&character_id={GIGX}"),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Stop ESI tracking first"), "{}", res.body);
    assert_eq!(esi_fats(&h, &hash).await.len(), 3);
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=stop_tracking&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("manual".into()))
    );
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=remove_fat&character_id={GIGX}"),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(esi_fats(&h, &hash).await.len(), 2);
    // Resuming waits a minute after the last read.
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("try again shortly"), "{}", res.body);
    age_poll(&h, &hash).await;
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("tracking"));

    // Members see the FAT like any other, and their affiliation fills in
    // when they next use the app: for the last week's FATs only.
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".fats SET created_at = now() - interval '8 days' WHERE character_id = $1"
    ))
    .bind(ALT)
    .execute(&h.db)
    .await
    .unwrap();
    let res = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    // Both of Line's characters are in already: nothing left to tick.
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let other = create_link(&h, &owner, "Next fleet", "").await;
    let res = post(
        &h,
        &format!("links/{other}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for (character, expected) in [(LINE, Some(LINE_CORP)), (ALT, None)] {
        let corporation: Option<i64> = sqlx::query_scalar(sql!(
            "SELECT f.corporation_id FROM \"{schema}\".fats f JOIN \"{schema}\".links l ON l.id = f.link_id WHERE f.character_id = $1 AND l.hash = $2"
        ))
        .bind(character)
        .bind(&hash)
        .fetch_one(&h.db)
        .await
        .unwrap();
        assert_eq!(corporation, expected, "{character}");
    }

    // Closing the link stops tracking, and the job stops queuing itself.
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=close&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("closed".into()))
    );
    poll_now(&h).await;
    assert_eq!(queued_polls(&h).await, 0);

    // Reopened and resumed, it tracks again, until the six-hour cap.
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=reopen&expiry=60",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    age_poll(&h, &hash).await;
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("tracking"));
    assert_eq!(queued_polls(&h).await, 1);
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET esi_started_at = now() - interval '7 hours' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    poll_now(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("cap".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);

    let logs = open(&h, "logs", &owner).await;
    assert!(
        logs.body.contains("ESI Fleet Tracking Stopped"),
        "{}",
        logs.body
    );
    assert!(logs.body.contains("Resume ESI Fleet Tracking"));
    no_problems(&plugin_problems(&h).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_stops_when_not_in_a_fleet(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let owner = approve_fc(&h, &owner).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/fleet")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({ "error": "Character is not in a fleet" })),
        )
        .mount(&h.esi_server)
        .await;
    let hash = tracked_link(&h, &owner).await;
    work(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("fleet_ended".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
    assert!(esi_fats(&h, &hash).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_stops_when_boss_passes_and_resumes(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = approve_fc(&h, &owner).await;
    // Line has boss now.
    mount_fleet(&h, LINE).await;
    mount_members(&h).await;
    let hash = tracked_link(&h, &owner).await;
    work(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("not_boss".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
    assert!(esi_fats(&h, &hash).await.is_empty());
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(details.body.contains("Not boss"), "{}", details.body);
    assert!(
        details.body.contains("isn&#39;t the fleet boss")
            || details.body.contains("isn't the fleet boss"),
        "{}",
        details.body
    );
    let resume = open(&h, &format!("links/{hash}?_tab=1"), &owner).await;
    assert!(
        resume.body.contains("Resume ESI tracking"),
        "{}",
        resume.body
    );
    // Not within a minute of the last read.
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &owner,
    )
    .await;
    assert!(res.body.contains("try again shortly"), "{}", res.body);
    age_poll(&h, &hash).await;
    // A manager can't restart someone else's character's tracking: only
    // its owner is offered Resume.
    grant(&h, &owner, "manage_afat", MEMBER_STATE).await;
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("stopped"));
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=resume&confirm=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await, (Some("tracking".into()), None));
    assert_eq!(queued_polls(&h).await, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_stops_on_403(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let owner = approve_fc(&h, &owner).await;
    mount_fleet(&h, CHRIBBA).await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({ "error": "forbidden" })),
        )
        .mount(&h.esi_server)
        .await;
    let hash = tracked_link(&h, &owner).await;
    work(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("refused".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_your_own_approved_characters_track(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = approve_fc(&h, &owner).await;
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;

    // The FC whose character is approved may pick it.
    let create = open(&h, "links/create", &owner).await;
    assert!(
        create.body.contains("Track Chribba&#39;s fleet")
            || create.body.contains("Track Chribba's fleet"),
        "{}",
        create.body
    );
    // Another FC can't pick it, and is told how to opt in.
    let create = open(&h, "links/create", &line).await;
    assert_eq!(create.status, StatusCode::OK, "{}", create.body);
    assert!(!create.body.contains("Chribba"), "{}", create.body);
    assert!(create.body.contains("Offer a character"), "{}", create.body);
    let res = post(
        &h,
        "links/create",
        &format!("_form=create&fleet=Mine&fleet_type=&doctrine=&expiry=60&track={CHRIBBA}"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(queued_polls(&h).await, 0);

    // A data source that's withdrawn stops the tracking at the next poll.
    mount_fleet(&h, CHRIBBA).await;
    mount_members(&h).await;
    let hash = tracked_link(&h, &owner).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/sources/{CHRIBBA}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("data_source".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_deactivated_fcs_data_source_stops_tracking(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;
    // Line, an FC, offers his main; the owner approves it.
    let line = offer_source(&h, &line, LINE, "Line Member").await;
    approve_source(&h, &owner, LINE).await;
    mount_fleet_of(&h, LINE, LINE).await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([member(LINE), member(ALT)])),
        )
        .mount(&h.esi_server)
        .await;
    let hash = tracked_link_by(&h, &line, LINE).await;
    work(&h).await;
    assert_eq!(esi_fats(&h, &hash).await.len(), 2);

    // Deactivated: his character is no longer a data source anywhere.
    let account: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(LINE)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = send(
        &h.app,
        form(&format!("/admin/users/{account}/deactivate"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let sources = tether_db::plugin_esi::data_sources(&h.db, ID)
        .await
        .unwrap();
    assert!(sources.iter().all(|s| !s.in_use()), "{sources:?}");
    poll_now(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("data_source".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
    let admin = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        admin.body.contains("account deactivated or blacklisted"),
        "{}",
        admin.body
    );
}

// ---- Secure Groups: the FAT filter ---------------------------------------------

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fats_feed_secure_groups(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let hash = create_link(&h, &owner, "Roam", "").await;
    let res = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{LINE}=on&c_{ALT}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let line_account = me(&h, &line).await["account_id"].as_i64().unwrap();

    // At least 2 FATs in 30 days, added up across an account's characters.
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Active pilots","internal":false,"hidden":false}"#,
        ),
    )
    .await;
    let group = serde_json::from_str::<serde_json::Value>(&group.body).unwrap()["id"]
        .as_i64()
        .unwrap();
    send(
        &h.app,
        form(
            &format!("/admin/groups/{group}/smart"),
            "smart=on&auto_join=on&grace_days=0",
            &owner,
        ),
    )
    .await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/groups/{group}/smart/filters"),
            &format!("kind=app&app={ID}/fats&f_days=30&at_least=2"),
            &owner,
        ),
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{group}"),
        "{}",
        res.body
    );
    let in_group = |account: i64| {
        let db = h.db.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM core.group_members WHERE group_id = $1 AND account_id = $2)",
            )
            .bind(group)
            .bind(account)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };

    // Nothing reported yet: the group can't be judged, so it's left alone.
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(!in_group(line_account).await);

    // The app's hourly job reports each character's count.
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:report_filters"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(&h).await;
    no_problems(&plugin_problems(&h).await);
    let values: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT character_id, value FROM core.plugin_filter_values ORDER BY character_id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(values, vec![(ALT, 1), (LINE, 1)]);

    // One FAT each: 2 across the account, so in.
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap();
    assert!(in_group(line_account).await);
    let owner_account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    assert!(!in_group(owner_account).await, "no FATs, not in");
    let listed = page(&h, "/groups", &line).await.body;
    assert!(
        listed.contains("Fleet Activity Tracking: FATs in the last days (Days: 30): at least 2"),
        "{listed}"
    );
}
