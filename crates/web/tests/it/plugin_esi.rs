//! Plugin ESI, identity and Discord (F16, N8, N10): user scopes only for
//! characters registered for the app by holders of its permissions (any
//! state, as Alliance Auth's apps), data sources offered and
//! approved, every call checked and logged, the host choosing the ids,
//! who owns a character told only to the bundled Member Audit, and Discord
//! only to assigned channels, pinging only state roles.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

const ID: &str = "acme.esi";
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
         [permissions]\nview = \"See\"\nmanage = \"Manage\"\nadd_owner = \"Add owners\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
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
/// installed (which requires nothing of Member).
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
async fn user_scopes_need_a_character_registered_for_the_app(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    const REGISTER: &str = "/register/start?app=acme.esi";

    // Signed out: to the login page, nothing started.
    for uri in ["/register/start", REGISTER, "/apps/acme.esi/owners/add"] {
        let res = send(&h.app, form(uri, "", "no-such-session")).await;
        assert_eq!(res.location(), "/login", "{uri}");
    }
    // A plugin's admin actions need admin.plugins.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/acme.esi/sources/{CHRIBBA}/remove"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    // Installing the plugin requires nothing of Member (AA's apps are
    // opt-in): Chribba isn't flagged, but isn't registered for the app
    // either, so it reads nothing of his.
    assert_eq!(state_of(&h, &owner).await, "Member");
    let state_checklist = page(&h, "/register", &owner).await.body;
    assert!(
        !state_checklist.contains("Read skills and attributes"),
        "{state_checklist}"
    );
    // The state's checklist leads to the app's.
    assert!(
        state_checklist.contains("/register?app=acme.esi"),
        "{state_checklist}"
    );
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotRegistered");
    assert_eq!(probe(&h, "characters", &[]).await, "[]");

    // The app's checklist says what to grant; registering asks EVE for it
    // (and for what the state requires, in the same login).
    let checklist = page(&h, "/register?app=acme.esi", &owner).await.body;
    assert!(checklist.contains("Register Chribba"), "{checklist}");
    assert!(checklist.contains("Register for ESI probe"), "{checklist}");
    assert!(
        checklist.contains("Read skills and attributes"),
        "{checklist}"
    );
    assert!(checklist.contains(r#"action="/register/start?app=acme.esi""#));
    let (asked, owner) = grant(&h, &owner, REGISTER, "196379789:Chribba").await;
    assert!(asked.contains(&SKILLS.to_owned()), "{asked:?}");
    assert!(
        asked.contains(&tether_core::scopes::CORP_MEMBERSHIP.to_owned()),
        "{asked:?}"
    );
    assert_eq!(state_of(&h, &owner).await, "Member");

    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("5000000"), "{out}");
    let characters = probe(&h, "characters", &[]).await;
    assert!(characters.contains("Chribba"), "{characters}");
    let checklist = page(&h, "/register?app=acme.esi", &owner).await.body;
    assert!(checklist.contains("Registered"), "{checklist}");
    assert!(
        checklist.contains("/register/unregister?app=acme.esi"),
        "{checklist}"
    );
    // Registering for the state alone registers for no app.
    let registered: i64 = sqlx::query_scalar("SELECT count(*) FROM core.app_characters")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(registered, 1);

    // Not approved for this plugin, or the wrong kind of subject.
    let out = esi(&h, "character-assets", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = esi(&h, "corporation-mining-extractions", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = esi(&h, "no-such-endpoint", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");

    // A Guest holding none of the app's permissions: not served, even with
    // the scope, and may not register for it.
    sqlx::query("UPDATE core.character_tokens SET scopes = $2 WHERE character_id = $1")
        .bind(443630591_i64)
        .bind(vec![SKILLS])
        .execute(&h.db)
        .await
        .unwrap();
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert_eq!(out, "err Error::NotRegistered");
    let res = page(&h, "/register?app=acme.esi", &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let res = send(&h.app, form(REGISTER, "", &pilot)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert!(
        !page(&h, "/register", &pilot)
            .await
            .body
            .contains("acme.esi")
    );

    // Given one of its permissions, whatever the state (AA's): the token
    // carries the scope, but the character isn't registered for the app,
    // so it isn't read until its pilot registers it.
    grant_to_guests(&h, &owner, "view").await;
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert_eq!(out, "err Error::NotRegistered");
    let checklist = page(&h, "/register?app=acme.esi", &pilot).await;
    assert_eq!(checklist.status, StatusCode::OK, "{}", checklist.body);
    assert!(
        checklist.body.contains("Register The Mittani"),
        "{}",
        checklist.body
    );
    let (_, pilot) = grant(&h, &pilot, REGISTER, "443630591:The Mittani").await;
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert!(!out.contains("NotRegistered"), "{out}");
    let characters = probe(&h, "characters", &[]).await;
    assert!(characters.contains("The Mittani"), "{characters}");
    let registered: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'plugin.character_registered' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(registered["character_id"], 443630591);
    assert!(
        page(&h, "/register", &pilot)
            .await
            .body
            .contains("/register?app=acme.esi")
    );
    // Only while it holds one: without it, no longer.
    let grant_id: i64 = sqlx::query_scalar(
        "SELECT id FROM core.permission_grants WHERE permission = 'plugin.acme.esi.view'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    sqlx::query("DELETE FROM core.permission_grants WHERE id = $1")
        .bind(grant_id)
        .execute(&h.db)
        .await
        .unwrap();
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert_eq!(out, "err Error::NotRegistered");
    // Through a group it's in, likewise.
    let group: i64 =
        sqlx::query_scalar("INSERT INTO core.groups (name) VALUES ('Probers') RETURNING id")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO core.group_members (group_id, account_id) \
         SELECT $1, account_id FROM core.characters WHERE id = 443630591",
    )
    .bind(group)
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO core.permission_grants (permission, group_id) VALUES ('plugin.acme.esi.view', $1)",
    )
    .bind(group)
    .execute(&h.db)
    .await
    .unwrap();
    let out = esi(&h, "character-skills", ("character", 443630591)).await;
    assert!(!out.contains("NotRegistered"), "{out}");
    sqlx::query("DELETE FROM core.group_members WHERE group_id = $1")
        .bind(group)
        .execute(&h.db)
        .await
        .unwrap();

    // Unregistering (the app's page): not read any more, audited. Only
    // one's own characters.
    let res = send(
        &h.app,
        form(
            "/register/unregister?app=acme.esi",
            &format!("character_id={CHRIBBA}"),
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/register/unregister?app=acme.esi",
            &format!("character_id={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/register?app=acme.esi");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotRegistered");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'plugin.character_unregistered'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    let (_, owner) = grant(&h, &owner, REGISTER, "196379789:Chribba").await;

    // A revoked token stops the plugin at once.
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
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(
        admin.contains("Recent data access") && admin.contains(SKILLS),
        "{admin}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plain_login_keeps_the_scopes_a_character_registered(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    // Signing in again asks for nothing, and mustn't replace the richer
    // token.
    let _ = owner;
    let owner = log_in_as(&h, "196379789:Chribba", None).await;
    assert!(h.sso.last_requested.lock().unwrap().is_empty());
    assert_eq!(state_of(&h, &owner).await, "Member");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    // Token Management shows what was granted and what uses it (the
    // Dashboard only whether each character is registered).
    let profile = page(&h, "/tokens", &owner).await.body;
    assert!(profile.contains("Read skills and attributes"), "{profile}");
    assert!(profile.contains("ESI probe"), "{profile}");
    assert!(
        !profile.contains("Member requirement, ESI probe"),
        "{profile}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn data_sources_are_added_and_in_use_at_once(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;

    // Add owner is on the app's own page (AA's), with its owners.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains(r#"action="/apps/acme.esi/owners/add""#),
        "{main}"
    );
    assert!(main.contains("No owners yet"), "{main}");
    let (asked, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    // In use at once, as AA's Add Owner: nobody approves it.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(main.contains(">active</span>"), "{main}");
    assert!(!main.contains("/approve"), "{main}");
    assert!(!main.contains("waiting"), "{main}");
    // Not on the Dashboard any more.
    let dashboard = page(&h, "/dashboard", &owner).await.body;
    assert!(!dashboard.contains("corporation data"), "{dashboard}");
    assert!(!dashboard.contains("/apps/acme.esi"), "{dashboard}");
    // Audited as the pilot's, with the corporation it's for.
    let added: String = sqlx::query_scalar(
        "SELECT details::text FROM core.audit_log WHERE action = 'plugin.data_source_added'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(
        added.contains(&format!(r#""corporation_id": {CHRIBBA_CORP}"#)),
        "{added}"
    );
    // The host picks the corporation: the source's own.
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("40161234"), "{out}");
    let sources = probe(&h, "sources", &[]).await;
    assert!(sources.contains("Chribba"), "{sources}");
    // A data source is for corporation endpoints only.
    let out = esi(&h, "character-skills", ("source", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // The Apps admin page lists it too.
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(admin.contains("Data sources"), "{admin}");

    // Token Management always shows what an account's characters are
    // used for, with Withdraw.
    let tokens = page(&h, "/tokens", &owner).await.body;
    assert!(
        tokens.contains("App owners") && tokens.contains("ESI probe"),
        "{tokens}"
    );
    let withdraw = format!("/apps/acme.esi/owners/{CHRIBBA}/withdraw?from=tokens");
    assert!(tokens.contains(&withdraw), "{tokens}");

    // Withdrawn by its owner: no longer used, and the admins see it went.
    let res = send(&h.app, form(&withdraw, "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/tokens");
    assert!(
        !page(&h, "/tokens", &owner)
            .await
            .body
            .contains("App owners")
    );
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(main.contains("Withdrawn by Chribba"), "{main}");
}

async fn grant_to_guests(h: &Harness, owner: &str, permission: &str) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.{ID}.{permission}&grantee=state:{GUEST_STATE}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_add_owner_holders_add_and_only_admins_remove(db: PgPool) {
    const MITTANI: i64 = 443630591;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;

    // May open the app, but not add owners: no button, no owners card,
    // and the route refuses.
    grant_to_guests(&h, &owner, "view").await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await;
    assert_eq!(main.status, StatusCode::OK, "{}", main.body);
    assert!(!main.body.contains("Add owner"), "{}", main.body);
    assert!(!main.body.contains("owners-title"), "{}", main.body);
    let res = send(&h.app, form("/apps/acme.esi/owners/add", "", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Nor may the app's manage permission, as in AA.
    grant_to_guests(&h, &owner, "manage").await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await.body;
    assert!(!main.contains("Add owner"), "{main}");
    let res = send(&h.app, form("/apps/acme.esi/owners/add", "", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // An add_ permission may: its holder adds their own character, in use
    // at once, and sees only their own.
    grant_to_guests(&h, &owner, "add_owner").await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await.body;
    assert!(main.contains("Add owner"), "{main}");
    let (asked, pilot) = grant(
        &h,
        &pilot,
        "/apps/acme.esi/owners/add",
        "443630591:The Mittani",
    )
    .await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    let main = page(&h, "/plugins/acme.esi", &pilot).await.body;
    assert!(main.contains("The Mittani"), "{main}");
    assert!(
        main.contains(&format!("/apps/acme.esi/owners/{MITTANI}/withdraw")),
        "{main}"
    );
    assert!(!main.contains("Chribba"), "{main}");
    assert!(!main.contains("/approve"), "{main}");
    let res = send(
        &h.app,
        form(
            &format!("/apps/acme.esi/owners/{CHRIBBA}/remove"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // Nor can anyone withdraw someone else's character.
    let res = send(
        &h.app,
        form(
            &format!("/apps/acme.esi/owners/{CHRIBBA}/withdraw"),
            "",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // The admin sees both, and may remove the pilot's.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains("The Mittani") && main.contains("Chribba"),
        "{main}"
    );
    assert!(
        main.contains(&format!("/apps/acme.esi/owners/{MITTANI}/remove")),
        "{main}"
    );
    let res = send(
        &h.app,
        form(
            &format!("/apps/acme.esi/owners/{MITTANI}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(main.contains("Removed by Chribba"), "{main}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pages_know_who_is_looking(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let res = page(&h, "/plugins/acme.esi/viewer", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Chribba"), "{}", res.body);
    assert!(res.body.contains(&CHRIBBA_CORP.to_string()), "{}", res.body);
    // Its own permissions, without the prefix.
    assert!(res.body.contains("&#34;view&#34;"), "{}", res.body);
    assert!(!res.body.contains("admin.plugins"), "{}", res.body);
    // No viewer in a job.
    assert_eq!(probe(&h, "viewer", &[]).await, "None");
}

/// The probe as a bundled app under `id`: no `[publisher]`, no signature.
fn bundled_probe(id: &str) -> Vec<u8> {
    let manifest = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Probe {id}\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [capabilities.esi]\nuser = [\"{SKILLS}\"]\n"
    );
    testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &probe_component()),
    ])
}

async fn approve_bundled(h: &Harness, owner: &str, id: &str, package: &[u8]) {
    use sha2::Digest;
    let sha: String = sha2::Sha256::digest(package)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-bundled/{id}/approve"),
            &format!("package={sha}&reviewed=none"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

/// Who owns a character (`identity.owners`) is Member Audit's alone, as
/// bundled with Tether: not a signed app's, nor another bundled one's.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_bundled_member_audit_learns_who_owns_characters(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let member_audit = bundled_probe("tether.member-audit");
    let other = bundled_probe("acme.bundled");
    let h = harness_with_bundled(db, vec![member_audit.clone(), other.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    approve_bundled(&h, &owner, "tether.member-audit", &member_audit).await;
    approve_bundled(&h, &owner, "acme.bundled", &other).await;
    run_jobs(&h).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    assert_eq!(state_of(&h, &owner).await, "Member");
    // Registered for Member Audit only: another app reading the same scope
    // doesn't read him (aa-memberaudit reads only characters added to it).
    for id in [ID, "acme.bundled"] {
        let characters = run_probe(&h, id, "characters", Vec::new(), false).await;
        assert!(!characters.contains("Chribba"), "{id}: {characters}");
    }
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.bundled",
        "196379789:Chribba",
    )
    .await;
    grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    // Once registered, every app may list Chribba...
    for id in [ID, "acme.bundled", "tether.member-audit"] {
        let characters = run_probe(&h, id, "characters", Vec::new(), false).await;
        assert!(characters.contains("Chribba"), "{id}: {characters}");
    }
    // ...but only Member Audit learns whose he is, in a page or a form.
    assert_eq!(probe(&h, "owners", &[]).await, "None");
    assert_eq!(
        run_probe(&h, "acme.bundled", "owners", Vec::new(), true).await,
        "None"
    );
    for as_page in [false, true] {
        let owners = run_probe(&h, "tether.member-audit", "owners", Vec::new(), as_page).await;
        for part in [
            format!("character-id: {CHRIBBA}"),
            format!(
                "main: Character {{ id: {CHRIBBA}, name: \"Chribba\", corporation-id: {CHRIBBA_CORP}"
            ),
            "name: \"Member\"".to_owned(),
        ] {
            assert!(owners.contains(&part), "{part}\n{owners}");
        }
    }
    // Only the characters `esi.characters` lists: not one whose token lacks
    // the app's scope, or was revoked, or whose account holds none of the
    // app's permissions (here: deactivated).
    let only = |sql: &'static str| {
        let db = h.db.clone();
        async move {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
    };
    only("UPDATE core.character_tokens SET scopes = '{}'").await;
    assert_eq!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await,
        "Some([])"
    );
    only("UPDATE core.character_tokens SET scopes = ARRAY['esi-skills.read_skills.v1'], state = 'revoked'").await;
    assert_eq!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await,
        "Some([])"
    );
    only("UPDATE core.character_tokens SET state = 'valid'").await;
    assert!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false)
            .await
            .contains("Chribba")
    );
    // A pilot holding none of the app's permissions (it adds none: only
    // the owner may use it), even with the scope.
    let _pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    sqlx::query("UPDATE core.character_tokens SET scopes = $2 WHERE character_id = $1")
        .bind(443630591_i64)
        .bind(vec![SKILLS])
        .execute(&h.db)
        .await
        .unwrap();
    let owners = run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await;
    assert!(!owners.contains("The Mittani"), "{owners}");
    // Any state, as AA's: the owner holds every app, and the owner's state
    // is told as it is.
    sqlx::query("UPDATE core.accounts SET state_id = $1 WHERE is_owner")
        .bind(GUEST_STATE)
        .execute(&h.db)
        .await
        .unwrap();
    let owners = run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await;
    assert!(
        owners.contains("Chribba") && owners.contains("name: \"Guest\""),
        "{owners}"
    );
}

/// A signed package can take Member Audit's id only where Tether bundles
/// no Member Audit, and then it learns nothing either.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_signed_app_under_member_audits_id_learns_no_owners(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(2);
    let manifest = format!(
        "[plugin]\nid = \"tether.member-audit\"\nname = \"Not Member Audit\"\nversion = \"1.0.0\"\n\
         host_api = \"1\"\n\n[publisher]\nkey = \"{}\"\n\n[capabilities.esi]\nuser = [\"{SKILLS}\"]\n",
        key.public()
    );
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &probe_component()),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    run_jobs(&h).await;
    grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    let characters = run_probe(&h, "tether.member-audit", "characters", Vec::new(), false).await;
    assert!(characters.contains("Chribba"), "{characters}");
    assert_eq!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await,
        "None"
    );
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
            "/admin/plugins/acme.esi/channels",
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

// ---- the character viewer's endpoints, and syncing right away --------------

const MAIL: &str = "esi-mail.read_mail.v1";
const STRUCTURES: &str = "esi-universe.read_structures.v1";
const MITTANI: i64 = 443630591;
const KEEPSTAR: i64 = 1030000000001;

/// A probe that reads members' mail and structures (as Member Audit
/// will), has a data source (as Moon Mining does) and a schedule.
async fn install_viewer(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"ESI probe\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n\
         [capabilities.esi]\nuser = [\"{MAIL}\", \"{STRUCTURES}\"]\ndata_source = [\"{MINING}\"]\n\n\
         [[capabilities.schedules]]\nname = \"sync\"\nevery = \"15m\"\n\n\
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

/// Chribba's alliance is Member, Chribba is the owner, and the viewer
/// probe is installed.
async fn member_with_viewer(db: PgPool) -> (Harness, String) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install_viewer(&h, &owner).await;
    mount_esi(&h).await;
    run_jobs(&h).await;
    (h, owner)
}

async fn esi_with(
    h: &Harness,
    endpoint: &str,
    who: (&str, i64),
    params: &[(&str, &str)],
) -> String {
    let who_id = who.1.to_string();
    let mut query = vec![("endpoint", endpoint), (who.0, who_id.as_str())];
    query.extend_from_slice(params);
    probe(h, "esi", &query).await
}

/// The schedules of plugin jobs queued, oldest first.
async fn queued_plugin_runs(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT schedule FROM core.jobs WHERE kind = 'plugin.job' AND state = 'queued' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// `schedule.run_now` entries: who (`None`, the system) and why.
async fn run_now_audits(db: &PgPool) -> Vec<(Option<i64>, String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT actor_account_id, target, details FROM core.audit_log \
         WHERE action = 'schedule.run_now' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// As if the queued runs had finished, the last queued `ago` (a Postgres
/// interval).
async fn finish_runs(db: &PgPool, ago: &str) {
    sqlx::query(
        "UPDATE core.jobs SET state = 'succeeded', finished_at = now() WHERE kind = 'plugin.job'",
    )
    .execute(db)
    .await
    .unwrap();
    sqlx::query("UPDATE core.schedules SET last_enqueued_at = now() - $1::interval")
        .bind(ago)
        .execute(db)
        .await
        .unwrap();
}

/// Waits for the background sync a login starts: until `n` audited runs
/// exist (at most five seconds), or, for none, a second for it to have
/// (not) queued anything.
async fn await_runs(db: &PgPool, n: usize) {
    if n == 0 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        return;
    }
    for _ in 0..100 {
        if run_now_audits(db).await.len() >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn mount_viewer_esi(h: &Harness) {
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/mail/77")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "body": "Fleet at 19:00", "from": 90000001, "labels": [1], "read": true,
            "recipients": [{"recipient_id": CHRIBBA, "recipient_type": "character"}],
            "subject": "Ops", "timestamp": "2026-09-20T19:04:05Z"
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/universe/structures/{KEEPSTAR}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "Home Keepstar", "owner_id": 98000001, "solar_system_id": 30000142,
            "type_id": 35834, "position": {"x": 1.0, "y": 2.0, "z": 3.0}
        })))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/stations/60003760"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "max_dockable_ship_volume": 50000000.0, "name": "Some Station",
            "office_rental_cost": 10000.0, "owner": 1000035,
            "position": {"x": 1.0, "y": 2.0, "z": 3.0}, "race_id": 1,
            "reprocessing_efficiency": 0.5, "reprocessing_stations_take": 0.05,
            "services": ["market"], "station_id": 60003760, "system_id": 30000142, "type_id": 1531
        })))
        .mount(&h.esi_server)
        .await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_character_viewer_reads_only_characters_registered_for_it(db: PgPool) {
    let (h, owner) = member_with_viewer(db).await;
    mount_viewer_esi(&h).await;
    let mail = |who: (&'static str, i64)| {
        let h = &h;
        async move { esi_with(h, "character-mail-body", who, &[("mail_id", "77")]).await }
    };

    // Chribba hasn't registered for the app yet.
    assert_eq!(
        mail(("character", CHRIBBA)).await,
        "err Error::NotRegistered"
    );
    let (asked, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    assert!(asked.contains(&MAIL.to_owned()), "{asked:?}");
    assert!(asked.contains(&STRUCTURES.to_owned()), "{asked:?}");

    // The one mail asked for, of the character the host names.
    let out = mail(("character", CHRIBBA)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("Fleet at 19:00"), "{out}");
    // A docked structure's name and system, not its owner.
    let out = esi_with(
        &h,
        "universe-structure",
        ("character", CHRIBBA),
        &[("structure_id", &KEEPSTAR.to_string())],
    )
    .await;
    assert!(
        out.contains("Home Keepstar") && out.contains("30000142"),
        "{out}"
    );
    assert!(
        !out.contains("owner_id") && !out.contains("position"),
        "{out}"
    );
    // No mail id, no call.
    let out = esi_with(&h, "character-mail-body", ("character", CHRIBBA), &[]).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");

    // Not a scope this app was approved for, or not a character subject.
    let out = esi(&h, "character-contracts", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = mail(("source", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // A Guest holding none of the app's permissions, even with the scopes.
    let _guest = log_in_as(&h, "443630591:The Mittani", None).await;
    sqlx::query("UPDATE core.character_tokens SET scopes = $2 WHERE character_id = $1")
        .bind(MITTANI)
        .bind(vec![MAIL])
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        mail(("character", MITTANI)).await,
        "err Error::NotRegistered"
    );

    // Public entries need no scope, and any subject will do.
    let out = esi_with(
        &h,
        "universe-station",
        ("character", 0),
        &[("station_id", "60003760")],
    )
    .await;
    assert!(out.contains("Some Station"), "{out}");

    let log = access_log(&h.db).await;
    assert!(log.contains(&("character-mail-body".to_owned(), "ok".to_owned())));
    assert!(log.contains(&(
        "character-mail-body".to_owned(),
        "not registered".to_owned()
    )));
    let _ = owner;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn registering_and_approving_a_source_run_the_apps_schedules_now(db: PgPool) {
    let (h, owner) = member_with_viewer(db).await;
    let schedule = format!("plugin:{ID}:sync");
    assert!(queued_plugin_runs(&h.db).await.is_empty());

    // Registering Chribba runs the app's schedules, audited as the
    // system's doing.
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    await_runs(&h.db, 1).await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );
    let audits = run_now_audits(&h.db).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].0, None);
    assert_eq!(audits[0].1, format!("schedule:{schedule}"));
    assert_eq!(
        audits[0].2,
        serde_json::json!({ "reason": "character_registered", "character_id": CHRIBBA })
    );

    // Logging in again registers nothing new: no run.
    finish_runs(&h.db, "1 hour").await;
    let owner = log_in_as(&h, "196379789:Chribba", Some(&owner)).await;
    await_runs(&h.db, 0).await;
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert_eq!(run_now_audits(&h.db).await.len(), 1);

    // An alt registered five minutes after the last run waits for the
    // next tick: Tether's own runs are ten minutes apart at least.
    finish_runs(&h.db, "5 minutes").await;
    let owner = log_in_as(&h, "443630591:The Mittani", Some(&owner)).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "443630591:The Mittani",
    )
    .await;
    await_runs(&h.db, 0).await;
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert_eq!(run_now_audits(&h.db).await.len(), 1);
    // Eleven minutes after, it would have run: another alt shows it.
    finish_runs(&h.db, "11 minutes").await;
    let owner = log_in_as(&h, "1887431749:gigX", Some(&owner)).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "1887431749:gigX",
    )
    .await;
    await_runs(&h.db, 2).await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );

    // Adding a data source runs them too, as the pilot who added it, a
    // minute after the last run (a person's gap).
    finish_runs(&h.db, "61 seconds").await;
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );
    let admin = me(&h, &owner).await["account_id"].as_i64();
    let audits = run_now_audits(&h.db).await;
    assert_eq!(audits.len(), 3, "{audits:?}");
    assert_eq!(audits[2].0, admin);
    assert_eq!(
        audits[2].2,
        serde_json::json!({ "reason": "data_source_added", "character_id": CHRIBBA })
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sync_that_cannot_be_queued_fails_neither_registering_nor_adding(db: PgPool) {
    let (h, owner) = member_with_viewer(db).await;
    mount_viewer_esi(&h).await;
    // The queue refuses the app's jobs.
    sqlx::query(
        "CREATE FUNCTION core.test_refuse_plugin_jobs() RETURNS trigger LANGUAGE plpgsql \
         AS $$ BEGIN RAISE EXCEPTION 'refused for the test'; END $$",
    )
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_refuse_plugin_jobs BEFORE INSERT ON core.jobs FOR EACH ROW \
         WHEN (NEW.kind = 'plugin.job') EXECUTE FUNCTION core.test_refuse_plugin_jobs()",
    )
    .execute(&h.db)
    .await
    .unwrap();

    // Registered all the same (`grant` checks the login went through).
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    let out = esi_with(
        &h,
        "character-mail-body",
        ("character", CHRIBBA),
        &[("mail_id", "77")],
    )
    .await;
    assert!(out.starts_with("ok"), "{out}");
    assert!(page(&h, "/register", &owner).await.body.contains("Chribba"));

    // Added all the same.
    grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");

    // Nothing queued, and nothing claimed to be: the audit goes with the
    // run it records.
    await_runs(&h.db, 0).await;
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert!(run_now_audits(&h.db).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn waiting_offers_are_dropped_and_owners_stay_with_their_account(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    mount_esi(&h).await;
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");

    // An offer from before owners needed no approval: migration 0047
    // drops it (its offerer may hold no add permission), audited as the
    // system.
    sqlx::query(
        "UPDATE core.plugin_data_sources SET approved_at = NULL, approved_by = NULL, \
         corporation_id = NULL",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    sqlx::raw_sql(include_str!(
        "../../../../migrations/0047_owners_without_approval.sql"
    ))
    .execute(&h.db)
    .await
    .unwrap();
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_data_sources")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let audited: (Option<i64>, String) = sqlx::query_as(
        "SELECT actor_account_id, details::text FROM core.audit_log \
         WHERE action = 'plugin.data_source_removed'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audited.0, None);
    assert!(audited.1.contains(&CHRIBBA.to_string()), "{}", audited.1);
    assert!(
        audited.1.contains("owners are now added, not offered"),
        "{}",
        audited.1
    );
    // The owners card lists it as removed; holders add it again in one
    // login.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(main.contains("Removed by"), "{main}");
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");

    // Added by an account the character isn't on (any more: sold, or
    // moved to another account): the app stops reading through it.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let other = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    sqlx::query("UPDATE core.plugin_data_sources SET offered_by = $1")
        .bind(other)
        .execute(&h.db)
        .await
        .unwrap();
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    let sources = tether_db::plugin_esi::data_sources(&h.db, "acme.esi")
        .await
        .unwrap();
    assert!(sources.iter().all(|s| !s.in_use()), "{sources:?}");
    let _ = owner;
}

/// AA's Member Audit compliance groups: an admin requires an app's scopes
/// of a state in one click (asking first, as for a scope). And the upgrade
/// to apps by permission keeps the app scopes Member required as its own.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_state_requires_an_apps_scopes_in_one_click(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    // Chribba registered for Member (compliant), not for the app.
    let (_, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    run_jobs(&h).await;
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(
        states.contains("Require ESI probe&#39;s scopes for Member")
            || states.contains("Require ESI probe's scopes for Member"),
        "{states}"
    );
    let uri = format!("/admin/states/{MEMBER_STATE}/scopes/app");
    // Chribba hasn't granted it: the change asks first.
    let asked = send(&h.app, form(&uri, "plugin=acme.esi", &owner)).await;
    assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
    assert!(
        asked
            .body
            .contains("every character registered for ESI probe"),
        "{}",
        asked.body
    );
    let applied = send(&h.app, form(&uri, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(applied.location(), "/admin/states", "{}", applied.body);
    run_jobs(&h).await;
    // Member requires the app itself, not its scopes as plain requirements.
    let apps: Vec<(i64, String)> =
        sqlx::query_as("SELECT state_id, plugin_id FROM core.state_apps")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(apps, [(MEMBER_STATE, ID.to_owned())]);
    let scopes: i64 = sqlx::query_scalar("SELECT count(*) FROM core.state_scopes")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(scopes, 0);
    let audit: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'state.app_required' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audit["app"], ID);
    assert_eq!(audit["scopes"], serde_json::json!([SKILLS]));
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(
        states.contains("Every character registered for ESI probe"),
        "{states}"
    );
    let compliant = |h: &Harness| {
        let db = h.db.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT compliant FROM core.accounts a JOIN core.characters c ON c.account_id = a.id \
                 WHERE c.id = $1",
            )
            .bind(CHRIBBA)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    // Flagged (not demoted) until registered for the app; the state's
    // checklist says so, and registering there registers for it too.
    assert!(!compliant(&h).await);
    assert_eq!(state_of(&h, &owner).await, "Member");
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(
        checklist.contains("It also registers each character for ESI probe"),
        "{checklist}"
    );
    assert!(
        checklist.contains(r#"action="/register/start?checklist=1""#),
        "{checklist}"
    );
    // A plain Add Character (the Dashboard) registers for no app.
    let (asked, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    assert!(asked.contains(&SKILLS.to_owned()), "{asked:?}");
    run_jobs(&h).await;
    assert!(!compliant(&h).await);
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?checklist=1",
        "196379789:Chribba",
    )
    .await;
    run_jobs(&h).await;
    assert!(compliant(&h).await);
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    // The scopes without the registration aren't enough (AA: compliance is
    // registration with Member Audit).
    let res = send(
        &h.app,
        form(
            "/register/unregister?app=acme.esi",
            &format!("character_id={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(!compliant(&h).await);
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(
        checklist.contains("Not registered for ESI probe"),
        "{checklist}"
    );
    // Registering for the app itself counts too.
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    run_jobs(&h).await;
    assert!(compliant(&h).await);
    // Stopping requiring it: nothing more asked of Member.
    let stop = format!("/admin/states/{MEMBER_STATE}/scopes/app/remove");
    let stopped = send(&h.app, form(&stop, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(stopped.location(), "/admin/states", "{}", stopped.body);
    let apps: i64 = sqlx::query_scalar("SELECT count(*) FROM core.state_apps")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(apps, 0);
    let again = send(&h.app, form(&stop, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND, "{}", again.body);
    let applied = send(&h.app, form(&uri, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(applied.location(), "/admin/states", "{}", applied.body);
    // Nothing left to add; an unknown app or Guest can't.
    let again = send(&h.app, form(&uri, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    let unknown = send(&h.app, form(&uri, "plugin=acme.none&confirm=1", &owner)).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    let guest = send(
        &h.app,
        form(
            &format!("/admin/states/{GUEST_STATE}/scopes/app"),
            "plugin=acme.esi&confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(guest.status, StatusCode::BAD_REQUEST, "{}", guest.body);
    // Signed out, or without admin.states: no.
    let res = send(&h.app, form(&uri, "plugin=acme.esi", "no-such-session")).await;
    assert_eq!(res.location(), "/login");
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = send(&h.app, form(&uri, "plugin=acme.esi&confirm=1", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);

    // The upgrade's step (migration 0050), again from before it: Member
    // required the app's scopes by itself, and keeps them as its own.
    sqlx::query("DELETE FROM core.state_scopes")
        .execute(&h.db)
        .await
        .unwrap();
    let migration = include_str!("../../../../migrations/0050_app_access_by_permission.sql");
    let (keep, _) = migration
        .split_once("-- Whether an account holds")
        .expect("the function follows the upgrade step");
    sqlx::raw_sql(keep).execute(&h.db).await.unwrap();
    // And registers for each app every character it could read until now
    // (a Member's, with all its scopes); nobody else's.
    let (_, register) = migration
        .split_once("-- Nothing changes on upgrade")
        .expect("the registration step");
    let _pilot = log_in_as(&h, "1887431749:gigX", None).await;
    sqlx::query("UPDATE core.character_tokens SET scopes = $1")
        .bind(vec![SKILLS, tether_core::scopes::CORP_MEMBERSHIP])
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM core.app_characters")
        .execute(&h.db)
        .await
        .unwrap();
    let (_, register) = register.split_once('\n').expect("the comment's first line");
    sqlx::raw_sql(register).execute(&h.db).await.unwrap();
    let registered: Vec<(String, i64)> =
        sqlx::query_as("SELECT plugin_id, character_id FROM core.app_characters")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(registered, [(ID.to_owned(), CHRIBBA)]);
    let audit: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'plugin.characters_registered'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audit["characters"], 1);
    let required: Vec<(i64, String)> =
        sqlx::query_as("SELECT state_id, scope FROM core.state_scopes")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(required, [(MEMBER_STATE, SKILLS.to_owned())]);

    // Then 0052: a state requiring all of an app's scopes requires the app
    // (those scope rows give way), and every character of its accounts
    // meeting the app's scopes is registered, so nobody's compliance
    // changes.
    sqlx::query("DELETE FROM core.state_apps")
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM core.app_characters")
        .execute(&h.db)
        .await
        .unwrap();
    // Blue requires the same scope, added by hand: it stays a plain scope.
    sqlx::query("INSERT INTO core.state_scopes (state_id, scope) VALUES ($1, $2)")
        .bind(BLUE_STATE)
        .bind(SKILLS)
        .execute(&h.db)
        .await
        .unwrap();
    let migration = include_str!("../../../../migrations/0052_state_app_requirements.sql");
    let (_, steps) = migration
        .split_once("-- A state whose app scopes came from the app")
        .expect("the steps after the table");
    let (_, steps) = steps.split_once('\n').expect("the comment's first line");
    sqlx::raw_sql(steps).execute(&h.db).await.unwrap();
    let apps: Vec<(i64, String)> =
        sqlx::query_as("SELECT state_id, plugin_id FROM core.state_apps")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(apps, [(MEMBER_STATE, ID.to_owned())]);
    let scopes: Vec<(i64, String)> =
        sqlx::query_as("SELECT state_id, scope FROM core.state_scopes")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(scopes, [(BLUE_STATE, SKILLS.to_owned())]);
    let registered: Vec<(String, i64)> =
        sqlx::query_as("SELECT plugin_id, character_id FROM core.app_characters")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(registered, [(ID.to_owned(), CHRIBBA)]);
    let account: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(CHRIBBA)
        .fetch_one(&h.db)
        .await
        .unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
    assert!(compliant(&h).await);
}
