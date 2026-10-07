//! The Contacts app end to end (aa-contacts): installed from its real
//! component and migration; a data source added through Add data source by a holder
//! of AA's manage permissions (the manifest's owner_permissions); its
//! corporation's and alliance's contacts and labels read; seen by those
//! with a character in them (superusers all); notes and server links by
//! their permissions.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::Secret;
use tether_esi::sso::SsoTokens;
use tether_jobs::{Outcome, Registry, WorkerConfig, run_once};
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "tether.contacts";
const CHRIBBA: i64 = 196379789;
const CORP: i64 = 1164409536;
const ALLIANCE: i64 = 159826257;
const HOSTILE: i64 = 99005338;
const FRIEND: i64 = 98000001;

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT.get_or_init(|| build_guest("contacts")).clone()
}

fn plugin_file(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../plugins/contacts/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = plugin_file("plugin.toml").replace("PUBLISHER_KEY", &key.public());
    let migration = plugin_file("migrations/0001_contacts.sql");
    let rotation = plugin_file("migrations/0002_update_rotation.sql");
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
        ("migrations/0001_contacts.sql", migration.as_bytes()),
        ("migrations/0002_update_rotation.sql", rotation.as_bytes()),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

/// Offers Chribba as an owner (the SSO round trip); in use at once.
async fn add_owner(h: &Harness, owner: &str) -> String {
    add_owner_as(h, owner, &format!("{CHRIBBA}:Chribba")).await
}

/// Offers a character (`id:name`) as an owner.
async fn add_owner_as(h: &Harness, owner: &str, who: &str) -> String {
    let res = send(&h.app, form(&format!("/apps/{ID}/owners/add"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(asked.contains(&"esi-alliances.read_contacts.v1".to_owned()));
    assert!(asked.contains(&"esi-corporations.read_contacts.v1".to_owned()));
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                who.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

async fn update(h: &Harness) {
    sqlx::query(
        "UPDATE core.schedules SET next_run_at = now() - interval '1 minute' WHERE name = $1",
    )
    .bind(format!("plugin:{ID}:update"))
    .execute(&h.db)
    .await
    .unwrap();
    tether_jobs::schedule::run_due(&h.db).await.unwrap();
    work(h).await;
}

/// Runs the queued jobs that are due.
async fn work(h: &Harness) {
    let mut registry = Registry::new();
    tether_web::plugin_jobs::register_jobs(&mut registry, h.db.clone(), h.plugins.clone());
    let config = WorkerConfig::default();
    while run_once(&h.db, &registry, &config).await.unwrap() != Outcome::Idle {}
}

/// How often ESI was asked for `at`.
async fn reads(h: &Harness, at: &str) -> usize {
    h.esi_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.path() == at)
        .count()
}

async fn mount(h: &Harness) {
    let json = |v: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(v)
    };
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contacts")))
        .respond_with(json(serde_json::json!([
            { "contact_id": HOSTILE, "contact_type": "alliance", "standing": -10.0, "label_ids": [1] },
            { "contact_id": FRIEND, "contact_type": "corporation", "standing": 5.0 },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contacts/labels")))
        .respond_with(json(
            serde_json::json!([{ "label_id": 1, "label_name": "Reds" }]),
        ))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/alliances/{ALLIANCE}/contacts")))
        .respond_with(json(serde_json::json!([
            { "contact_id": HOSTILE, "contact_type": "alliance", "standing": -5.0 },
        ])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/alliances/{ALLIANCE}/contacts/labels")))
        .respond_with(json(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": HOSTILE, "name": "Pandemic Horde", "category": "alliance" },
            { "id": FRIEND, "name": "Friendly Corp", "category": "corporation" },
            { "id": CORP, "name": "Otherworld Enterprises", "category": "corporation" },
            { "id": ALLIANCE, "name": "Otherworld Empire", "category": "alliance" },
        ])))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
}

/// Chribba and Pilot A as owners, both in CORP and ALLIANCE.
async fn two_owners(h: &Harness, owner: &str) -> String {
    let owner = add_owner(h, owner).await;
    let owner = add_owner_as(h, &owner, "443630591:Pilot A").await;
    sqlx::query(
        "UPDATE core.characters SET corporation_id = $1, alliance_id = $2 WHERE id = 443630591",
    )
    .bind(CORP)
    .bind(ALLIANCE)
    .execute(&h.db)
    .await
    .unwrap();
    // (Approved for the corporation it's in now.)
    sqlx::query(
        "UPDATE core.plugin_data_sources SET corporation_id = $1 WHERE character_id = 443630591",
    )
    .bind(CORP)
    .execute(&h.db)
    .await
    .unwrap();
    let in_use: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_data_sources d \
         JOIN core.characters c ON c.id = d.character_id \
         WHERE d.plugin_id = $1 AND d.approved_at IS NOT NULL AND d.corporation_id = c.corporation_id",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(in_use, 2);
    owner
}

/// Runs the follow-ups updates queued (a minute early), until none is
/// left; how many ran.
async fn follow_ups(h: &Harness) -> usize {
    let mut ran = 0;
    loop {
        let due = sqlx::query(
            "UPDATE core.jobs SET run_at = now() \
             WHERE plugin_id = $1 AND job_key = 'update-more' AND state = 'queued'",
        )
        .bind(ID)
        .execute(&h.db)
        .await
        .unwrap()
        .rows_affected();
        if due == 0 {
            return ran;
        }
        ran += 1;
        assert!(ran < 5, "the follow-ups don't stop");
        work(h).await;
    }
}

async fn post(h: &Harness, token: &str, at: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/plugins/{ID}/{at}"), body, token)).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn contacts_end_to_end(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    // The review names who adds owners: AA's manage permissions.
    let about = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        about.body.contains("manage_alliance_contacts"),
        "{}",
        about.body
    );
    let owner = add_owner(&h, &owner).await;
    update(&h).await;

    // The owner (a superuser, in both) sees both, with their contacts.
    let index = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert_eq!(index.status, StatusCode::OK, "{}", index.body);
    assert!(
        index.body.contains("Otherworld Enterprises"),
        "{}",
        index.body
    );
    assert!(index.body.contains("Otherworld Empire"), "{}", index.body);
    let corp = page(&h, &format!("/plugins/{ID}/corporation/{CORP}"), &owner).await;
    for text in [
        "Pandemic Horde",
        "Friendly Corp",
        "-10.0",
        "+5.0",
        "Reds",
        "Update now",
    ] {
        assert!(corp.body.contains(text), "{text}: {}", corp.body);
    }
    // Notes and a server link on a contact.
    let at = format!("corporation/{CORP}/contact/{HOSTILE}");
    let res = post(&h, &owner, &at, "_form=notes&notes=Shoot+on+sight").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = post(
        &h,
        &owner,
        &at,
        "_form=add_link&name=Comms&url=https%3A%2F%2Fdiscord.gg%2Fx&password=hunter2&color=danger",
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let contact = page(&h, &format!("/plugins/{ID}/{at}"), &owner).await;
    for text in ["Shoot on sight", "Comms", "https://discord.gg/x", "hunter2"] {
        assert!(contact.body.contains(text), "{text}: {}", contact.body);
    }

    // A member of the corporation without the notes and links permissions:
    // the contacts, not the notes, links or the update.
    // (Moved into it after logging in, which reads its affiliation.)
    let member = log_in_as(&h, "443630591:Pilot A", None).await;
    sqlx::query(
        "UPDATE core.characters SET corporation_id = $1, alliance_id = $2 WHERE id = 443630591",
    )
    .bind(CORP)
    .bind(ALLIANCE)
    .execute(&h.db)
    .await
    .unwrap();
    let corp = page(&h, &format!("/plugins/{ID}/corporation/{CORP}"), &member).await;
    assert_eq!(corp.status, StatusCode::OK, "{}", corp.body);
    assert!(corp.body.contains("Pandemic Horde"), "{}", corp.body);
    assert!(!corp.body.contains("Shoot on sight"), "{}", corp.body);
    assert!(!corp.body.contains("Update now"), "{}", corp.body);
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/{at}"), &member)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let res = post(&h, &member, &format!("corporation/{CORP}"), "_form=update").await;
    assert_ne!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // An outsider sees neither.
    let outsider = log_in_as(&h, "406944591:Pilot B", None).await;
    let index = page(&h, &format!("/plugins/{ID}"), &outsider).await;
    assert!(
        !index.body.contains("Otherworld Enterprises"),
        "{}",
        index.body
    );
    assert_eq!(
        page(&h, &format!("/plugins/{ID}/corporation/{CORP}"), &outsider)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

/// Owners in one corporation and alliance: each is read once a run, so a
/// big alliance's run doesn't run out of ESI calls on the same few. Labels
/// whose read fails for a moment stay, and the overview says the update
/// failed. Update now reads the one asked for, as aa-contacts'.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn each_is_read_once_and_labels_outlast_a_failed_read(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    let owner = two_owners(&h, &owner).await;
    update(&h).await;
    let alliance = format!("/alliances/{ALLIANCE}/contacts");
    let corp = format!("/corporations/{CORP}/contacts");
    assert_eq!(reads(&h, &alliance).await, 1);
    assert_eq!(reads(&h, &corp).await, 1);

    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CORP}/contacts/labels")))
        .respond_with(ResponseTemplate::new(502))
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    update(&h).await;
    assert_eq!(reads(&h, &corp).await, 2);
    let listed = page(&h, &format!("/plugins/{ID}/corporation/{CORP}"), &owner).await;
    assert!(listed.body.contains("Reds"), "{}", listed.body);
    let error: Option<String> = sqlx::query_scalar(
        r#"SELECT last_error FROM "plugin_tether.contacts".tracked WHERE kind = 'corporation'"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.starts_with("labels not read")),
        "{error:?}"
    );
    let index = page(&h, &format!("/plugins/{ID}"), &owner).await;
    assert!(index.body.contains("Last update failed"), "{}", index.body);

    // Update now: that corporation, not its alliance.
    sqlx::query(
        r#"UPDATE "plugin_tether.contacts".tracked SET updated_at = now() - interval '1 hour'"#,
    )
    .execute(&h.db)
    .await
    .unwrap();
    let res = post(&h, &owner, &format!("corporation/{CORP}"), "_form=update").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    work(&h).await;
    assert_eq!(reads(&h, &corp).await, 3);
    assert_eq!(reads(&h, &alliance).await, 2);
}

/// The first owner in an alliance and corporation (by name) whose EVE
/// login stopped working doesn't stop them being read: the next owner in
/// them reads them.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_owner_whose_login_stopped_does_not_hold_the_rest_up(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    let owner = two_owners(&h, &owner).await;
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked' WHERE character_id = $1")
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    update(&h).await;
    assert_eq!(
        reads(&h, &format!("/alliances/{ALLIANCE}/contacts")).await,
        1
    );
    assert_eq!(
        reads(&h, &format!("/corporations/{CORP}/contacts")).await,
        1
    );
    let tracked: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        r#"SELECT kind, last_error, updated_at IS NOT NULL
           FROM "plugin_tether.contacts".tracked ORDER BY kind"#,
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        tracked,
        vec![
            ("alliance".to_owned(), None, true),
            ("corporation".to_owned(), None, true),
        ]
    );
    let corp = page(&h, &format!("/plugins/{ID}/corporation/{CORP}"), &owner).await;
    assert!(corp.body.contains("Pandemic Horde"), "{}", corp.body);
}

/// More corporations than one run's ESI calls reach: the hourly run reads
/// what it can, a follow-up the rest, each exactly once, and the follow-ups
/// stop.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_run_s_tail_is_read_by_a_follow_up(db: PgPool) {
    const MORE: i64 = 50;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount(&h).await;
    add_owner(&h, &owner).await;
    // Fifty more owners on Chribba's account, each in a corporation of
    // its own (2,000,001 to 2,000,050), outside any alliance.
    for sql in [
        "INSERT INTO core.characters (id, account_id, name, corporation_id) \
         SELECT 2100000000 + n, account_id, 'Owner ' || n, 2000000 + n \
         FROM core.characters, generate_series(1, $2) n WHERE id = $1",
        "INSERT INTO core.plugin_data_sources \
             (plugin_id, character_id, offered_by, approved_by, approved_at, corporation_id) \
         SELECT plugin_id, 2100000000 + n, offered_by, approved_by, now(), 2000000 + n \
         FROM core.plugin_data_sources, generate_series(1, $2) n WHERE character_id = $1",
    ] {
        sqlx::query(sql)
            .bind(CHRIBBA)
            .bind(MORE as i32)
            .execute(&h.db)
            .await
            .unwrap();
    }
    let scopes: Vec<String> =
        sqlx::query_scalar("SELECT scopes FROM core.character_tokens WHERE character_id = $1")
            .bind(CHRIBBA)
            .fetch_one(&h.db)
            .await
            .unwrap();
    for n in 1..=MORE {
        let id = 2_100_000_000 + n;
        let tokens = SsoTokens {
            access_token: Secret::new(format!("access-{id}-login")),
            refresh_token: Some(Secret::new(format!("refresh-{id}-1"))),
            expires_at: Some(std::time::SystemTime::now() + std::time::Duration::from_secs(1200)),
            owner_hash: None,
        };
        h.vault.store(id, &tokens, &scopes).await.unwrap();
    }
    let json = |v: serde_json::Value| {
        ResponseTemplate::new(200)
            .insert_header("x-pages", "1")
            .set_body_json(v)
    };
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/corporations/20000[0-9]{2}/contacts(/labels)?$",
        ))
        .respond_with(json(serde_json::json!([])))
        .mount(&h.esi_server)
        .await;
    update(&h).await;
    let read_first = sqlx::query_scalar::<_, i64>(
        r#"SELECT count(*) FROM "plugin_tether.contacts".tracked WHERE attempted_at IS NOT NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(
        read_first > 2 && read_first < MORE + 2,
        "one run read {read_first}"
    );
    assert!(follow_ups(&h).await >= 1);
    for corp in (1..=MORE).map(|n| 2_000_000 + n).chain([CORP]) {
        assert_eq!(
            reads(&h, &format!("/corporations/{corp}/contacts")).await,
            1,
            "{corp}"
        );
    }
    assert_eq!(
        reads(&h, &format!("/alliances/{ALLIANCE}/contacts")).await,
        1
    );
    let untried: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "plugin_tether.contacts".tracked
           WHERE attempted_at IS NULL OR last_error IS NOT NULL"#,
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(untried, 0);
}
