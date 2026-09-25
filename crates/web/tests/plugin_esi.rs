#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Plugin ESI, identity and Discord (F16, N8, N10): user scopes only for
//! registered characters on compliant accounts, data sources offered and
//! approved, every call checked and logged, the host choosing the ids, and
//! Discord only to assigned channels, pinging only state roles.

mod common;

use std::sync::OnceLock;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "nmu.esi";
/// Chribba, and his corporation in the affiliation fixture.
const CHRIBBA: i64 = 196379789;
const CHRIBBA_CORP: i64 = 1164409536;
const SKILLS: &str = "esi-skills.read_skills.v1";
const MINING: &str = "esi-industry.read_corporation_mining.v1";

fn probe_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-storage"))
        .clone()
}

async fn install(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"ESI probe\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\ndiscord = [\"send_message\"]\n\n\
         [capabilities.esi]\nuser = [\"{SKILLS}\"]\ndata_source = [\"{MINING}\"]\n\n\
         [permissions]\nview = \"See\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(h, owner, &bytes, &key.sign(&bytes)).await;
}

/// The probe through `submit` (so it may send to Discord).
async fn probe(h: &Harness, path: &str, query: &[(&str, &str)]) -> String {
    let query = query
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    run_probe(h, ID, path, query, false).await
}

async fn esi(h: &Harness, endpoint: &str, who: (&str, i64)) -> String {
    probe(
        h,
        "esi",
        &[("endpoint", endpoint), (who.0, &who.1.to_string())],
    )
    .await
}

/// Goes through the SSO round trip a profile button starts; returns the
/// scopes the login asked for and the new session (logins rotate it).
async fn grant(h: &Harness, token: &str, uri: &str, character: &str) -> (Vec<String>, String) {
    let res = send(&h.app, form(uri, "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let asked = h.sso.last_requested.lock().unwrap().clone();
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, token)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    (asked, res.cookie_value(SESSION))
}

async fn mount_esi(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/skills")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "skills": [], "total_sp": 5000000, "unallocated_sp": 0
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/extractions"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([{
                    "chunk_arrival_time": "2026-09-30T18:05:00Z",
                    "extraction_start_time": "2026-09-24T00:00:00Z",
                    "moon_id": 40161234,
                    "natural_decay_time": "2026-09-30T21:05:00Z",
                    "structure_id": 1030000000001i64
                }])),
        )
        .mount(&h.esi_server)
        .await;
}

async fn access_log(db: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT endpoint, outcome FROM core.plugin_access_log ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

/// Runs queued jobs (re-evaluations) until the queue is empty.
async fn run_jobs(h: &Harness) {
    let mut registry = tether_jobs::Registry::new();
    tether_web::states::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    let config = tether_jobs::WorkerConfig::default();
    while tether_jobs::run_once(&h.db, &registry, &config)
        .await
        .unwrap()
        != tether_jobs::Outcome::Idle
    {}
}

/// Chribba's alliance is Member, Chribba is the owner, and the probe is
/// installed, so Member requires its user scope.
async fn member_with_plugin(db: PgPool) -> (Harness, String) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    run_jobs(&h).await;
    (h, owner)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn user_scopes_need_a_compliant_registered_account(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;

    // Signed out: to the login page, nothing started.
    for uri in ["/register/start", "/profile/plugins/nmu.esi/offer"] {
        let res = send(&h.app, form(uri, "", "no-such-session")).await;
        assert_eq!(res.location(), "/login", "{uri}");
    }
    // A plugin's admin actions need admin.plugins.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/nmu.esi/sources/{CHRIBBA}/approve"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    // Installing the plugin made Member require its scope, which Chribba
    // hasn't granted: still Member (flagged), but no data for the plugin.
    assert_eq!(state_of(&h, &owner).await, "Member");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotRegistered");
    assert_eq!(probe(&h, "characters", &[]).await, "[]");

    // The checklist says what to grant; registering asks EVE for it.
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(checklist.contains("Register Chribba"), "{checklist}");
    assert!(
        checklist.contains("Read skills and attributes"),
        "{checklist}"
    );
    let (asked, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    assert!(asked.contains(&SKILLS.to_owned()), "{asked:?}");
    assert_eq!(state_of(&h, &owner).await, "Member");

    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("5000000"), "{out}");
    let characters = probe(&h, "characters", &[]).await;
    assert!(characters.contains("Chribba"), "{characters}");

    // Not approved for this plugin, or the wrong kind of subject.
    let out = esi(&h, "character-assets", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = esi(&h, "corporation-mining-extractions", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = esi(&h, "no-such-endpoint", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // A Guest's character, even one whose token has the scope.
    sqlx::query("UPDATE core.character_tokens SET scopes = $2 WHERE character_id = $1")
        .bind(443630591_i64)
        .bind(vec![SKILLS])
        .execute(&h.db)
        .await
        .unwrap();
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert_eq!(out, "err Error::NotRegistered");

    // A revoked token flags the account and stops the plugin at once.
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked' WHERE character_id = $1")
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    let account = tether_db::plugin_esi::character_account(&h.db, CHRIBBA)
        .await
        .unwrap()
        .unwrap();
    tether_web::states::evaluate_account(&h.db, account)
        .await
        .unwrap();
    assert_eq!(state_of(&h, &owner).await, "Member");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotRegistered");

    // Every call is in the access log.
    let log = access_log(&h.db).await;
    assert!(
        log.contains(&("character-skills".to_owned(), "ok".to_owned())),
        "{log:?}"
    );
    assert!(log.contains(&("character-skills".to_owned(), "not registered".to_owned())));
    let admin = page(&h, "/admin/plugins/nmu.esi", &owner).await.body;
    assert!(
        admin.contains("Recent data access") && admin.contains(SKILLS),
        "{admin}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plain_login_keeps_the_scopes_a_character_registered(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    let (_, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    // Logging in again asks for nothing, and mustn't replace the richer
    // token.
    let owner = log_in_as(&h, "196379789:Chribba", Some(&owner)).await;
    assert!(h.sso.last_requested.lock().unwrap().is_empty());
    assert_eq!(state_of(&h, &owner).await, "Member");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    // The profile shows what was granted and what uses it.
    let profile = page(&h, "/profile", &owner).await.body;
    assert!(profile.contains("Read skills and attributes"), "{profile}");
    assert!(
        profile.contains("Member requirement, ESI probe"),
        "{profile}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn data_sources_are_offered_then_approved(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;

    let (asked, owner) = grant(
        &h,
        &owner,
        "/profile/plugins/nmu.esi/offer",
        "196379789:Chribba",
    )
    .await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    assert!(
        page(&h, "/profile", &owner)
            .await
            .body
            .contains("waiting for an admin")
    );
    // Offered is not approved.
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    assert_eq!(probe(&h, "sources", &[]).await, "[]");

    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/nmu.esi/sources/{CHRIBBA}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    // The host picks the corporation: the source's own.
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("40161234"), "{out}");
    let sources = probe(&h, "sources", &[]).await;
    assert!(sources.contains("Chribba"), "{sources}");
    // A data source is for corporation endpoints only.
    let out = esi(&h, "character-skills", ("source", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");

    // Withdrawn by its owner: no longer used.
    let res = send(
        &h.app,
        form(
            &format!("/profile/plugins/nmu.esi/offer/{CHRIBBA}/withdraw"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pages_know_who_is_looking(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let res = page(&h, "/plugins/nmu.esi/viewer", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Chribba"), "{}", res.body);
    assert!(res.body.contains(&CHRIBBA_CORP.to_string()), "{}", res.body);
    // Its own permissions, without the prefix.
    assert!(res.body.contains("&#34;view&#34;"), "{}", res.body);
    assert!(!res.body.contains("admin.plugins"), "{}", res.body);
    // No viewer in a job.
    assert_eq!(probe(&h, "viewer", &[]).await, "None");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn discord_messages_go_only_where_an_admin_allows(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;

    // Not set up; then set up but not assigned.
    let out = probe(
        &h,
        "send",
        &[("channel", DISCORD_PING_CHANNEL), ("text", "pop")],
    )
    .await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    discord_ready(&h, &owner).await;
    let out = probe(
        &h,
        "send",
        &[("channel", DISCORD_PING_CHANNEL), ("text", "pop")],
    )
    .await;
    assert!(out.contains("not one of this plugin"), "{out}");

    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.esi/channels",
            &format!("channel_id={DISCORD_PING_CHANNEL}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;

    // Pages can't send, even to an assigned channel.
    let from_page = run_probe(
        &h,
        ID,
        "send",
        vec![("text".to_owned(), "pop".to_owned())],
        true,
    )
    .await;
    assert!(from_page.contains("pages can't send"), "{from_page}");

    let out = probe(
        &h,
        "send",
        &[("text", "Moon popped @everyone"), ("state", "member")],
    )
    .await;
    assert_eq!(out, "ok");
    let sent = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path().ends_with("/messages"))
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    let content = body["content"].as_str().unwrap();
    assert!(
        content.starts_with(&format!("<@&{DISCORD_MEMBER_ROLE}>")),
        "{content}"
    );
    // Typed @everyone is defused, and only the state role may ping.
    assert!(!content.contains("@everyone"), "{content}");
    assert_eq!(
        body["allowed_mentions"]["roles"],
        serde_json::json!([DISCORD_MEMBER_ROLE])
    );

    // A state with no role mapped can't be pinged, nor one that doesn't
    // exist.
    let out = probe(&h, "send", &[("text", "hi"), ("state", "Blue")]).await;
    assert!(out.contains("no Discord role"), "{out}");
    let out = probe(&h, "send", &[("text", "hi"), ("state", "Admirals")]).await;
    assert!(out.contains("no Discord role"), "{out}");
}
