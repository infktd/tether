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
         [permissions]\nview = \"See\"\nmanage = \"Manage\"\nadd_owner = \"Add owners\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
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
            &format!("/apps/acme.esi/owners/{CHRIBBA}/remove"),
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
    // And on the app's Activity page, under Manage.
    let activity = page(&h, "/plugins/acme.esi/activity", &owner).await.body;
    assert!(
        activity.contains("ESI calls")
            && activity.contains("character-skills")
            && activity.contains(">not registered</span>"),
        "{activity}"
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

    // Add data source is on the app's Data sources page (AA's Add Owner),
    // under its Manage, with its data sources.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains(r#"href="/plugins/acme.esi/data-sources""#),
        "{main}"
    );
    assert!(!main.contains("/apps/acme.esi/owners/add"), "{main}");
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(
        listed.contains(r#"action="/apps/acme.esi/owners/add""#),
        "{listed}"
    );
    assert!(listed.contains("No data sources yet"), "{listed}");
    let (asked, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    // In use at once, as AA's Add Owner: nobody approves it.
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(listed.contains(">Not read yet</span>"), "{listed}");
    assert!(!listed.contains("/approve"), "{listed}");
    assert!(!listed.contains("waiting"), "{listed}");
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
    // Read through: working, and what it read.
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(listed.contains(">Working</span>"), "{listed}");
    assert!(listed.contains("Read mining extractions"), "{listed}");
    // A data source is for corporation endpoints only.
    let out = esi(&h, "character-skills", ("source", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // The Apps admin page points to it.
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(
        admin.contains(r#"href="/plugins/acme.esi/data-sources""#),
        "{admin}"
    );

    // Token Management always shows what an account's characters are
    // used for, with Withdraw.
    let tokens = page(&h, "/tokens", &owner).await.body;
    assert!(
        tokens.contains("App data sources") && tokens.contains("ESI probe"),
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
            .contains("App data sources")
    );
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(listed.contains("Withdrawn by Chribba"), "{listed}");
}

/// A character Tether has never seen, added as an owner: its corporation
/// is learnt before the source is written, so the first add is in use at
/// once. One whose corporation EVE won't give isn't added at all.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_new_character_added_as_owner_is_in_use_at_once(db: PgPool) {
    const GIGX: i64 = 1887431749;
    const GIGX_CORP: i64 = 98133756;
    const NOBODY: i64 = 2112000001;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    Mock::given(method("GET"))
        .and(path(format!("/corporation/{GIGX_CORP}/mining/extractions")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-pages", "1")
                .set_body_json(serde_json::json!([{
                    "chunk_arrival_time": "2026-09-30T18:05:00Z",
                    "extraction_start_time": "2026-09-24T00:00:00Z",
                    "moon_id": 40165678,
                    "natural_decay_time": "2026-09-30T21:05:00Z",
                    "structure_id": 1030000000002i64
                }])),
        )
        .mount(&h.esi_server)
        .await;

    // gigX comes to Tether through Add data source, once.
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "1887431749:gigX").await;
    let corporation: Option<i64> = sqlx::query_scalar(
        "SELECT corporation_id FROM core.plugin_data_sources WHERE plugin_id = $1 \
         AND character_id = $2",
    )
    .bind(ID)
    .bind(GIGX)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(corporation, Some(GIGX_CORP));
    let out = esi(&h, "corporation-mining-extractions", ("source", GIGX)).await;
    assert!(out.starts_with("ok pages=1"), "{out}");
    assert!(out.contains("40165678"), "{out}");
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(
        listed.contains("gigX") && listed.contains(">Working</span>"),
        "{listed}"
    );

    // Not in the affiliation fixture: EVE names no corporation, so nothing
    // is added (and nothing is in use for no corporation).
    let res = send(&h.app, form("/apps/acme.esi/owners/add", "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!("/auth/callback?code=ok:{NOBODY}:Nobody&state={state}"),
            &[(LOGIN, &login), (SESSION, &owner)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE, "{}", res.body);
    assert!(res.body.contains("Add it again"), "{}", res.body);
    let sources: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.plugin_data_sources WHERE character_id = $1")
            .bind(NOBODY)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(sources, 0);
}

async fn grant_to_guests(h: &Harness, owner: &str, permission: &str) {
    let res = send(
        &h.app,
        form(
            "/admin/permissions/set",
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

    // May open the app, but not add owners: no Data sources or Activity
    // under Manage, neither page, and the route refuses.
    grant_to_guests(&h, &owner, "view").await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await;
    assert_eq!(main.status, StatusCode::OK, "{}", main.body);
    assert!(!main.body.contains("/data-sources"), "{}", main.body);
    assert!(!main.body.contains("/activity"), "{}", main.body);
    for host in ["data-sources", "activity"] {
        let res = page(&h, &format!("/plugins/acme.esi/{host}"), &pilot).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{host}: {}", res.body);
    }
    let res = send(&h.app, form("/apps/acme.esi/owners/add", "", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Nor may the app's manage permission, as in AA.
    grant_to_guests(&h, &owner, "manage").await;
    let res = page(&h, "/plugins/acme.esi/data-sources", &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let res = send(&h.app, form("/apps/acme.esi/owners/add", "", &pilot)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // An add_ permission may: its holder adds their own character, in use
    // at once, and sees only their own (and not Activity, the admins').
    grant_to_guests(&h, &owner, "add_owner").await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await.body;
    assert!(
        main.contains(r#"href="/plugins/acme.esi/data-sources""#),
        "{main}"
    );
    assert!(!main.contains("/activity"), "{main}");
    let res = page(&h, "/plugins/acme.esi/activity", &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let listed = page(&h, "/plugins/acme.esi/data-sources", &pilot)
        .await
        .body;
    assert!(listed.contains("Add data source"), "{listed}");
    assert!(!listed.contains("/plugins/acme.esi/activity"), "{listed}");
    assert!(listed.contains("t added a character yet"), "{listed}");
    let (asked, pilot) = grant(
        &h,
        &pilot,
        "/apps/acme.esi/owners/add",
        "443630591:The Mittani",
    )
    .await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    let listed = page(&h, "/plugins/acme.esi/data-sources", &pilot)
        .await
        .body;
    assert!(listed.contains("The Mittani"), "{listed}");
    assert!(
        listed.contains(&format!("/apps/acme.esi/owners/{MITTANI}/withdraw")),
        "{listed}"
    );
    assert!(!listed.contains("Chribba"), "{listed}");
    assert!(!listed.contains("/approve"), "{listed}");
    // Coverage and the link to share are the admins'.
    assert!(!listed.contains("coverage-title"), "{listed}");
    // A character another account added (sold since): its new owner sees
    // nothing of that account's, not who nor what it read, and no notice.
    let chribba_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
            .bind(CHRIBBA)
            .fetch_one(&h.db)
            .await
            .unwrap();
    let added_by = |account: i64| {
        sqlx::query("UPDATE core.plugin_data_sources SET offered_by = $1 WHERE character_id = $2")
            .bind(account)
            .bind(MITTANI)
            .execute(&h.db)
    };
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    added_by(chribba_account).await.unwrap();
    let listed = page(&h, "/plugins/acme.esi/data-sources", &pilot)
        .await
        .body;
    assert!(
        listed.contains("Added by another account<")
            && listed.contains("Added from another account")
            && !listed.contains("Chribba"),
        "{listed}"
    );
    let main = page(&h, "/plugins/acme.esi", &pilot).await.body;
    assert!(!main.contains(r#"class="notice""#), "{main}");
    added_by(pilot_account).await.unwrap();
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
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(
        listed.contains("The Mittani") && listed.contains("Chribba"),
        "{listed}"
    );
    assert!(
        listed.contains(&format!("/apps/acme.esi/owners/{MITTANI}/remove")),
        "{listed}"
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
    assert_eq!(res.location(), "/plugins/acme.esi/data-sources");
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(listed.contains("Removed by Chribba"), "{listed}");
}

/// The Data sources page says how each source is doing, from the app's
/// calls through it, and how it reads each member corporation; a source
/// that stops working puts a notice on the app's other pages for those
/// who look after its sources.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn data_sources_say_how_they_are_doing_and_what_they_cover(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    const SOURCES: &str = "/plugins/acme.esi/data-sources";
    cover(&db, Builtin::Member, EntityKind::Corporation, 98133756).await;
    let (h, owner) = member_with_plugin(db).await;
    let gigx = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(state_of(&h, &gigx).await, "Member");

    // Two member corporations, neither read. Nobody may add a source yet:
    // what to grant, not a link that would show Directors Not found.
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(listed.contains("0 of 2 corporations read"), "{listed}");
    assert_eq!(
        listed.matches(">No data source</span>").count(),
        2,
        "{listed}"
    );
    assert!(
        listed.contains("Nobody may add data sources yet")
            && listed.contains(r#"Add owners (<span class="num">add_owner</span>)"#),
        "{listed}"
    );
    assert!(!listed.contains("Link to this page"), "{listed}");
    // Once someone may: the link to send a Director.
    tether_db::permissions::grant(
        &h.db,
        "plugin.acme.esi.add_owner",
        tether_db::permissions::Grantee::State(tether_core::states::StateId(1)),
    )
    .await
    .unwrap();
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(
        !listed.contains("Nobody may add data sources yet"),
        "{listed}"
    );
    assert!(
        listed.contains(r#"/plugins/acme.esi/data-sources" aria-label="Link to this page""#),
        "{listed}"
    );
    sqlx::query(
        "DELETE FROM core.permission_grants WHERE permission = 'plugin.acme.esi.add_owner'",
    )
    .execute(&h.db)
    .await
    .unwrap();
    // Tether's own pages take no posts, and nothing under them (in any
    // case) is the app's, though its main page rule covers every path.
    for host in ["data-sources", "activity"] {
        let res = send(
            &h.app,
            form(&format!("/plugins/acme.esi/{host}"), "", &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{host}: {}", res.body);
    }
    for under in [
        "data-sources/remove",
        "activity/x",
        "Activity",
        "DATA-SOURCES/x",
    ] {
        let uri = format!("/plugins/acme.esi/{under}");
        let res = page(&h, &uri, &owner).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{under}: {}", res.body);
        let res = send(&h.app, form(&uri, "_form=note", &owner)).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{under}: {}", res.body);
    }

    // Added: not read yet, then working once the app reads through it.
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(listed.contains(">Not read yet</span>"), "{listed}");
    assert!(
        listed.contains(">Not read yet through Chribba</span>"),
        "{listed}"
    );
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(listed.contains(">Working</span>"), "{listed}");
    assert!(listed.contains("1 of 2 corporations read"), "{listed}");
    assert!(listed.contains(">Read through Chribba</span>"), "{listed}");
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(!main.contains(r#"class="notice""#), "{main}");
    // The sidebar's foot counts it working, by the same rules.
    assert!(
        main.contains(r#"<span class="num text-foreground">1</span> working"#)
            && !main.contains("</span> not working"),
        "{main}"
    );
    // Manage opens Data sources, beside Activity, which names who looks.
    assert!(
        listed.contains(r#"href="/plugins/acme.esi/activity""#),
        "{listed}"
    );
    let activity = page(&h, "/plugins/acme.esi/activity", &owner).await.body;
    assert!(activity.contains("Viewing as Chribba"), "{activity}");

    // ESI refuses one endpoint (an in-game role): partly refused, and the
    // app's pages say so to those who look after its sources.
    Mock::given(method("GET"))
        .and(path(format!(
            "/corporation/{CHRIBBA_CORP}/mining/observers"
        )))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            serde_json::json!({"error": "Character does not have required role(s)"}),
        ))
        .mount(&h.esi_server)
        .await;
    let out = esi(&h, "corporation-mining-observers", ("source", CHRIBBA)).await;
    assert!(out.starts_with("err"), "{out}");
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(listed.contains(">Refused by ESI</span>"), "{listed}");
    assert!(
        listed.contains("ESI refused mining observers (403)"),
        "{listed}"
    );
    assert!(
        listed.contains(">Refused by ESI through Chribba</span>"),
        "{listed}"
    );
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains(r#"class="notice" data-tone="signal""#)
            && main.contains("ESI refused mining observers (403)"),
        "{main}"
    );
    assert!(
        main.contains(r#"<span class="num text-highlight">1</span> not working"#),
        "{main}"
    );
    // The sidebar's foot opens Administration's Data sources: every app's
    // sources, those not working first, each linking to its app's page.
    assert!(
        main.contains(r#"<a href="/admin/data-sources" class="source-health">"#),
        "{main}"
    );
    let all = page(&h, "/admin/data-sources", &owner).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    for part in [
        r#"<a href="/plugins/acme.esi/data-sources""#,
        ">Refused by ESI</span>",
        "ESI refused mining observers (403)",
        r#"<span class="num">1</span> not working"#,
        r#"<a href="/admin/data-sources" aria-current="page">Data sources</a>"#,
    ] {
        assert!(all.body.contains(part), "{part}: {}", all.body);
    }
    // The toolbar: the search, the app and whether they work.
    let broken = page(&h, "/admin/data-sources?status=broken", &owner)
        .await
        .body;
    assert!(
        broken.contains(r#"<a href="/plugins/acme.esi/data-sources""#),
        "{broken}"
    );
    assert!(broken.contains(r#"Status <span class="filter-chip-value">Not working</span>"#));
    let working = page(&h, "/admin/data-sources?status=working", &owner)
        .await
        .body;
    assert!(working.contains("No data source matches."), "{working}");
    let none = page(&h, "/admin/data-sources?q=zzz", &owner).await.body;
    assert!(none.contains("No data source matches."), "{none}");
    // Not to those who only use the app.
    grant_to_guests(&h, &owner, "view").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let main = page(&h, "/plugins/acme.esi", &pilot).await;
    assert_eq!(main.status, StatusCode::OK, "{}", main.body);
    assert!(!main.body.contains("ESI refused"), "{}", main.body);
    assert_eq!(
        page(&h, "/admin/data-sources", &pilot).await.status,
        StatusCode::FORBIDDEN
    );

    // Its login revoked: the source reads nothing, whatever it calls.
    sqlx::query("UPDATE core.character_tokens SET state = 'revoked' WHERE character_id = $1")
        .bind(CHRIBBA)
        .execute(&h.db)
        .await
        .unwrap();
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("err"), "{out}");
    let listed = page(&h, SOURCES, &owner).await.body;
    assert!(listed.contains(">Login stopped working</span>"), "{listed}");
    assert!(
        listed.contains(">Its data sources aren&#39;t working</span>"),
        "{listed}"
    );
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains(r#"class="notice" data-tone="danger""#)
            && main.contains("Its EVE login was revoked or expired"),
        "{main}"
    );
}

/// A stopped app's data sources and activity stay on its admin page, so
/// disabling an app keeps what it read in view, and its sources can still
/// be removed (and withdrawn from Token Management).
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_stopped_apps_sources_and_activity_stay_on_its_admin_page(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    let (_, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    // While it runs, they're under its Manage.
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(!admin.contains("owners-title"), "{admin}");
    assert!(!admin.contains("calls-title"), "{admin}");

    let res = send(&h.app, form("/admin/plugins/acme.esi/disable", "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for host in ["data-sources", "activity"] {
        let res = page(&h, &format!("/plugins/acme.esi/{host}"), &owner).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{host}");
    }
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(
        admin.contains("owners-title")
            && admin.contains(&format!("/apps/acme.esi/owners/{CHRIBBA}/remove")),
        "{admin}"
    );
    assert!(
        admin.contains("calls-title") && admin.contains("corporation-mining-extractions"),
        "{admin}"
    );
    // No link to send: its pages aren't there.
    assert!(!admin.contains("Link to this page"), "{admin}");
    // Its own sources stay in Token Management, not used.
    let tokens = page(&h, "/tokens", &owner).await.body;
    assert!(
        tokens.contains("ESI probe") && tokens.contains(">Not used</span>"),
        "{tokens}"
    );
    // Removed from there, back there.
    let res = send(
        &h.app,
        form(
            &format!("/apps/acme.esi/owners/{CHRIBBA}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/admin/plugins/acme.esi");
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(admin.contains("Removed by Chribba"), "{admin}");
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

    // Whether they're a superuser: the owner is; a pilot holding the app's
    // permission isn't; nobody is in a job.
    let res = page(&h, "/plugins/acme.esi/superuser", &owner).await;
    assert!(res.body.contains(">true<"), "{}", res.body);
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let pilot_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = 443630591")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind("plugin.acme.esi.view")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    let res = page(&h, "/plugins/acme.esi/superuser", &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains(">false<"), "{}", res.body);
    assert_eq!(probe(&h, "superuser", &[]).await, "false");
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
    // Only the characters `esi.characters` lists: for the bundled Member
    // Audit, those whose token lacks the app's scope or was revoked too
    // (kept, as aa-memberaudit keeps them), but not one sold on, nor one
    // whose account holds none of the app's permissions (below).
    let only = |sql: &'static str| {
        let db = h.db.clone();
        async move {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
    };
    only("UPDATE core.character_tokens SET scopes = '{}'").await;
    assert!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false)
            .await
            .contains("Chribba")
    );
    only("UPDATE core.character_tokens SET scopes = ARRAY['esi-skills.read_skills.v1'], state = 'revoked'").await;
    assert!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false)
            .await
            .contains("Chribba")
    );
    only("UPDATE core.character_tokens SET revoked_reason = 'owner hash changed'").await;
    assert_eq!(
        run_probe(&h, "tether.member-audit", "owners", Vec::new(), false).await,
        "Some([])"
    );
    only("UPDATE core.character_tokens SET state = 'valid', revoked_reason = NULL").await;
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
    // Every character of the app's members, registered or not, is Member
    // Audit's alone too (aa-memberaudit's Finder and compliance reports):
    // an alt not registered with it is listed, unregistered; a pilot
    // holding none of its permissions isn't; nor is a character sold on.
    sqlx::query(
        "INSERT INTO core.characters (id, account_id, name, corporation_id) \
         SELECT 90000077, account_id, 'Unregistered Alt', 98000001 FROM core.characters WHERE id = $1",
    )
    .bind(CHRIBBA)
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(probe(&h, "members", &[]).await, "None");
    assert_eq!(
        run_probe(&h, "acme.bundled", "members", Vec::new(), true).await,
        "None"
    );
    for as_page in [false, true] {
        let members = run_probe(&h, "tether.member-audit", "members", Vec::new(), as_page).await;
        for part in [
            format!(
                "main: Character {{ id: {CHRIBBA}, name: \"Chribba\", corporation-id: {CHRIBBA_CORP}"
            ),
            format!("character: Character {{ id: {CHRIBBA}, name: \"Chribba\""),
            "name: \"Unregistered Alt\", corporation-id: 98000001, alliance-id: None }, \
             registered: false"
                .to_owned(),
            "name: \"Member\"".to_owned(),
        ] {
            assert!(members.contains(&part), "{part}\n{members}");
        }
        assert!(!members.contains("The Mittani"), "{members}");
    }
    sqlx::query(
        "INSERT INTO core.character_tokens (character_id, refresh_token, scopes, state, revoked_reason) \
         SELECT 90000077, refresh_token, scopes, 'revoked', 'owner hash changed' \
         FROM core.character_tokens WHERE character_id = $1",
    )
    .bind(CHRIBBA)
    .execute(&h.db)
    .await
    .unwrap();
    let members = run_probe(&h, "tether.member-audit", "members", Vec::new(), false).await;
    assert!(!members.contains("Unregistered Alt"), "{members}");
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
async fn a_signed_app_under_member_audits_id_learns_no_owners_or_members(db: PgPool) {
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
    for asked in ["owners", "members"] {
        assert_eq!(
            run_probe(&h, "tether.member-audit", asked, Vec::new(), false).await,
            "None"
        );
    }
    // Nor does the host send Member Audit's token-error notice for it:
    // nothing told, nothing marked.
    sqlx::query(
        "UPDATE core.character_tokens SET state = 'revoked', revoked_reason = 'invalid_grant' \
         WHERE character_id = $1",
    )
    .bind(CHRIBBA)
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(
        tether_web::compliance::token_errors(&h.db).await.unwrap(),
        0
    );
    let marked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.app_characters \
         WHERE plugin_id = 'tether.member-audit' AND token_error_notified_at IS NOT NULL",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(marked, 0);
    let told: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.notifications WHERE title LIKE 'Member Audit: Invalid%'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(told, 0);
}

/// HR Applications, as bundled with Tether, reads the characters now on
/// the account behind one of its own submitter references (AA core's
/// hrapplications shows and searches them); nobody else does.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_bundled_hr_applications_reads_its_submitters_characters(db: PgPool) {
    let hr = bundled_probe("tether.hr-applications");
    let other = bundled_probe("acme.bundled");
    let h = harness_with_bundled(db, vec![hr.clone(), other.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    approve_bundled(&h, &owner, "tether.hr-applications", &hr).await;
    approve_bundled(&h, &owner, "acme.bundled", &other).await;
    install(&h, &owner).await;
    let account = account_of(&h, CHRIBBA).await;
    let reference = |plugin: &'static str| {
        let db = h.db.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "INSERT INTO core.plugin_submitters (plugin_id, account_id) VALUES ($1, $2) \
                 RETURNING reference",
            )
            .bind(plugin)
            .bind(account)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let hr_ref = reference("tether.hr-applications").await;
    let other_ref = reference("acme.bundled").await;
    let signed_ref = reference(ID).await;
    let read = |plugin: &'static str, reference: String| {
        run_probe(
            &h,
            plugin,
            "submitter-characters",
            vec![("reference".to_owned(), reference)],
            true,
        )
    };
    // An alt added since: the account as it is now.
    sqlx::query(
        "INSERT INTO core.characters (id, account_id, name, corporation_id, alliance_id) \
         VALUES (90000078, $1, 'Later Alt', 98000001, 99000001)",
    )
    .bind(account)
    .execute(&h.db)
    .await
    .unwrap();
    let out = read("tether.hr-applications", hr_ref.clone()).await;
    for part in [
        format!("id: {CHRIBBA}, name: \"Chribba\""),
        "name: \"Later Alt\", corporation-id: 98000001, alliance-id: Some(99000001)".to_owned(),
    ] {
        assert!(out.contains(&part), "{part}\n{out}");
    }
    // Not another app's reference, nor a made-up one, nor one past its
    // year; and never for another app, even its own reference.
    assert_eq!(
        read("tether.hr-applications", other_ref.clone()).await,
        "None"
    );
    assert_eq!(read("tether.hr-applications", "0".repeat(32)).await, "None");
    assert_eq!(
        read("tether.hr-applications", "not hex".to_owned()).await,
        "None"
    );
    assert_eq!(read("acme.bundled", other_ref).await, "None");
    assert_eq!(read(ID, signed_ref).await, "None");
    // At most 1,000 lookups a call, a malformed reference's included.
    for (before, answered) in [("999", true), ("1000", false)] {
        let out = run_probe(
            &h,
            "tether.hr-applications",
            "submitter-lookups",
            vec![
                ("reference".to_owned(), hr_ref.clone()),
                ("n".to_owned(), before.to_owned()),
            ],
            false,
        )
        .await;
        assert_eq!(out, format!("answered={answered}"));
    }
    sqlx::query(
        "UPDATE core.plugin_submitters SET last_posted_at = now() - interval '366 days' \
         WHERE plugin_id = 'tether.hr-applications'",
    )
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(read("tether.hr-applications", hr_ref).await, "None");
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
    // No ping channel to give it yet: its page says where to add one.
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(
        admin.contains(r#"A channel must be a ping channel first: add one on the <a class="underline underline-offset-4" href="/admin/discord">"#),
        "{admin}"
    );
    discord_ready(&h, &owner).await;
    let out = probe(
        &h,
        "send",
        &[("channel", DISCORD_PING_CHANNEL), ("text", "pop")],
    )
    .await;
    assert!(out.contains("not one of this plugin"), "{out}");
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(admin.contains("Add a ping channel"), "{admin}");
    assert!(
        !admin.contains("A channel must be a ping channel first"),
        "{admin}"
    );

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

    // A card: the mention is the text above it, the images are CCP's,
    // and nothing in it can ping.
    let out = probe(
        &h,
        "embed",
        &[("title", "Extraction started"), ("state", "member")],
    )
    .await;
    assert_eq!(out, "ok");
    let sent = h.discord_server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&sent.last().unwrap().body).unwrap();
    assert_eq!(body["content"], format!("<@&{DISCORD_MEMBER_ROLE}>"));
    assert_eq!(
        body["embeds"],
        serde_json::json!([{
            "title": "Extraction started",
            "description": "Chunk arrives <t:1793592000:R> @\u{200B}everyone",
            "color": 0x2e_cc71,
            "author": {
                "name": "Acme Corp",
                "icon_url": "https://images.evetech.net/corporations/98000001/logo?size=64",
            },
            "thumbnail": { "url": "https://images.evetech.net/types/35835/render?size=128" },
            "fields": [
                { "name": "System", "value": "Mazitah", "inline": true },
                { "name": "Structure", "value": "Mazitah - Refinery", "inline": false },
            ],
            "footer": { "text": "Structures" },
            "timestamp": "2026-11-02T04:00:00Z",
        }])
    );
    let out = probe(&h, "embed", &[("title", "")]).await;
    assert!(out.contains("title is 1 to 256"), "{out}");
    let out = probe(&h, "embed", &[("title", "x"), ("image", "-1")]).await;
    assert!(out.contains("image id is positive"), "{out}");
}

/// Several pings in one message (aa-structures' ping groups): the roles
/// Tether maps to states (by name) and, for an app approved for
/// `mention_groups`, groups (by id, from `ping-groups`); one with no role
/// is left out and the message still goes. Never @everyone, @here or
/// anyone else.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn discord_messages_ping_several_state_and_group_roles(db: PgPool) {
    const FC_ROLE: &str = "500000000000000004";
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    let key = Key::new(3);
    let manifest = format!(
        "[plugin]\nid = \"acme.pings\"\nname = \"Pings\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\ndiscord = [\"send_message\", \"mention_groups\"]\n",
        key.public()
    );
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &probe_component()),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    // The install review says it can mention groups.
    let review = page(&h, "/admin/plugins/acme.pings", &owner).await.body;
    assert!(review.contains("Discord group mentions"), "{review}");
    discord_ready(&h, &owner).await;
    for id in [ID, "acme.pings"] {
        let res = send(
            &h.app,
            form(
                &format!("/admin/plugins/{id}/channels"),
                &format!("channel_id={DISCORD_PING_CHANNEL}"),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "700000000000000001", "channel_id": DISCORD_PING_CHANNEL }),
        ))
        .mount(&h.discord_server)
        .await;
    let group: i64 =
        sqlx::query_scalar("INSERT INTO core.groups (name) VALUES ('Capital FCs') RETURNING id")
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO core.discord_role_mappings (role_id, role_name, group_id) \
         VALUES ($1, 'Capital FCs', $2)",
    )
    .bind(FC_ROLE.parse::<i64>().unwrap())
    .bind(group)
    .execute(&h.db)
    .await
    .unwrap();
    let no_role: i64 =
        sqlx::query_scalar("INSERT INTO core.groups (name) VALUES ('No Role') RETURNING id")
            .fetch_one(&h.db)
            .await
            .unwrap();
    let last_body = || async {
        let sent = h.discord_server.received_requests().await.unwrap();
        let last = sent
            .iter()
            .rev()
            .find(|r| r.url.path().ends_with("/messages"))
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&last.body).unwrap()
    };
    let pings = |query: &[(&str, &str)]| {
        run_probe(
            &h,
            "acme.pings",
            "send-message",
            query
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            false,
        )
    };

    // The groups it may offer: those with a role (this one Hidden and
    // Internal, as new groups are), by id and name; nothing else. From a
    // page too. An app not approved for group mentions gets none.
    let listed = format!("[PingGroup {{ id: {group}, name: \"Capital FCs\" }}]");
    for from_page in [false, true] {
        let out = run_probe(&h, "acme.pings", "ping-groups", Vec::new(), from_page).await;
        assert_eq!(out, listed);
    }
    let out = run_probe(&h, ID, "ping-groups", Vec::new(), true).await;
    assert_eq!(out, "[]");

    // A state's (any case) and a group's role, each once; a group with no
    // role, one that doesn't exist and an unmapped state are left out.
    let groups = format!("{group},{no_role},{},{group}", no_role + 1000);
    let out = pings(&[
        ("text", "Timer @here"),
        ("states", "member,Blue"),
        ("groups", &groups),
    ])
    .await;
    assert_eq!(out, "ok");
    let body = last_body().await;
    assert_eq!(
        body["content"],
        format!("<@&{DISCORD_MEMBER_ROLE}> <@&{FC_ROLE}> Timer @\u{200B}here")
    );
    assert_eq!(
        body["allowed_mentions"],
        serde_json::json!({ "parse": [], "roles": [DISCORD_MEMBER_ROLE, FC_ROLE] })
    );
    // A renamed group is still pinged: pings follow the group, not its
    // name, and it's offered under its new one.
    sqlx::query("UPDATE core.groups SET name = 'Super FCs' WHERE id = $1")
        .bind(group)
        .execute(&h.db)
        .await
        .unwrap();
    let out = pings(&[("text", "Renamed"), ("groups", &group.to_string())]).await;
    assert_eq!(out, "ok");
    assert_eq!(
        last_body().await["allowed_mentions"]["roles"],
        serde_json::json!([FC_ROLE])
    );
    let out = run_probe(&h, "acme.pings", "ping-groups", Vec::new(), true).await;
    assert!(out.contains("Super FCs"), "{out}");

    // Nothing to ping still sends; a card alone too.
    let nobody = (no_role + 1000).to_string();
    let out = pings(&[("groups", &nobody), ("title", "Reinforced")]).await;
    assert_eq!(out, "ok");
    let body = last_body().await;
    assert_eq!(body["content"], "");
    assert_eq!(
        body["allowed_mentions"],
        serde_json::json!({ "parse": [], "roles": [] })
    );
    assert_eq!(body["embeds"][0]["title"], "Reinforced");

    // At most 10 pings; not from a page.
    let many = ["Member"; 11].join(",");
    let out = pings(&[("text", "x"), ("states", &many)]).await;
    assert!(out.contains("at most 10 pings"), "{out}");
    let out = run_probe(
        &h,
        "acme.pings",
        "send-message",
        vec![("text".to_owned(), "x".to_owned())],
        true,
    )
    .await;
    assert!(out.contains("pages can't send"), "{out}");

    // An app not approved for group mentions can't name a group; states
    // are fine.
    let out = run_probe(
        &h,
        ID,
        "send-message",
        vec![
            ("text".to_owned(), "x".to_owned()),
            ("groups".to_owned(), group.to_string()),
        ],
        false,
    )
    .await;
    assert!(out.contains("wasn't approved to mention groups"), "{out}");
    let out = run_probe(
        &h,
        ID,
        "send-message",
        vec![
            ("text".to_owned(), "x".to_owned()),
            ("states".to_owned(), "Member".to_owned()),
        ],
        false,
    )
    .await;
    assert_eq!(out, "ok");
    assert_eq!(
        last_body().await["allowed_mentions"]["roles"],
        serde_json::json!([DISCORD_MEMBER_ROLE])
    );
}

/// Discord refusing the bot is final for the app, so it moves on (Moon
/// Mining finishes its job, the relays mark the one message failed);
/// only a passing failure says "try later". Before, every refusal came
/// back Unavailable: pings retried until dead and outboxes jammed. A
/// refusal in a channel is remembered for a minute: the app's next sends
/// there (its retry without the mention) aren't sent to Discord again,
/// nor counted against its sends.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn discord_refusing_the_bot_is_final_for_the_app(db: PgPool) {
    const OTHER_CHANNEL: &str = "600000000000000002";
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner).await;
    discord_ready(&h, &owner).await;
    let res = send(
        &h.app,
        form(
            "/admin/discord/channels",
            &format!("channel_id={OTHER_CHANNEL}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    for channel in [DISCORD_PING_CHANNEL, OTHER_CHANNEL] {
        let res = send(
            &h.app,
            form(
                "/admin/plugins/acme.esi/channels",
                &format!("channel_id={channel}"),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    async fn answer(h: &Harness, status: u16, body: serde_json::Value) {
        h.discord_server.reset().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/api/v10/channels/\d+/messages$"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&h.discord_server)
            .await;
    }
    async fn send_to(h: &Harness, channel: &str, state: Option<&str>) -> String {
        let mut query = vec![("channel", channel), ("text", "Moon popped")];
        query.extend(state.map(|s| ("state", s)));
        probe(h, "send", &query).await
    }
    async fn asked(h: &Harness, channel: &str) -> usize {
        let at = format!("/api/v10/channels/{channel}/messages");
        h.discord_server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == at)
            .count()
    }

    // Discord down: try later, and the next send asks again.
    answer(
        &h,
        500,
        serde_json::json!({"code": 0, "message": "Internal Server Error"}),
    )
    .await;
    assert_eq!(
        send_to(&h, DISCORD_PING_CHANNEL, None).await,
        "err Error::Unavailable"
    );
    assert_eq!(
        send_to(&h, DISCORD_PING_CHANNEL, None).await,
        "err Error::Unavailable"
    );
    assert_eq!(asked(&h, DISCORD_PING_CHANNEL).await, 2);

    // The bot may not post there: final, in plain words.
    answer(
        &h,
        403,
        serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
    )
    .await;
    let out = send_to(&h, DISCORD_PING_CHANNEL, Some("member")).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    assert!(
        out.contains("give it View Channel and Send Messages"),
        "{out}"
    );
    // The app's retry without the mention, and the rest of its messages
    // there, get the same answer without Discord being asked again, and
    // without spending the app's sends.
    for _ in 0..tether_web::plugin_services::SENDS_PER_MINUTE {
        assert_eq!(send_to(&h, DISCORD_PING_CHANNEL, None).await, out);
    }
    assert_eq!(asked(&h, DISCORD_PING_CHANNEL).await, 1);

    // Another channel is asked: deleted in Discord, so final too.
    answer(
        &h,
        404,
        serde_json::json!({"code": 10003, "message": "Unknown Channel"}),
    )
    .await;
    let out = send_to(&h, OTHER_CHANNEL, None).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    assert!(out.contains("That channel no longer exists"), "{out}");
    assert_eq!(asked(&h, OTHER_CHANNEL).await, 1);
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
         [permissions]\nview = \"See\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
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
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert_eq!(run_now_audits(&h.db).await.len(), 1);

    // With plenty of ESI budget left, an alt registered five minutes
    // after the last run runs them at once (only a minute apart at least;
    // ten while the budget is low).
    finish_runs(&h.db, "5 minutes").await;
    let owner = log_in_as(&h, "443630591:The Mittani", Some(&owner)).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "443630591:The Mittani",
    )
    .await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );
    assert_eq!(run_now_audits(&h.db).await.len(), 2);
    // Another alt while that run still waits: it's the same run.
    let owner = log_in_as(&h, "1887431749:gigX", Some(&owner)).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "1887431749:gigX",
    )
    .await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );
    assert_eq!(run_now_audits(&h.db).await.len(), 2);
    // One while that run is running (it may have read before the new
    // character came): one more is queued behind it.
    sqlx::query(
        "UPDATE core.jobs SET state = 'running' WHERE kind = 'plugin.job' AND state = 'queued'",
    )
    .execute(&h.db)
    .await
    .unwrap();
    sqlx::query("UPDATE core.schedules SET last_enqueued_at = now() - interval '2 minutes'")
        .execute(&h.db)
        .await
        .unwrap();
    let owner = log_in_as(&h, "406944591:Fourth", Some(&owner)).await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "406944591:Fourth",
    )
    .await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );
    assert_eq!(run_now_audits(&h.db).await.len(), 3);

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
    assert_eq!(audits.len(), 4, "{audits:?}");
    assert_eq!(audits[3].0, admin);
    assert_eq!(
        audits[3].2,
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
    // The Data sources page lists it as removed; holders add it again in
    // one login.
    let listed = page(&h, "/plugins/acme.esi/data-sources", &owner)
        .await
        .body;
    assert!(listed.contains("Removed by"), "{listed}");
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
    // Member holds none of the app's permissions: its pilots couldn't
    // register (only the owner, who holds everything), and States says so
    // until one is granted.
    assert!(
        states.contains("Member holds none of ESI probe's permissions"),
        "{states}"
    );
    tether_db::permissions::grant(
        &h.db,
        "plugin.acme.esi.view",
        tether_db::permissions::Grantee::State(tether_core::states::StateId(MEMBER_STATE)),
    )
    .await
    .unwrap();
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(!states.contains("holds none of"), "{states}");
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

// ---- writes (Save to EVE) -------------------------------------------------------

const WRITE_FITTINGS: &str = "esi-fittings.write_fittings.v1";
const FITTING: &str = r#"{"name":"Fast Tackle","description":"","ship_type_id":587,"items":[{"flag":"LoSlot0","quantity":1,"type_id":2048}]}"#;

/// The probe, asking pilots for the fitting write scope.
async fn install_writer(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"ESI probe\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities.esi]\nuser = [\"{WRITE_FITTINGS}\"]\n\n\
         [permissions]\nview = \"See\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(h, owner, &bytes, &key.sign(&bytes)).await;
}

/// Someone looking: Chribba's account, with his one character.
async fn chribba_looking(h: &Harness) -> tether_plugins::services::Viewer {
    use tether_plugins::services::{Builtin, Character, State, Viewer};
    let account: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(CHRIBBA)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let me = Character {
        id: CHRIBBA,
        name: "Chribba".to_owned(),
        corporation_id: CHRIBBA_CORP,
        alliance_id: Some(159826257),
    };
    Viewer {
        account_id: account,
        main: me.clone(),
        characters: vec![me],
        state: State {
            name: "Member".to_owned(),
            builtin: Some(Builtin::Member),
        },
        permissions: vec!["view".to_owned()],
    }
}

async fn save_fitting(
    h: &Harness,
    viewer: Option<tether_plugins::services::Viewer>,
    as_page: bool,
    character: i64,
    body: &str,
) -> String {
    let query = vec![
        ("endpoint".to_owned(), "character-fitting-save".to_owned()),
        ("character".to_owned(), character.to_string()),
        ("body".to_owned(), body.to_owned()),
    ];
    run_probe_as(h, ID, "esi-post", query, viewer, as_page).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_write_to_eve_only_for_the_pilot_at_their_own_click(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install_writer(&h, &owner).await;
    run_jobs(&h).await;
    // Two fittings reach ESI, whatever else is tried: one press each.
    Mock::given(method("POST"))
        .and(path(format!("/characters/{CHRIBBA}/fittings")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "fitting_id": 42
        })))
        .expect(2)
        .mount(&h.esi_server)
        .await;
    let chribba = chribba_looking(&h).await;

    // Not registered for the app yet.
    let out = save_fitting(&h, Some(chribba.clone()), false, CHRIBBA, FITTING).await;
    assert_eq!(out, "err Error::NotRegistered");
    let (asked, _) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    assert!(asked.contains(&WRITE_FITTINGS.to_owned()), "{asked:?}");

    // Nobody looking (as in a job), or a page render: never.
    let out = save_fitting(&h, None, false, CHRIBBA, FITTING).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = save_fitting(&h, Some(chribba.clone()), true, CHRIBBA, FITTING).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // Only the pilot's own characters.
    let out = save_fitting(&h, Some(chribba.clone()), false, MITTANI, FITTING).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // A read can't reach a write, and the body must be ESI's fitting.
    let out = esi(&h, "character-fitting-save", ("character", CHRIBBA)).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    let out = save_fitting(&h, Some(chribba.clone()), false, CHRIBBA, "{}").await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    let out = save_fitting(&h, Some(chribba.clone()), false, CHRIBBA, "not json").await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    let unknown = vec![
        ("endpoint".to_owned(), "character-contacts-save".to_owned()),
        ("character".to_owned(), CHRIBBA.to_string()),
        ("body".to_owned(), FITTING.to_owned()),
    ];
    let out = run_probe_as(&h, ID, "esi-post", unknown, Some(chribba.clone()), false).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");

    // The pilot's own click, for their own registered character: saved,
    // logged, and on the audit log as them.
    let out = save_fitting(&h, Some(chribba.clone()), false, CHRIBBA, FITTING).await;
    assert_eq!(out, r#"ok {"fitting_id":42}"#);
    assert!(
        access_log(&h.db)
            .await
            .contains(&("character-fitting-save".to_owned(), "ok".to_owned()))
    );
    let audited: Vec<(Option<i64>, serde_json::Value)> = sqlx::query_as(
        "SELECT actor_account_id, details FROM core.audit_log WHERE action = 'plugin.esi_write'",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        audited,
        vec![(
            Some(chribba.account_id),
            serde_json::json!({ "endpoint": "character-fitting-save", "character_id": CHRIBBA })
        )]
    );

    // One change in EVE per press of a button.
    let twice = vec![
        ("endpoint".to_owned(), "character-fitting-save".to_owned()),
        ("character".to_owned(), CHRIBBA.to_string()),
        ("body".to_owned(), FITTING.to_owned()),
        ("times".to_owned(), "2".to_owned()),
    ];
    let out = run_probe_as(&h, ID, "esi-post", twice, Some(chribba.clone()), false).await;
    let (first, second) = out.split_once(" | ").unwrap();
    assert_eq!(first, r#"ok {"fitting_id":42}"#);
    assert!(second.starts_with("err Error::NotAllowed"), "{second}");
}

/// The probe at `version`, asking pilots for `user` scopes.
fn probe_package(version: &str, user: &[&str]) -> (Vec<u8>, String) {
    let key = Key::new(1);
    let user = user
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"ESI probe\"\nversion = \"{version}\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\ndiscord = [\"send_message\"]\n\n\
         [capabilities.esi]\nuser = [{user}]\ndata_source = [\"{MINING}\"]\n\n\
         [permissions]\nview = \"See\"\nmanage = \"Manage\"\nadd_owner = \"Add owners\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_new_write_scope_asks_pilots_again_and_no_state_may_require_it(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    let chribba = chribba_looking(&h).await;
    // Not one of the app's scopes: no writes.
    let out = save_fitting(&h, Some(chribba.clone()), false, CHRIBBA, FITTING).await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");

    // Registered for reading, and required of Member.
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    let registered = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM core.app_characters")
            .fetch_one(&h.db)
            .await
            .unwrap()
    };
    assert_eq!(registered().await, 1);
    let require = format!("/admin/states/{MEMBER_STATE}/scopes/app");
    let applied = send(&h.app, form(&require, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(applied.location(), "/admin/states", "{}", applied.body);

    // A version that also writes: refused while a state requires the app.
    let (bytes, signature) = probe_package("1.1.0", &[SKILLS, WRITE_FITTINGS]);
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review_at = res.location().to_owned();
    let review = page(&h, &review_at, &owner).await.body;
    assert!(
        review.contains("Changes pilots&#39;") || review.contains("Changes pilots'"),
        "{review}"
    );
    let refused = send(&h.app, form(&format!("{review_at}/approve"), "", &owner)).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(
        refused.body.contains("Stop requiring ESI probe"),
        "{}",
        refused.body
    );
    assert_eq!(registered().await, 1);

    // Not required: it installs, and pilots register again to consent to it.
    let stop = format!("/admin/states/{MEMBER_STATE}/scopes/app/remove");
    let res = send(&h.app, form(&stop, "plugin=acme.esi", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    install_package(&h, &owner, &bytes, &signature).await;
    assert_eq!(registered().await, 0);
    let cleared: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'plugin.registrations_cleared'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(cleared["characters"], 1);
    let out = save_fitting(&h, Some(chribba), false, CHRIBBA, FITTING).await;
    assert_eq!(out, "err Error::NotRegistered");
    // And the States page no longer offers to require it.
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(!states.contains("Require ESI probe"), "{states}");
}

/// An app asking for another (read) scope keeps reading its registered
/// characters with the scopes their tokens have, as aa-memberaudit's
/// sections each fetch a token for their own scope: only a call needing
/// the new one is refused (`MissingScope`; `NotRegistered` through the
/// older `get`). They stay registered and compliant (aa-memberaudit's
/// compliance is every character registered), and their pilot is asked,
/// gently, to register again, which allows it.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_app_asking_for_another_scope_keeps_reading_what_it_may(db: PgPool) {
    const ONLINE: &str = "esi-location.read_online.v1";
    let (h, owner) = member_with_plugin(db).await;
    tether_db::permissions::grant(
        &h.db,
        "plugin.acme.esi.view",
        tether_db::permissions::Grantee::State(tether_core::states::StateId(MEMBER_STATE)),
    )
    .await
    .unwrap();
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    let require = format!("/admin/states/{MEMBER_STATE}/scopes/app");
    let applied = send(&h.app, form(&require, "plugin=acme.esi&confirm=1", &owner)).await;
    assert_eq!(applied.location(), "/admin/states", "{}", applied.body);
    run_jobs(&h).await;
    let compliant = || async {
        sqlx::query_scalar::<_, bool>(
            "SELECT compliant FROM core.accounts a JOIN core.characters c ON c.account_id = a.id \
             WHERE c.id = $1",
        )
        .bind(CHRIBBA)
        .fetch_one(&h.db)
        .await
        .unwrap()
    };
    assert!(compliant().await);

    // 1.1.0 also reads whether the character is online.
    let (bytes, signature) = probe_package("1.1.0", &[SKILLS, ONLINE]);
    install_package(&h, &owner, &bytes, &signature).await;
    run_jobs(&h).await;
    assert!(compliant().await, "still registered, still compliant");
    let characters = probe(&h, "characters", &[]).await;
    assert!(characters.contains("Chribba"), "{characters}");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    let out = esi(&h, "character-online", ("character", CHRIBBA)).await;
    assert_eq!(out, format!("err Error::MissingScope(\"{ONLINE}\")"));
    let raw = probe(
        &h,
        "esi",
        &[
            ("endpoint", "character-online"),
            ("character", &CHRIBBA.to_string()),
            ("raw", "1"),
        ],
    )
    .await;
    assert_eq!(raw, "err Error::NotRegistered");
    let log = access_log(&h.db).await;
    assert_eq!(
        log.last(),
        Some(&(
            "character-online".to_owned(),
            "token lacks the scope".to_owned()
        ))
    );
    // Not a token error; the app's page says what registering again allows,
    // and the pilot is told once.
    assert_eq!(
        tether_web::compliance::token_errors(&h.db).await.unwrap(),
        0
    );
    let app = page(&h, "/register?app=acme.esi", &owner).await.body;
    assert!(
        app.contains("Register again to allow: Read whether the character is online"),
        "{app}"
    );
    assert!(app.contains(">Register again</button>"), "{app}");
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(checklist.contains("You're all set"), "{checklist}");
    assert_eq!(
        tether_web::compliance::scope_prompts(&h.db).await.unwrap(),
        1
    );
    assert_eq!(
        tether_web::compliance::scope_prompts(&h.db).await.unwrap(),
        0
    );

    // Registering again allows it.
    let (asked, owner) = grant(
        &h,
        &owner,
        "/register/start?app=acme.esi",
        "196379789:Chribba",
    )
    .await;
    assert!(asked.contains(&ONLINE.to_owned()), "{asked:?}");
    Mock::given(method("GET"))
        .and(path(format!("/characters/{CHRIBBA}/online")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "online": true, "last_login": "2026-09-30T18:05:00Z",
            "last_logout": "2026-09-30T17:40:00Z", "logins": 12
        })))
        .mount(&h.esi_server)
        .await;
    let out = esi(&h, "character-online", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    let app = page(&h, "/register?app=acme.esi", &owner).await.body;
    assert!(!app.contains("Register again to allow"), "{app}");
}

/// aa-memberaudit's removal notices: dropping a character from Member Audit
/// (as bundled) tells the holders of `notified_on_character_removal` whose
/// view scope covers the pilot.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_character_dropped_from_member_audit_notifies_holders_in_scope(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let manifest = format!(
        "[plugin]\nid = \"tether.member-audit\"\nname = \"Member Audit\"\nversion = \"1.0.0\"\n\
         host_api = \"1\"\n\n[capabilities.esi]\nuser = [\"{SKILLS}\"]\n\n[permissions]\n\
         basic_access = \"Use it\"\nview_same_corporation = \"Corporation\"\n\
         view_everything = \"Everything\"\nnotified_on_character_removal = \"Notified\"\n"
    );
    let package = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &probe_component()),
    ]);
    let h = harness_with_bundled(db, vec![package.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    approve_bundled(&h, &owner, "tether.member-audit", &package).await;
    run_jobs(&h).await;
    // The Mittani (another corporation) may see his own corporation only.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let pilot_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
            .bind(MITTANI)
            .fetch_one(&h.db)
            .await
            .unwrap();
    for permission in [
        "notified_on_character_removal",
        "view_same_corporation",
        "basic_access",
    ] {
        sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
            .bind(format!("plugin.tether.member-audit.{permission}"))
            .bind(pilot_account)
            .execute(&h.db)
            .await
            .unwrap();
    }
    let _ = pilot;

    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    let res = send(
        &h.app,
        form(
            "/register/unregister?app=tether.member-audit",
            &format!("character_id={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    let notices: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT account_id, title, message FROM core.notifications \
         WHERE title = 'Member Audit: Character has been removed!' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    // The superuser (view_everything, as AA's) is told; the Mittani's
    // corporation isn't Chribba's.
    let owner_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
            .bind(CHRIBBA)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(
        notices,
        vec![(
            owner_account,
            "Member Audit: Character has been removed!".to_owned(),
            "Chribba has removed character Chribba".to_owned()
        )]
    );

    // With view_everything, the Mittani is told too.
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind("plugin.tether.member-audit.view_everything")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    let (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    let res = send(
        &h.app,
        form(
            "/register/unregister?app=tether.member-audit",
            &format!("character_id={CHRIBBA}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let told: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.notifications WHERE account_id = $1 \
         AND title = 'Member Audit: Character has been removed!'",
    )
    .bind(pilot_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(told, 1);
}

/// aa-memberaudit's token-error notices, sent by the host: a character
/// registered with Member Audit (as bundled) that can't be read tells its
/// pilot once, until it works again; never a sold one, nor one not
/// registered with it, nor one whose pilot holds none of its permissions;
/// not while the setting is off.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_audit_character_that_cant_be_read_is_told_once(db: PgPool) {
    use tether_core::states::{Builtin, EntityKind};
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let manifest = format!(
        "[plugin]\nid = \"tether.member-audit\"\nname = \"Member Audit\"\nversion = \"1.0.0\"\n\
         host_api = \"1\"\n\n[capabilities.esi]\nuser = [\"{SKILLS}\"]\n\n[permissions]\n\
         basic_access = \"Use it\"\n"
    );
    let package = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &probe_component()),
    ]);
    let h = harness_with_bundled(db, vec![package.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    approve_bundled(&h, &owner, "tether.member-audit", &package).await;
    run_jobs(&h).await;
    // The Mittani, with a token that's gone, but not registered with it.
    log_in_as(&h, "443630591:The Mittani", None).await;
    let (_, mut owner) = grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    let owner_account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    async fn run(h: &Harness) -> usize {
        tether_web::compliance::token_errors(&h.db).await.unwrap()
    }
    async fn token(h: &Harness, character: i64, state: &str, reason: Option<&str>) {
        sqlx::query(
            "UPDATE core.character_tokens SET state = $2, revoked_reason = $3 \
             WHERE character_id = $1",
        )
        .bind(character)
        .bind(state)
        .bind(reason)
        .execute(&h.db)
        .await
        .unwrap();
    }
    async fn notices(h: &Harness) -> Vec<(i64, String, String, String)> {
        sqlx::query_as(
            "SELECT account_id, level, title, message FROM core.notifications \
             WHERE title LIKE 'Member Audit: Invalid%' ORDER BY id",
        )
        .fetch_all(&h.db)
        .await
        .unwrap()
    }
    async fn marked(h: &Harness, character: i64) -> bool {
        sqlx::query_scalar(
            "SELECT token_error_notified_at IS NOT NULL FROM core.app_characters \
             WHERE plugin_id = 'tether.member-audit' AND character_id = $1",
        )
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
    }
    token(&h, MITTANI, "revoked", Some("invalid_grant")).await;
    assert_eq!(run(&h).await, 0);

    // EVE refused its login: one danger notice, in AA's words, then
    // nothing more while it stays broken.
    token(&h, CHRIBBA, "revoked", Some("invalid_grant")).await;
    assert_eq!(run(&h).await, 1);
    assert_eq!(run(&h).await, 0);
    let told = notices(&h).await;
    assert_eq!(told.len(), 1, "{told:?}");
    let (account, level, title, message) = &told[0];
    assert_eq!(*account, owner_account);
    assert_eq!(level, "danger");
    assert_eq!(title, "Member Audit: Invalid or missing token for Chribba");
    assert!(
        message
            .starts_with("Member Audit could not find a valid token for your character Chribba.")
            && message.contains("Its EVE login has stopped working"),
        "{message}"
    );
    assert!(marked(&h, CHRIBBA).await);

    // Registered again: the mark clears, and the next breakage tells
    // them again. A token deleted in Token Management counts (as AA).
    (_, owner) = grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    assert_eq!(run(&h).await, 0);
    assert!(!marked(&h, CHRIBBA).await);
    token(&h, CHRIBBA, "revoked", Some("deleted")).await;
    assert_eq!(run(&h).await, 1);
    let told = notices(&h).await;
    assert!(
        !told[1].3.contains("Its EVE login has stopped working"),
        "{told:?}"
    );

    // Sold: it leaves the account anyway, and nobody is told (AA's
    // orphans aren't).
    grant(
        &h,
        &owner,
        "/register/start?app=tether.member-audit",
        "196379789:Chribba",
    )
    .await;
    assert_eq!(run(&h).await, 0);
    token(&h, CHRIBBA, "revoked", Some("owner hash changed")).await;
    assert_eq!(run(&h).await, 0);

    // Short of a scope Member Audit asked for since: not a token error.
    // It's still read, and its pilot is asked once, gently, to register
    // it again.
    token(&h, CHRIBBA, "valid", None).await;
    assert_eq!(run(&h).await, 0);
    sqlx::query(
        "UPDATE core.plugins SET user_scopes = user_scopes || '{esi-wallet.read_character_wallet.v1}' \
         WHERE id = 'tether.member-audit'",
    )
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(run(&h).await, 0);
    assert!(!marked(&h, CHRIBBA).await);
    async fn prompts(h: &Harness) -> usize {
        tether_web::compliance::scope_prompts(&h.db).await.unwrap()
    }
    assert_eq!(prompts(&h).await, 1);
    assert_eq!(prompts(&h).await, 0);
    let (level, title, message): (String, String, String) = sqlx::query_as(
        "SELECT level, title, message FROM core.notifications WHERE account_id = $1 \
         AND title LIKE 'Member Audit: register again%'",
    )
    .bind(owner_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(level, "info");
    assert_eq!(title, "Member Audit: register again to allow more");
    assert_eq!(
        message,
        "Member Audit now also asks to read the character's wallet and journal. Register \
         Chribba for Member Audit again (Register Character) to allow it. Until then Member \
         Audit keeps reading everything else."
    );
    // Its token has every scope again: the prompt's mark clears.
    sqlx::query(
        "UPDATE core.plugins SET user_scopes = array_remove(user_scopes, \
         'esi-wallet.read_character_wallet.v1') WHERE id = 'tether.member-audit'",
    )
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(run(&h).await, 0);
    assert_eq!(prompts(&h).await, 0);
    let prompted: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT scopes_prompted FROM core.app_characters \
         WHERE plugin_id = 'tether.member-audit' AND character_id = $1",
    )
    .bind(CHRIBBA)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(prompted, None);

    // Switched off: nothing marked, nobody told.
    sqlx::query(
        "INSERT INTO core.settings (key, value) \
         VALUES ('notifications.member_audit_token_errors', 'false')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    token(&h, CHRIBBA, "revoked", Some("invalid_grant")).await;
    assert_eq!(run(&h).await, 0);
    assert!(!marked(&h, CHRIBBA).await);
    sqlx::query("DELETE FROM core.settings WHERE key = 'notifications.member_audit_token_errors'")
        .execute(&h.db)
        .await
        .unwrap();
    // Only while Member Audit is on.
    sqlx::query("UPDATE core.plugins SET enabled = false WHERE id = 'tether.member-audit'")
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(run(&h).await, 0);
    sqlx::query("UPDATE core.plugins SET enabled = true WHERE id = 'tether.member-audit'")
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(run(&h).await, 1);
    assert_eq!(notices(&h).await.len(), 3);

    // Registered by a pilot who holds none of Member Audit's permissions
    // any more (AA tells only users who may use it): the Mittani's token
    // is gone, and nobody is told until he holds one again.
    let mittani: i64 = sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(MITTANI)
        .fetch_one(&h.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO core.app_characters (plugin_id, character_id, registered_by) \
         VALUES ('tether.member-audit', $1, $2)",
    )
    .bind(MITTANI)
    .bind(mittani)
    .execute(&h.db)
    .await
    .unwrap();
    assert_eq!(run(&h).await, 0);
    assert!(!marked(&h, MITTANI).await);
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind("plugin.tether.member-audit.basic_access")
        .bind(mittani)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(run(&h).await, 1);
    assert!(marked(&h, MITTANI).await);
    let told = notices(&h).await;
    assert_eq!(told.len(), 4, "{told:?}");
    assert_eq!(told[3].0, mittani);
    assert_eq!(
        told[3].2,
        "Member Audit: Invalid or missing token for The Mittani"
    );
}

// ---- downloads (aa-memberaudit's data exports) --------------------------------

async fn install_files(h: &Harness, owner: &str) {
    let key = Key::new(8);
    let manifest = format!(
        "[plugin]\nid = \"acme.files\"\nname = \"Files\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\ndownloads = true\n\n\
         [permissions]\nview = \"See\"\nexports = \"Download\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(h, owner, &bytes, &key.sign(&bytes)).await;
}

async fn build_download(
    h: &Harness,
    rows: serde_json::Value,
    extra: &[(&str, &str)],
    as_page: bool,
) -> String {
    let mut query = vec![
        ("name".to_owned(), "wallet".to_owned()),
        ("title".to_owned(), "Wallet journal".to_owned()),
        ("permission".to_owned(), "exports".to_owned()),
        (
            "header".to_owned(),
            serde_json::json!(["date", "amount", "description"]).to_string(),
        ),
        ("rows".to_owned(), rows.to_string()),
    ];
    for (k, v) in extra {
        query.retain(|(key, _)| key != k);
        query.push(((*k).to_owned(), (*v).to_owned()));
    }
    run_probe(h, "acme.files", "download-build", query, as_page).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_offer_csv_downloads_the_host_writes(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    install_files(&h, &owner).await;
    let rows = serde_json::json!([
        ["2026-09-27", "-1500.50", "=HYPERLINK(\"x\")"],
        ["2026-09-26", "2000", "a, b"],
    ]);

    // Not while a page draws, and only for one of the app's permissions.
    let out = build_download(&h, rows.clone(), &[], true).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    let out = build_download(&h, rows.clone(), &[("permission", "admin")], false).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    let out = build_download(&h, serde_json::json!([["only one cell"]]), &[], false).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    assert_eq!(build_download(&h, rows.clone(), &[], false).await, "ok");
    assert_eq!(
        run_probe(&h, "acme.files", "download-files", Vec::new(), true).await,
        "wallet Wallet journal 2"
    );

    // The host's CSV: quoted, formulas defused, numbers kept.
    const CSV: &str = "date,amount,description\r\n\
                       2026-09-27,-1500.50,\"'=HYPERLINK(\"\"x\"\")\"\r\n\
                       2026-09-26,2000,\"a, b\"\r\n";
    let res = page(&h, "/plugins/acme.files/downloads/wallet", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body, CSV);
    assert_eq!(res.headers["content-type"], "text/csv; charset=utf-8");
    assert_eq!(
        res.headers["content-disposition"],
        "attachment; filename=\"wallet.csv\""
    );
    // Asked by htmx (a link the browser didn't take as a download): back
    // as a navigation, never its rows swapped into the page as HTML.
    let mut by_htmx = get("/plugins/acme.files/downloads/wallet", &[(SESSION, &owner)]);
    by_htmx
        .headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let res = send(&h.app, by_htmx).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.headers["hx-redirect"],
        "/plugins/acme.files/downloads/wallet"
    );
    assert!(res.body.is_empty(), "{}", res.body);
    // A build under way doesn't change what's served.
    let out = build_download(&h, serde_json::json!([]), &[("finish", "no")], false).await;
    assert_eq!(out, "ok");
    assert_eq!(
        page(&h, "/plugins/acme.files/downloads/wallet", &owner)
            .await
            .body,
        CSV
    );

    // A chain a newer build overtook is told to stop, and the file stays whole.
    let out = build_download(&h, rows.clone(), &[("stale", "yes")], false).await;
    assert_eq!(out, "err Error::Superseded");
    assert_eq!(
        page(&h, "/plugins/acme.files/downloads/wallet", &owner)
            .await
            .body,
        CSV
    );

    // Only for holders of its permission, to others as if missing;
    // signed out, to the login.
    let res = page(&h, "/plugins/acme.files/downloads/wallet", &pilot).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let signed_out = send(&h.app, get("/plugins/acme.files/downloads/wallet", &[])).await;
    assert_eq!(signed_out.location(), "/login");
    let pilot_account: i64 =
        sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
            .bind(MITTANI)
            .fetch_one(&h.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind("plugin.acme.files.exports")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    let res = page(&h, "/plugins/acme.files/downloads/wallet", &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = page(&h, "/plugins/acme.files/downloads/nothing", &owner).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action = 'plugin.download'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audited, 4);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_see_the_character_you_act_as(db: PgPool) {
    use tether_web::auth::ACTING_COOKIE;
    const MITTANI_ID: i64 = 443630591;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let owner = log_in_as(&h, "443630591:The Mittani", Some(&owner)).await;
    install(&h, &owner).await;
    let open = |path: &'static str, acting: Option<String>| {
        let (h, owner) = (&h, owner.clone());
        async move {
            let mut jar = vec![(SESSION, owner.as_str())];
            if let Some(a) = acting.as_deref() {
                jar.push((ACTING_COOKIE, a));
            }
            send(&h.app, get(&format!("/plugins/acme.esi/{path}"), &jar))
                .await
                .body
        }
    };
    // The main, until another is chosen.
    let body = open("acting", None).await;
    assert!(body.contains("id: 196379789"), "{body}");

    // Acting as the alt: apps see it through `acting`; the viewer's main,
    // and everything scoped by it, stays the main.
    let res = send(
        &h.app,
        form(
            "/profile/acting",
            &format!("character_id={MITTANI_ID}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let acting = res.cookie_value(ACTING_COOKIE);
    assert_eq!(acting, MITTANI_ID.to_string());
    let body = open("acting", Some(acting.clone())).await;
    assert!(body.contains(&format!("id: {MITTANI_ID}")), "{body}");
    let body = open("viewer", Some(acting.clone())).await;
    assert!(body.contains("main: Character { id: 196379789"), "{body}");
    // No watermark: the page shows no other pilot (it names the account's
    // main, `viewer.main`, on pages that do).
    assert!(!body.contains("Viewing as"), "{body}");
    let dashboard = send(
        &h.app,
        get("/dashboard", &[(SESSION, &owner), (ACTING_COOKIE, &acting)]),
    )
    .await
    .body;
    assert!(dashboard.contains("Acting as"), "{dashboard}");

    // Back to the main: the cookie goes, the way a browser accepts for a
    // `__Host-` cookie (Secure, Path=/).
    let res = send(
        &h.app,
        axum::http::Request::post("/profile/acting")
            .header(axum::http::header::ORIGIN, SITE)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(
                axum::http::header::COOKIE,
                format!("{SESSION}={owner}; {ACTING_COOKIE}={acting}"),
            )
            .body(axum::body::Body::from("character_id=196379789"))
            .unwrap(),
    )
    .await;
    let cleared = res
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|c| c.to_str().unwrap().to_owned())
        .find(|c| c.starts_with(&format!("{ACTING_COOKIE}=")))
        .unwrap_or_default();
    assert!(
        cleared.starts_with(&format!("{ACTING_COOKIE}=;"))
            && cleared.contains("Secure")
            && cleared.contains("Path=/"),
        "{cleared}"
    );

    // Only the account's own characters count: another's id is ignored.
    let body = open("acting", Some("1887431749".to_owned())).await;
    assert!(body.contains("id: 196379789"), "{body}");
    let res = send(
        &h.app,
        form("/profile/acting", "character_id=1887431749", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);

    // Logging out forgets it.
    let res = send(
        &h.app,
        axum::http::Request::post("/auth/logout")
            .header(axum::http::header::ORIGIN, SITE)
            .header(
                axum::http::header::COOKIE,
                format!("{SESSION}={owner}; {ACTING_COOKIE}={acting}"),
            )
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        res.headers.get_all("set-cookie").iter().any(|c| c
            .to_str()
            .unwrap()
            .starts_with(&format!("{ACTING_COOKIE}=;"))),
        "{:?}",
        res.headers
    );
}

// ---- notices (the notify interface) -------------------------------------------

async fn install_notices(h: &Harness, owner: &str) {
    let key = Key::new(9);
    let manifest = format!(
        "[plugin]\nid = \"acme.notes\"\nname = \"Notes\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nnotify = true\n\n\
         [permissions]\nview = \"See\"\napprove = \"Approve\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(h, owner, &bytes, &key.sign(&bytes)).await;
}

async fn account_of(h: &Harness, character: i64) -> i64 {
    sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn notices(h: &Harness, account: i64) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT level, title, message FROM core.notifications WHERE account_id = $1 ORDER BY id",
    )
    .bind(account)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_notify_only_their_own_audience(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    log_in_as(&h, "443630591:The Mittani", None).await;
    install_notices(&h, &owner).await;
    let owner_account = account_of(&h, 196379789).await;
    let pilot = account_of(&h, MITTANI).await;
    let to = |account: i64, title: &str| {
        vec![
            ("account".to_owned(), account.to_string()),
            ("title".to_owned(), title.to_owned()),
            ("message".to_owned(), "Your copy is ready.".to_owned()),
        ]
    };
    let before = notices(&h, pilot).await.len();

    // Not while a page draws.
    let out = run_probe(&h, "acme.notes", "notify-account", to(pilot, "Ready"), true).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");

    // Someone outside the app's audience isn't reached, nor told about.
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-account",
        to(pilot, "Ready"),
        false,
    )
    .await;
    assert_eq!(out, "ok false");
    assert_eq!(notices(&h, pilot).await.len(), before);

    // Once they hold one of its permissions: under the app's name, once
    // while unread.
    sqlx::query("INSERT INTO core.permission_grants (permission, account_id) VALUES ($1, $2)")
        .bind("plugin.acme.notes.view")
        .bind(pilot)
        .execute(&h.db)
        .await
        .unwrap();
    for _ in 0..2 {
        let out = run_probe(
            &h,
            "acme.notes",
            "notify-account",
            to(pilot, "Ready"),
            false,
        )
        .await;
        assert_eq!(out, "ok true");
    }
    let got = notices(&h, pilot).await;
    assert_eq!(got.len(), before + 1);
    assert_eq!(
        got.last().unwrap(),
        &(
            "success".to_owned(),
            "Notes: Ready".to_owned(),
            "Your copy is ready.".to_owned()
        )
    );

    // Plain text within limits.
    for title in ["", &"x".repeat(101)] {
        let out = run_probe(&h, "acme.notes", "notify-account", to(pilot, title), false).await;
        assert!(out.starts_with("err Error::Invalid"), "{title}: {out}");
    }

    // Holders of its own permission, but the one who acted. The owner
    // holds everything.
    let holders = |permission: &str, except: Option<i64>| {
        let mut q = vec![
            ("permission".to_owned(), permission.to_owned()),
            ("title".to_owned(), "New request".to_owned()),
            ("message".to_owned(), "Someone asked for a copy.".to_owned()),
        ];
        if let Some(e) = except {
            q.push(("except".to_owned(), e.to_string()));
        }
        q
    };
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-holders",
        holders("approve", None),
        false,
    )
    .await;
    assert_eq!(out, "ok 1");
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-holders",
        holders("approve", Some(owner_account)),
        false,
    )
    .await;
    assert_eq!(out, "ok 0");
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-holders",
        holders("admin", None),
        false,
    )
    .await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    assert!(
        notices(&h, owner_account)
            .await
            .iter()
            .any(|n| n.1 == "Notes: New request")
    );

    // An account hears from one app at most 20 times an hour (the two
    // "Ready" above count, the repeat too).
    let mut sent = 2;
    for n in 0..25 {
        let out = run_probe(
            &h,
            "acme.notes",
            "notify-account",
            to(pilot, &format!("Ready {n}")),
            false,
        )
        .await;
        if out == "ok true" {
            sent += 1;
        } else {
            assert_eq!(out, "ok false");
        }
    }
    assert_eq!(sent, 20);
    // Marked as the app's, and only its newest five are kept: an app
    // can't push the rest of an account's notices out.
    let kept: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT plugin_id FROM core.notifications WHERE account_id = $1 AND title LIKE 'Notes: %'",
    )
    .bind(pilot)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(kept, vec![Some("acme.notes".to_owned()); 5]);
    assert_eq!(notices(&h, pilot).await.len(), before + 5);
    let listed = page(&h, "/notifications", &owner).await.body;
    assert!(listed.contains("Notes: New request"), "{listed}");
    assert!(listed.contains("Sent by the app acme.notes"), "{listed}");

    // Only with the capability.
    install_files(&h, &owner).await;
    let out = run_probe(
        &h,
        "acme.files",
        "notify-account",
        to(owner_account, "Hi"),
        false,
    )
    .await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
}

/// Jay, 2026-10-07: an app may notify an account that submitted one of
/// its forms (an applicant, a requester), holding none of its permissions,
/// by the reference the host gave it while that pilot posted; nobody else.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn apps_notify_who_submitted_their_forms_by_reference(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    log_in_as(&h, "443630591:The Mittani", None).await;
    install_notices(&h, &owner).await;
    let pilot = account_of(&h, MITTANI).await;
    // The pilot posting a form holds none of the app's permissions (an
    // applicant on a signed-in page).
    let mut applicant = chribba_looking(&h).await;
    applicant.account_id = pilot;
    applicant.permissions.clear();
    let reference = |viewer, as_page| {
        run_probe_as(
            &h,
            "acme.notes",
            "notify-submitter-reference",
            Vec::new(),
            viewer,
            as_page,
        )
    };

    // Only for the pilot posting a form: not while a page draws, not
    // with nobody posting (a job's way).
    let out = reference(Some(applicant.clone()), true).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    let out = reference(None, false).await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM core.plugin_submitters")
            .fetch_one(&h.db)
            .await
            .unwrap(),
        0
    );

    // One per account and app, the same every time: random hex.
    let out = reference(Some(applicant.clone()), false).await;
    let token = out.strip_prefix("ok ").expect(&out).to_owned();
    assert_eq!(token.len(), 32, "{token}");
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{token}"
    );
    assert_eq!(
        reference(Some(applicant.clone()), false).await,
        format!("ok {token}")
    );

    let to = |reference: &str, title: &str| {
        vec![
            ("reference".to_owned(), reference.to_owned()),
            ("title".to_owned(), title.to_owned()),
            (
                "message".to_owned(),
                "Your application was accepted.".to_owned(),
            ),
        ]
    };
    // Not from a page.
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&token, "Accepted"),
        true,
    )
    .await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");
    // From a job or a recruiter's submit: reaches the applicant, who
    // holds none of the app's permissions.
    let before = notices(&h, pilot).await.len();
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&token, "Accepted"),
        false,
    )
    .await;
    assert_eq!(out, "ok true");
    let got = notices(&h, pilot).await;
    assert_eq!(got.len(), before + 1);
    assert_eq!(
        got.last().unwrap(),
        &(
            "info".to_owned(),
            "Notes: Accepted".to_owned(),
            "Your application was accepted.".to_owned()
        )
    );

    // A year after the last post it was asked for in, it reaches nobody;
    // asking again in a new post renews it.
    sqlx::query(
        "UPDATE core.plugin_submitters SET last_posted_at = now() - interval '366 days' \
         WHERE account_id = $1",
    )
    .bind(pilot)
    .execute(&h.db)
    .await
    .unwrap();
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&token, "Late"),
        false,
    )
    .await;
    assert_eq!(out, "ok false");
    assert_eq!(
        reference(Some(applicant.clone()), false).await,
        format!("ok {token}")
    );
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&token, "Late"),
        false,
    )
    .await;
    assert_eq!(out, "ok true");

    // A reference never made reaches nobody; something that isn't one is
    // refused unread.
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&"0".repeat(32), "Accepted"),
        false,
    )
    .await;
    assert_eq!(out, "ok false");
    for bad in [pilot.to_string(), format!("{token}'"), String::new()] {
        let out = run_probe(&h, "acme.notes", "notify-submitter", to(&bad, "X"), false).await;
        assert!(out.starts_with("err Error::Invalid"), "{bad}: {out}");
    }

    // Another app can't use it, even with the capability; its own
    // reference for the same account is another.
    let key = Key::new(10);
    let manifest = format!(
        "[plugin]\nid = \"acme.other\"\nname = \"Other\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nnotify = true\n\n\
         [permissions]\nview = \"See\"\n\n[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
        key.public()
    );
    let component = probe_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    let out = run_probe(
        &h,
        "acme.other",
        "notify-submitter",
        to(&token, "Hi"),
        false,
    )
    .await;
    assert_eq!(out, "ok false");
    let theirs = run_probe_as(
        &h,
        "acme.other",
        "notify-submitter-reference",
        Vec::new(),
        Some(applicant.clone()),
        false,
    )
    .await;
    assert_ne!(theirs, format!("ok {token}"));
    assert!(theirs.starts_with("ok "), "{theirs}");

    // Only with the capability.
    install_files(&h, &owner).await;
    let out = run_probe_as(
        &h,
        "acme.files",
        "notify-submitter-reference",
        Vec::new(),
        Some(applicant.clone()),
        false,
    )
    .await;
    assert!(out.starts_with("err Error::Invalid"), "{out}");

    // References go with the account.
    sqlx::query("DELETE FROM core.accounts WHERE id = $1")
        .bind(pilot)
        .execute(&h.db)
        .await
        .unwrap();
    let out = run_probe(
        &h,
        "acme.notes",
        "notify-submitter",
        to(&token, "Again"),
        false,
    )
    .await;
    assert_eq!(out, "ok false");
}
