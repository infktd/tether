//! The Fleet Activity Tracking plugin end to end: installed from its real
//! component and migration; FCs create FAT links with a fleet type and an
//! expiry, members register their characters (once each, only while the
//! link is open), managers add, remove and delete, and statistics per
//! pilot, corporation, alliance and month behind aa-afat's permissions.
//! ESI-tracked fleets through the fleet boss an FC adds from Create FAT
//! Link (AA style: no approval), with mocked `/characters/{id}/fleet` and
//! `/fleets/{id}/members`. aa-afat's rules: registering needs the character
//! online (mocked `/online`, `/location` and `/ship`, with the app's
//! location scopes), any FC changes any link, a link reopens once within
//! the grace time, manual FATs within 24 hours, and aa-afat's settings.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use chrono::{Datelike, Utc};
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
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
    // Every migration the app ships, in order.
    let mut migrations: Vec<String> = std::fs::read_dir(format!(
        "{}/../../plugins/fleet-activity-tracking/migrations",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .map(|e| format!("migrations/{}", e.unwrap().file_name().to_string_lossy()))
    .collect();
    migrations.sort();
    let contents: Vec<String> = migrations.iter().map(|m| plugin_file(m)).collect();
    let component = component();
    let mut files: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ];
    for (name, sql) in migrations.iter().zip(&contents) {
        files.push((name, sql.as_bytes()));
    }
    let bytes = testing::zip(&files);
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
    // Everyone so far registered for the app (its location scopes), and
    // online in a Rokh in Jita.
    grant_location_scopes(&h).await;
    mount_online(&h, true, 10).await;
    (h, owner, line)
}

/// aa-afat's add_fat scopes, the app's user scopes.
const LOCATION_SCOPES: [&str; 3] = [
    "esi-location.read_location.v1",
    "esi-location.read_ship_type.v1",
    "esi-location.read_online.v1",
];

/// Every character logged in so far registers for the app.
async fn grant_location_scopes(h: &Harness) {
    sqlx::query("UPDATE core.character_tokens SET scopes = scopes || $1::text[]")
        .bind(LOCATION_SCOPES.map(str::to_owned).to_vec())
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO core.app_characters (plugin_id, character_id) \
         SELECT $1, character_id FROM core.character_tokens ON CONFLICT DO NOTHING",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
}

/// Every character's `/online`, `/location` and `/ship`: online (or not),
/// in Jita, in a Rokh. A lower `priority` wins over earlier mounts.
async fn mount_online(h: &Harness, online: bool, priority: u8) {
    for (route, body) in [
        (
            r"^/characters/\d+/online/?$",
            serde_json::json!({ "online": online }),
        ),
        (
            r"^/characters/\d+/location/?$",
            serde_json::json!({ "solar_system_id": JITA }),
        ),
        (
            r"^/characters/\d+/ship/?$",
            serde_json::json!({ "ship_type_id": ROKH, "ship_item_id": 1, "ship_name": "Rokh" }),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path_regex(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .with_priority(priority)
            .mount(&h.esi_server)
            .await;
    }
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
    // Each type's row has Disable (or Enable) and Delete.
    let cta: i64 = sqlx::query_scalar(sql!(
        "SELECT id FROM \"{}\".fleet_types WHERE name = 'CTA'",
        schema(&h).await
    ))
    .fetch_one(&h.db)
    .await
    .unwrap();
    for (what, status) in [("disable", "Disabled"), ("enable", "Enabled")] {
        let res = post(
            &h,
            "fleet-types",
            &format!("_form=change_type&type={cta}&action={what}"),
            &owner,
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{what}: {}", res.body);
        let types = open(&h, "fleet-types", &owner).await;
        assert!(types.body.contains(status), "{status}: {}", types.body);
    }

    let hash = create_link(&h, &owner, "Home defense", "CTA").await;
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert_eq!(details.status, StatusCode::OK, "{}", details.body);
    assert!(details.body.contains("Home defense"));
    assert!(details.body.contains("CTA"));
    // The views and Manage Tether draws from the manifest (Manage opens
    // Settings, whose bar is the Manage pages), New FAT link as the
    // header's button, and the link's own buttons: Close, and Delete
    // asking first.
    for href in ["links", "stats", "settings"] {
        assert!(
            details
                .body
                .contains(&format!("href=\"/plugins/{ID}/{href}\"")),
            "{href}: {}",
            details.body
        );
    }
    let settings = open(&h, "settings", &owner).await;
    for href in ["fleet-types", "logs"] {
        assert!(
            settings
                .body
                .contains(&format!("href=\"/plugins/{ID}/{href}\"")),
            "{href}: {}",
            settings.body
        );
    }
    assert!(
        details.body.contains(&format!(
            "href=\"/plugins/{ID}/links/create\">New FAT link</a>"
        )),
        "{}",
        details.body
    );
    assert!(
        details
            .body
            .contains("Nobody can register once it&#39;s closed."),
        "{}",
        details.body
    );
    assert!(
        details
            .body
            .contains("&#34;Home defense&#34; and its 0 FATs are deleted"),
        "{}",
        details.body
    );
    assert!(
        details
            .body
            .contains(&format!("/plugins/{ID}/links/{hash}/add"))
    );
    // aa-afat's copy link: the register page's full address, while open.
    assert!(
        details.body.contains(&format!(
            "value=\"{SITE}/plugins/{ID}/links/{hash}/add\" readonly"
        )),
        "{}",
        details.body
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
    // Portraits and logos, and each row's Remove for a manager.
    for image in [
        format!("characters/{LINE}/portrait"),
        format!("corporations/{CHRIBBA_CORP}/logo"),
        format!("alliances/{CHRIBBA_ALLIANCE}/logo"),
    ] {
        assert!(details.body.contains(&image), "{image}: {}", details.body);
    }
    assert!(
        details
            .body
            .contains("Line Alt&#39;s FAT for this fleet is removed"),
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

    // Reopening (within the grace time) lets members register again, for
    // the reopen duration; after it, no more manual FATs (aa-afat).
    let res = post(&h, &format!("links/{late}"), "_form=reopen", &owner).await;
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
    // The FAT records where the pilot was, and in what.
    let details = open(&h, &format!("links/{late}"), &owner).await;
    assert!(details.body.contains("Rokh"), "{}", details.body);
    assert!(details.body.contains("Jita"), "{}", details.body);
    assert!(
        details
            .body
            .contains("FATs can be added by hand only within 24 hours"),
        "{}",
        details.body
    );
    let res = post(
        &h,
        &format!("links/{late}"),
        "_form=add_fat&character=Line+Member",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);

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
    let res = post(&h, &format!("links/{late}"), "_form=delete", &owner).await;
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
    for at in ["fleet-types", "settings", "logs"] {
        assert_eq!(
            open(&h, at, &line).await.status,
            StatusCode::NOT_FOUND,
            "{at}"
        );
    }

    // FCs (add_fatlink) create links and change any link, as aa-afat.
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
    assert!(!theirs.body.contains("Only the FC who created this link"));
    let res = post(
        &h,
        &format!("links/{hash}"),
        "_form=edit&fleet=Mine&fleet_type=&doctrine=",
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // (The host refuses forms the page doesn't draw for that viewer.)
    // An FC adds a missed pilot, but only managers remove FATs or delete
    // links.
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
    let res = post(&h, &format!("links/{own}"), "_form=delete", &line).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // A link reopens once, by any FC; then not even a manager can.
    expire(&h, &hash).await;
    let res = post(&h, &format!("links/{hash}"), "_form=reopen", &line).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(&h, &format!("links/{hash}"), "_form=close", &line).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for token in [&line, &owner] {
        let res = post(&h, &format!("links/{hash}"), "_form=reopen", token).await;
        assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    }
    let closed = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        closed.body.contains("reopened once already"),
        "{}",
        closed.body
    );

    no_problems(&plugin_problems(&h).await);
}

/// As aa-afat, manage_afat opens every corporation's, alliance's and
/// pilot's statistics, as stats_corporation_other does.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn managers_see_every_corporations_statistics(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let alliance = format!("stats/alliance/{CHRIBBA_ALLIANCE}");
    let corporation = format!("stats/corporation/{CHRIBBA_CORP}");
    let pilot = format!("stats/character/{CHRIBBA}");
    for at in [&alliance, &corporation, &pilot] {
        assert_eq!(
            open(&h, at, &line).await.status,
            StatusCode::FORBIDDEN,
            "{at}"
        );
    }
    let stats = open(&h, "stats", &line).await;
    assert!(!stats.body.contains("Alliances"), "{}", stats.body);

    grant(&h, &owner, "manage_afat", MEMBER_STATE).await;
    for at in [&alliance, &corporation, &pilot] {
        let res = open(&h, at, &line).await;
        assert_eq!(res.status, StatusCode::OK, "{at}: {}", res.body);
    }
    let stats = open(&h, "stats", &line).await;
    assert!(stats.body.contains("Alliances"), "{}", stats.body);
    assert!(stats.body.contains("Corporations"), "{}", stats.body);
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
    // The years as chips under the header, this one marked.
    let now = chrono::Utc::now().format("%Y").to_string();
    let last: i32 = now.parse::<i32>().unwrap() - 1;
    assert!(
        stats.body.contains(&format!(
            r#"href="/plugins/{ID}/stats" aria-current="page">{now}</a>"#
        )),
        "{}",
        stats.body
    );
    assert!(
        stats
            .body
            .contains(&format!(r#"href="/plugins/{ID}/stats/{last}">{last}</a>"#)),
        "{}",
        stats.body
    );
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

/// A big corporation's year: more pilot-months than one query may return.
/// Every pilot's months are counted whole, the busiest 300 are shown, and
/// the table says so.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_big_corporations_statistics_are_exact(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let schema = schema(&h).await;
    let year = Utc::now().year() - 1;
    // A fleet each month of last year.
    sqlx::query(sql!(
        "INSERT INTO \"{schema}\".links (hash, fleet, creator_account, creator_id, creator_name, \
             created_at, expires_at) \
         SELECT 'big' || m, 'Op ' || m, 1, 1, 'FC', make_timestamptz($1, m, 15, 12, 0, 0, 'UTC'), \
             make_timestamptz($1, m, 15, 13, 0, 0, 'UTC') \
         FROM generate_series(1, 12) m"
    ))
    .bind(year)
    .execute(&h.db)
    .await
    .unwrap();
    // 5,000 pilots in January's; Ace in every one.
    sqlx::query(sql!(
        "INSERT INTO \"{schema}\".fats (link_id, character_id, character_name, corporation_id) \
         SELECT l.id, 2100000000 + n, 'Pilot ' || lpad(n::text, 5, '0'), $1 \
         FROM \"{schema}\".links l, generate_series(1, 5000) n WHERE l.hash = 'big1'"
    ))
    .bind(CHRIBBA_CORP)
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(sql!(
        "INSERT INTO \"{schema}\".fats (link_id, character_id, character_name, corporation_id) \
         SELECT id, 2099999999, 'Ace', $1 FROM \"{schema}\".links WHERE hash LIKE 'big%'"
    ))
    .bind(CHRIBBA_CORP)
    .execute(&h.db)
    .await
    .unwrap();
    let stats = open(
        &h,
        &format!("stats/corporation/{CHRIBBA_CORP}/{year}"),
        &owner,
    )
    .await;
    assert_eq!(stats.status, StatusCode::OK, "{}", stats.body);
    assert!(
        stats.body.contains("By pilot: the busiest 300 of 5001"),
        "{}",
        stats.body
    );
    // Ace first with all twelve, then the rest by name, each whole.
    let ace = stats.body.find(">Ace</a>").expect("Ace is listed");
    let first = stats
        .body
        .find(">Pilot 00001</a>")
        .expect("the first pilot");
    assert!(ace < first);
    // The table's last rows (Tether shows 25 at a time; it's the page's
    // second table).
    let last = open(
        &h,
        &format!("stats/corporation/{CHRIBBA_CORP}/{year}?_p1=12"),
        &owner,
    )
    .await;
    assert!(last.body.contains(">Pilot 00299</a>"), "{}", last.body);
    assert!(!last.body.contains(">Pilot 00300</a>"), "{}", last.body);
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

/// Chribba (the owner, our FC) logs in with himself as the fleet boss;
/// returns the owner's new session.
async fn add_fc(h: &Harness, owner: &str) -> String {
    offer_source(h, owner, CHRIBBA, "Chribba").await
}

/// Adds a character of the session's account as the app's data source
/// from New FAT link (the SSO round trip); returns the session after it.
async fn offer_source(h: &Harness, session: &str, character: i64, name: &str) -> String {
    let res = send(
        &h.app,
        form(
            &format!("/apps/{ID}/owners/add"),
            "back=links/create",
            session,
        ),
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
    let owner = add_fc(&h, &owner).await;
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
    // Every FC sees the same, as in aa-afat.
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;
    let other_fc = open(&h, &format!("links/{hash}"), &line).await;
    assert_eq!(other_fc.status, StatusCode::OK, "{}", other_fc.body);
    assert!(other_fc.body.contains("Line Alt"));
    assert!(other_fc.body.contains("Rokh"), "{}", other_fc.body);

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
    let res = post(&h, &format!("links/{hash}"), "_form=stop_tracking", &owner).await;
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
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("try again shortly"), "{}", res.body);
    age_poll(&h, &hash).await;
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
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
    // Both have their corporation: ESI's affiliation, read with the fleet,
    // for a pilot the app had never seen too.
    for (character, expected) in [(LINE, Some(LINE_CORP)), (ALT, Some(1000167))] {
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
    let res = post(&h, &format!("links/{hash}"), "_form=close", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("closed".into()))
    );
    poll_now(&h).await;
    assert_eq!(queued_polls(&h).await, 0);

    // Reopened and resumed, it tracks again, until the six-hour cap.
    let res = post(&h, &format!("links/{hash}"), "_form=reopen", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    age_poll(&h, &hash).await;
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
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
    let owner = add_fc(&h, &owner).await;
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/fleet")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({ "error": "Character is not in a fleet" })),
        )
        .mount(&h.esi_server)
        .await;
    let hash = tracked_link(&h, &owner).await;
    // As aa-afat, three reads in a row are ridden out: the fourth stops it.
    work(&h).await;
    for _ in 0..2 {
        poll_now(&h).await;
    }
    assert_eq!(tracking(&h, &hash).await, (Some("tracking".into()), None));
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        details
            .body
            .contains("The last 3 reads of the fleet failed"),
        "{}",
        details.body
    );
    assert!(
        details.body.contains("1 more time in a row"),
        "{}",
        details.body
    );
    poll_now(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("fleet_ended".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
    assert!(esi_fats(&h, &hash).await.is_empty());
}

async fn errors(h: &Harness, hash: &str) -> (Option<String>, i32) {
    let schema = schema(h).await;
    sqlx::query_as(sql!(
        "SELECT esi_error, esi_errors FROM \"{schema}\".links WHERE hash = $1"
    ))
    .bind(hash)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// aa-afat's rule exactly: only the same error, each within 75 seconds of
/// the last, after three in a row, stops tracking. Another error, a later
/// one, or a good read in between starts the count again.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_rides_out_passing_errors(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let owner = add_fc(&h, &owner).await;
    mount_fleet(&h, CHRIBBA).await;
    // ESI refuses the members twice, then answers.
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({ "error": "forbidden" })),
        )
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    mount_members(&h).await;
    let hash = tracked_link(&h, &owner).await;
    work(&h).await;
    assert_eq!(errors(&h, &hash).await, (Some("refused".into()), 1));
    poll_now(&h).await;
    assert_eq!(errors(&h, &hash).await, (Some("refused".into()), 2));
    // A good read clears them.
    poll_now(&h).await;
    assert_eq!(errors(&h, &hash).await, (None, 0));
    assert_eq!(esi_fats(&h, &hash).await.len(), 2);

    // Not in a fleet: three times, but the third over 75 seconds after the
    // second, so it counts from one again.
    mount_not_in_fleet(&h).await;
    poll_now(&h).await;
    poll_now(&h).await;
    assert_eq!(errors(&h, &hash).await, (Some("fleet_ended".into()), 2));
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET esi_error_at = now() - interval '76 seconds' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    poll_now(&h).await;
    assert_eq!(errors(&h, &hash).await, (Some("fleet_ended".into()), 1));
    // Another error starts it again too.
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET esi_error = 'not_boss' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    poll_now(&h).await;
    assert_eq!(errors(&h, &hash).await, (Some("fleet_ended".into()), 1));
    poll_now(&h).await;
    poll_now(&h).await;
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("tracking"));
    poll_now(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("fleet_ended".into()))
    );
    // Resuming starts with no errors.
    age_poll(&h, &hash).await;
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(errors(&h, &hash).await, (None, 0));
}

async fn expires_at(h: &Harness, hash: &str) -> Option<chrono::DateTime<Utc>> {
    let schema = schema(h).await;
    sqlx::query_scalar(sql!(
        "SELECT expires_at FROM \"{schema}\".links WHERE hash = $1"
    ))
    .bind(hash)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

/// ESI's answer for Chribba's fleet from now on: he isn't in one.
async fn mount_not_in_fleet(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/fleet")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({ "error": "Character is not in a fleet" })),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

/// aa-afat's ESI FAT links have no expiry: open while the fleet is
/// tracked, however long past a clickable link's expiry, and closed when
/// tracking stops.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_esi_tracked_link_has_no_expiry(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = add_fc(&h, &owner).await;
    mount_fleet(&h, CHRIBBA).await;
    mount_members(&h).await;
    let hash = tracked_link(&h, &owner).await;
    assert_eq!(expires_at(&h, &hash).await, None);
    work(&h).await;
    assert_eq!(esi_fats(&h, &hash).await.len(), 2);

    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        details.body.contains("When the fleet ends"),
        "{}",
        details.body
    );
    assert!(details.body.contains("Open"), "{}", details.body);
    let overview = open(&h, "", &owner).await;
    assert!(overview.body.contains("Tracked fleet"), "{}", overview.body);

    // Five hours in (any expiry long gone, within the six-hour cap), it
    // still tracks, and members may still register.
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET created_at = now() - interval '5 hours', \
         esi_started_at = now() - interval '5 hours' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    poll_now(&h).await;
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("tracking"));
    assert_eq!(esi_fats(&h, &hash).await.len(), 3);
    let register = open(&h, &format!("links/{hash}/add"), &line).await;
    assert!(
        register.body.contains("When the fleet ends"),
        "{}",
        register.body
    );

    // Past the six-hour cap it's closed, even before the job stops it.
    for (started, open_now) in [("7 hours", false), ("5 hours", true)] {
        sqlx::query(sql!(
            "UPDATE \"{schema}\".links SET esi_started_at = now() - $2::interval WHERE hash = $1"
        ))
        .bind(&hash)
        .bind(started)
        .execute(&h.db)
        .await
        .unwrap();
        let register = open(&h, &format!("links/{hash}/add"), &line).await;
        assert_eq!(
            register.body.contains("This FAT link is closed"),
            !open_now,
            "{}",
            register.body
        );
    }

    // The fleet ends: tracking stops and the link closes then.
    mount_not_in_fleet(&h).await;
    for _ in 0..4 {
        poll_now(&h).await;
    }
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("fleet_ended".into()))
    );
    let closed = expires_at(&h, &hash).await.unwrap();
    assert!(closed <= Utc::now() && closed > Utc::now() - chrono::Duration::minutes(1));
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(details.body.contains("Closed"), "{}", details.body);
    let res = post(
        &h,
        &format!("links/{hash}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert!(res.status != StatusCode::SEE_OTHER, "{}", res.body);

    // Its FC resumes it (a new fleet, say): open again, without expiry.
    age_poll(&h, &hash).await;
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await, (Some("tracking".into()), None));
    assert_eq!(expires_at(&h, &hash).await, None);

    // Closed by hand, it resumes only once reopened.
    let res = post(&h, &format!("links/{hash}"), "_form=close", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("closed".into()))
    );
    age_poll(&h, &hash).await;
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        !details.body.contains(">Resume tracking</button>"),
        "{}",
        details.body
    );
    assert!(
        details.body.contains(">Reopen</button>"),
        "{}",
        details.body
    );
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("stopped"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_stops_when_boss_passes_and_resumes(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = add_fc(&h, &owner).await;
    // Line has boss now.
    mount_fleet(&h, LINE).await;
    mount_members(&h).await;
    let hash = tracked_link(&h, &owner).await;
    work(&h).await;
    for _ in 0..3 {
        poll_now(&h).await;
    }
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
    // Its owner gets Resume tracking among the link's buttons.
    let resume = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        resume.body.contains(">Resume tracking</button>"),
        "{}",
        resume.body
    );
    // Not within a minute of the last read.
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert!(res.body.contains("try again shortly"), "{}", res.body);
    age_poll(&h, &hash).await;
    // A manager can't restart someone else's character's tracking: only
    // its owner is offered Resume.
    grant(&h, &owner, "manage_afat", MEMBER_STATE).await;
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &line).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("stopped"));
    let res = post(&h, &format!("links/{hash}"), "_form=resume", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(tracking(&h, &hash).await, (Some("tracking".into()), None));
    assert_eq!(queued_polls(&h).await, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn esi_tracking_stops_on_403(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let owner = add_fc(&h, &owner).await;
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
    for _ in 0..2 {
        poll_now(&h).await;
    }
    assert_eq!(tracking(&h, &hash).await.0.as_deref(), Some("tracking"));
    poll_now(&h).await;
    assert_eq!(
        tracking(&h, &hash).await,
        (Some("stopped".into()), Some("refused".into()))
    );
    assert_eq!(queued_polls(&h).await, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_your_own_characters_track(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let owner = add_fc(&h, &owner).await;
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;

    // The FC who added his character may pick it.
    let create = open(&h, "links/create", &owner).await;
    assert!(
        create.body.contains("Track Chribba&#39;s fleet")
            || create.body.contains("Track Chribba's fleet"),
        "{}",
        create.body
    );
    // Another FC can't pick it, and can log in with his own fleet boss.
    let create = open(&h, "links/create", &line).await;
    assert_eq!(create.status, StatusCode::OK, "{}", create.body);
    assert!(!create.body.contains("Chribba"), "{}", create.body);
    assert!(
        create.body.contains("Log in with the fleet boss"),
        "{}",
        create.body
    );
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
        form(&format!("/apps/{ID}/owners/{CHRIBBA}/remove"), "", &owner),
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
    // Line, an FC, logs in with his main as the fleet boss.
    let line = offer_source(&h, &line, LINE, "Line Member").await;
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
    // Its Data sources page says why, and the app's pages say so to
    // those who look after its sources.
    let listed = page(&h, &format!("/plugins/{ID}/data-sources"), &owner).await;
    assert!(
        listed.body.contains(">Not used</span>")
            && listed
                .body
                .contains("Its account is deactivated or blacklisted"),
        "{}",
        listed.body
    );
    let main = open(&h, "", &owner).await;
    assert!(
        main.body
            .contains("A data source isn&#39;t working: Line Member")
            || main
                .body
                .contains("A data source isn't working: Line Member"),
        "{}",
        main.body
    );
    assert!(
        main.body
            .contains(&format!(r#"href="/plugins/{ID}/data-sources""#)),
        "{}",
        main.body
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
            "smart=on&auto_join=on",
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

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_fc_logs_in_with_the_fleet_boss_from_create_fat_link(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    // Members without add_fatlink get no login button (and no page).
    let gigx = log_in_as(&h, &format!("{GIGX}:gigX"), None).await;
    assert_ne!(open(&h, "links/create", &gigx).await.status, StatusCode::OK);
    let res = send(
        &h.app,
        form(
            &format!("/apps/{ID}/owners/add"),
            "back=links/create",
            &line,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // aa-afat: an FC with add_fatlink logs in with the fleet boss right
    // there, and nobody approves it.
    grant(&h, &owner, "add_fatlink", MEMBER_STATE).await;
    let create = open(&h, "links/create", &line).await;
    assert_eq!(create.status, StatusCode::OK, "{}", create.body);
    assert!(
        create.body.contains(&format!(
            r#"<form method="post" action="/apps/{ID}/owners/add" hx-boost="false" class="flex flex-wrap items-center gap-2"><input type="hidden" name="back" value="links/create"><button type="submit" class="btn" data-variant="outline" data-size="sm">Log in with the fleet boss</button><span class="text-xs text-muted-foreground">Adds one of your characters as this app's data source, used at once · EVE asks for:</span><span class="badge num" data-variant="outline">esi-fleets.read_fleet.v1</span></form>"#
        )),
        "{}",
        create.body
    );
    assert!(!create.body.contains("name=\"track\""), "{}", create.body);
    // The header's Add data source comes back here too.
    assert_eq!(
        create
            .body
            .matches(r#"name="back" value="links/create""#)
            .count(),
        1,
        "{}",
        create.body
    );
    // A fleet boss's fleet isn't a corporation's data: its Data sources
    // page says so.
    let listed = open(&h, "data-sources", &owner).await;
    assert!(
        listed
            .body
            .contains("Fleet Activity Tracking reads ESI through"),
        "{}",
        listed.body
    );
    assert!(
        !listed.body.contains("corporation&#39;s data") && !listed.body.contains("in-game role"),
        "{}",
        listed.body
    );
    // Only one of the app's own pages to come back to.
    for bad in ["https://evil.example", "/admin", "../x", "a?b=c"] {
        let res = send(
            &h.app,
            form(
                &format!("/apps/{ID}/owners/add"),
                &format!("back={}", bad.replace('?', "%3F")),
                &line,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{bad}: {}", res.body);
    }

    let res = send(
        &h.app,
        form(
            &format!("/apps/{ID}/owners/add"),
            "back=links/create",
            &line,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{LINE}:Line%20Member&state={state}"),
            &[(LOGIN, &login), (SESSION, &line)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Back on New FAT link, with the fleet boss chosen.
    assert_eq!(
        res.location(),
        format!("/plugins/{ID}/links/create?owner={LINE}")
    );
    let line = res.cookie_value(SESSION);
    let back = page(&h, res.location(), &line).await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.body);
    assert!(
        back.body
            .contains(&format!(r#"<option value="{LINE}" selected>"#)),
        "{}",
        back.body
    );
    assert!(back.body.contains("can be tracked"), "{}", back.body);
    // In use at once, audited as the FC's.
    let sources = tether_db::plugin_esi::data_sources(&h.db, ID)
        .await
        .unwrap();
    assert!(
        sources.iter().any(|s| s.character.id == LINE && s.in_use()),
        "{sources:?}"
    );
    let added: Option<i64> = sqlx::query_scalar(
        "SELECT actor_account_id FROM core.audit_log WHERE action = 'plugin.data_source_added' \
         AND target = $1",
    )
    .bind(format!("plugin:{ID}"))
    .fetch_one(&h.db)
    .await
    .unwrap();
    let line_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
            .bind(LINE)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(added, Some(line_account));

    // Create: tracking starts.
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

    // Its Data sources page: working, no approval anywhere.
    let listed = open(&h, "data-sources", &owner).await.body;
    assert!(listed.contains(">Working</span>"), "{listed}");
    assert!(!listed.contains("/approve"), "{listed}");
    assert!(!listed.contains("waiting for an admin"), "{listed}");
}

const PUBLISHER: &str = "acme.doctrines";

/// The storage probe as an app sharing doctrines, as Fittings does.
async fn install_doctrine_publisher(h: &Harness, owner: &str) {
    let key = Key::new(4);
    let manifest = format!(
        "[plugin]\nid = \"{PUBLISHER}\"\nname = \"Doctrine Book\"\nversion = \"1.0.0\"\n\
         host_api = \"1\"\n\n[publisher]\nkey = \"{}\"\n\n[capabilities]\ndoctrines = \"publish\"\n\n\
         [permissions]\nview = \"See\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n\
         [[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = build_guest("tether-plugins-test-guest-storage");
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(h, owner, &bytes, &key.sign(&bytes)).await;
}

/// aa-afat's Setting and its rules: the reopen grace time and duration,
/// manual FATs within 24 hours, the log duration, all from the settings.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn aa_afat_settings_and_rules(db: PgPool) {
    let (h, owner, line) = setup(db).await;
    let schema = schema(&h).await;

    // The defaults, as aa-afat's, and New FAT link offers the expiry.
    let settings = open(&h, "settings", &owner).await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(settings.body.contains("Default FAT link reopen grace time"));
    let res = post(
        &h,
        "settings",
        "_form=settings&expiry_minutes=45&reopen_grace_minutes=30&reopen_duration_minutes=15&log_days=90",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let create = open(&h, "links/create", &owner).await;
    assert!(
        create.body.contains("name=\"expiry\" value=\"45\""),
        "{}",
        create.body
    );
    // aa-afat's use_doctrines_from_fittings_module, off by default: the
    // doctrine is typed in.
    let select = "<select class=\"select\" id=\"doctrine\"";
    assert!(!create.body.contains(select));
    assert!(create.body.contains("name=\"doctrine\""), "{}", create.body);
    let res = post(
        &h,
        "settings",
        "_form=settings&expiry_minutes=45&reopen_grace_minutes=30&reopen_duration_minutes=15\
         &log_days=90&use_doctrines_from_fittings=on",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let on: bool = sqlx::query_scalar(sql!(
        "SELECT use_doctrines_from_fittings FROM \"{schema}\".settings"
    ))
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(on);
    // On, while no app shares a doctrine (Fittings not installed): still
    // typed in, as aa-afat's without Fittings, and Settings says why and
    // what to do.
    let create = open(&h, "links/create", &owner).await;
    assert!(!create.body.contains(select), "{}", create.body);
    assert!(
        create
            .body
            .contains("Fittings shares no doctrines with you: type it in."),
        "{}",
        create.body
    );
    let settings = open(&h, "settings", &owner).await;
    assert!(
        settings
            .body
            .contains("Fittings shares no doctrines with you now, so FCs type the doctrine in"),
        "{}",
        settings.body
    );
    let res = post(
        &h,
        "links/create",
        "_form=create&fleet=Roam&fleet_type=&doctrine=Ferox&expiry=60",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // Shared (here by a stand-in for Fittings): one of those it shares.
    install_doctrine_publisher(&h, &owner).await;
    let shared = serde_json::json!([{ "key": "1", "name": "Frigate Gang", "link": "doctrine/1" }]);
    let out = run_probe(
        &h,
        PUBLISHER,
        "doctrines-publish",
        vec![
            ("list".to_owned(), shared.to_string()),
            ("see_all".to_owned(), "view".to_owned()),
        ],
        false,
    )
    .await;
    assert_eq!(out, "ok");
    let create = open(&h, "links/create", &owner).await;
    assert!(
        create
            .body
            .contains("The doctrines Fittings shares with you"),
        "{}",
        create.body
    );
    assert!(
        create.body.contains(r#"<option value="Frigate Gang">"#),
        "{}",
        create.body
    );
    let settings = open(&h, "settings", &owner).await;
    assert!(
        !settings.body.contains("Fittings shares no doctrines"),
        "{}",
        settings.body
    );
    // A doctrine that isn't offered is refused (by the host, and FAT).
    let res = post(
        &h,
        "links/create",
        "_form=create&fleet=Roam&fleet_type=&doctrine=Ferox&expiry=60",
        &owner,
    )
    .await;
    assert!(
        res.body.contains("Doctrine: choose one of the options"),
        "{}",
        res.body
    );
    // Unticked (browsers leave it out): typed in again.
    let res = post(
        &h,
        "settings",
        "_form=settings&expiry_minutes=45&reopen_grace_minutes=30&reopen_duration_minutes=15&log_days=90",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // Closed longer ago than the grace time: no reopening.
    let hash = create_link(&h, &owner, "Old fleet", "").await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET expires_at = now() - interval '31 minutes' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(
        details
            .body
            .contains("reopened only within 30 minutes of closing"),
        "{}",
        details.body
    );
    let res = post(&h, &format!("links/{hash}"), "_form=reopen", &owner).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    // Within it: reopened for the reopen duration.
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET expires_at = now() - interval '29 minutes' WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    let res = post(&h, &format!("links/{hash}"), "_form=reopen", &owner).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let minutes: f64 = sqlx::query_scalar(sql!(
        "SELECT extract(epoch FROM expires_at - now())::float8 / 60 FROM \"{schema}\".links WHERE hash = $1"
    ))
    .bind(&hash)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!((14.0..=15.0).contains(&minutes), "{minutes}");

    // Manual FATs only within 24 hours of the link's creation.
    let day_old = create_link(&h, &owner, "Yesterday", "").await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET created_at = now() - interval '25 hours' WHERE hash = $1"
    ))
    .bind(&day_old)
    .execute(&h.db)
    .await
    .unwrap();
    let res = post(
        &h,
        &format!("links/{day_old}"),
        "_form=add_fat&character=Line+Member",
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert!(fats(&h, &day_old).await.is_empty());

    // A pilot who isn't online can't register; nor can a character not
    // registered for the app (not offered, and refused if posted).
    let open_link = create_link(&h, &owner, "Now", "").await;
    mount_online(&h, false, 9).await;
    let res = post(
        &h,
        &format!("links/{open_link}/add"),
        &format!("_form=register&c_{LINE}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("Line Member isn&#39;t online in EVE"),
        "{}",
        res.body
    );
    assert!(fats(&h, &open_link).await.is_empty());
    mount_online(&h, true, 8).await;
    let line = log_in_as(&h, &format!("{GIGX}:gigX"), Some(&line)).await;
    // (Logging in asks for what Member requires; gigX declined it.)
    sqlx::query("UPDATE core.character_tokens SET scopes = '{}' WHERE character_id = $1")
        .bind(GIGX)
        .execute(&h.db)
        .await
        .unwrap();
    let res = post(
        &h,
        &format!("links/{open_link}/add"),
        &format!("_form=register&c_{LINE}=on&c_{GIGX}=on"),
        &line,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body
            .contains("gigX isn&#39;t registered for Fleet Activity Tracking"),
        "{}",
        res.body
    );
    // Tether's Register Character card, to register it.
    assert!(res.body.contains("href=\"/register"), "{}", res.body);
    assert_eq!(fats(&h, &open_link).await, vec![(LINE, None)]);

    // Logs are kept for the settings' days: 61 days old stays at 90.
    sqlx::query(sql!(
        "INSERT INTO \"{schema}\".logs (at, event, actor_id, actor_name, description) \
         VALUES (now() - interval '61 days', 'Create FAT Link', 1, 'Old FC', 'old'), \
                (now() - interval '91 days', 'Create FAT Link', 1, 'Older FC', 'older')"
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
    work(&h).await;
    let old: Vec<String> = sqlx::query_scalar(sql!(
        "SELECT actor_name FROM \"{schema}\".logs WHERE at < now() - interval '1 day'"
    ))
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(old, vec!["Old FC".to_owned()]);
    let logs = open(&h, "logs", &owner).await;
    assert!(logs.body.contains("Kept for 90 days"), "{}", logs.body);
    assert!(logs.body.contains("Settings Changed"), "{}", logs.body);
    no_problems(&plugin_problems(&h).await);
}

// ---- the fleet snapshot ------------------------------------------------------

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A line of EVE's fleet window, copied.
fn fleet_line(name: &str, ship: &str) -> String {
    format!("{name}\tJita\t{ship}\tBattleship\tSquad Member\t0 - 0 - 5\tWing 1 / Squad 1\n")
}

/// aa-afat's fleet snapshot: an FC pastes the fleet composition and every
/// pilot EVE knows gets a FAT with ship and system, under the manual FAT
/// rules, logged.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_fleet_snapshot_adds_fats(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    Mock::given(method("POST"))
        .and(path("/universe/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "characters": [
                { "id": LINE, "name": "Line Member" },
                { "id": ALT, "name": "Line Alt" },
                { "id": GIGX, "name": "gigX" },
            ],
            "systems": [{ "id": JITA, "name": "Jita" }],
            "inventory_types": [{ "id": ROKH, "name": "Rokh" }],
        })))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    let hash = create_link(&h, &owner, "Snapshot fleet", "").await;
    // The link's page: its own, Edit, Add FAT, Fleet snapshot.
    let details = open(&h, &format!("links/{hash}?_tab=3"), &owner).await;
    assert!(
        details.body.contains("Add fleet snapshot"),
        "{}",
        details.body
    );

    // Something else pasted is refused, saying where.
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=snapshot&composition={}", encode("Hello fleet")),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Line 1 "), "{}", res.body);
    assert!(fats(&h, &hash).await.is_empty());

    let paste = format!(
        "{}{}{}",
        fleet_line("Line Member", "Rokh"),
        fleet_line("line alt", "Rokh"),
        fleet_line("Nobody Here", "Rokh"),
    );
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=snapshot&composition={}", encode(&paste)),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("Fleet snapshot: 2 FATs added."),
        "{}",
        res.body
    );
    assert!(res.body.contains("Nobody Here"), "{}", res.body);
    assert_eq!(
        fats(&h, &hash).await,
        vec![
            (ALT, Some("Chribba".to_owned())),
            (LINE, Some("Chribba".to_owned()))
        ]
    );
    let rokh = (Some(ROKH), Some(JITA), false);
    assert_eq!(
        esi_fats(&h, &hash).await,
        vec![(ALT, rokh.0, rokh.1, false), (LINE, rokh.0, rokh.1, false)]
    );
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(details.body.contains("Rokh"), "{}", details.body);
    assert!(details.body.contains("Line Alt"), "{}", details.body);

    // Again with gigX: only he's new.
    let paste = format!("{paste}{}", fleet_line("gigX", "Rokh"));
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=snapshot&composition={}", encode(&paste)),
        &owner,
    )
    .await;
    assert!(
        res.body
            .contains("Fleet snapshot: 1 FAT added. 2 already had one."),
        "{}",
        res.body
    );
    assert_eq!(fats(&h, &hash).await.len(), 3);
    let logs = open(&h, "logs", &owner).await;
    assert!(logs.body.contains("Fleet Snapshot"), "{}", logs.body);
    assert!(
        logs.body.contains("Fleet snapshot added 2 FATs"),
        "{}",
        logs.body
    );

    // Not once the link's been reopened (aa-afat's manual FAT rule).
    let schema = schema(&h).await;
    sqlx::query(sql!(
        "UPDATE \"{schema}\".links SET reopened = 1 WHERE hash = $1"
    ))
    .bind(&hash)
    .execute(&h.db)
    .await
    .unwrap();
    let details = open(&h, &format!("links/{hash}"), &owner).await;
    assert!(!details.body.contains("Fleet snapshot"), "{}", details.body);
    let res = post(
        &h,
        &format!("links/{hash}"),
        &format!("_form=snapshot&composition={}", encode(&paste)),
        &owner,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    no_problems(&plugin_problems(&h).await);
}
