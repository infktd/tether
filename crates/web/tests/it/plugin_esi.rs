//! Plugin ESI, identity and Discord (F16, N8, N10): user scopes only for
//! registered characters on compliant accounts, data sources offered and
//! approved, every call checked and logged, the host choosing the ids, and
//! Discord only to assigned channels, pinging only state roles.

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
         [permissions]\nview = \"See\"\nmanage = \"Manage\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n",
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
    for uri in ["/register/start", "/apps/acme.esi/owners/add"] {
        let res = send(&h.app, form(uri, "", "no-such-session")).await;
        assert_eq!(res.location(), "/login", "{uri}");
    }
    // A plugin's admin actions need admin.plugins.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/acme.esi/sources/{CHRIBBA}/approve"),
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
    let admin = page(&h, "/admin/plugins/acme.esi", &owner).await.body;
    assert!(
        admin.contains("Recent data access") && admin.contains(SKILLS),
        "{admin}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plain_login_keeps_the_scopes_a_character_registered(db: PgPool) {
    let (h, owner) = member_with_plugin(db).await;
    let (_, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    // Signing in again asks for nothing, and mustn't replace the richer
    // token.
    let _ = owner;
    let owner = log_in_as(&h, "196379789:Chribba", None).await;
    assert!(h.sso.last_requested.lock().unwrap().is_empty());
    assert_eq!(state_of(&h, &owner).await, "Member");
    let out = esi(&h, "character-skills", ("character", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");
    // The profile shows what was granted and what uses it.
    let profile = page(&h, "/dashboard", &owner).await.body;
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

    // Add owner is on the app's own page (AA's), with its owners.
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(
        main.contains(r#"action="/apps/acme.esi/owners/add""#),
        "{main}"
    );
    assert!(main.contains("No owners yet"), "{main}");
    let (asked, owner) = grant(&h, &owner, "/apps/acme.esi/owners/add", "196379789:Chribba").await;
    assert!(asked.contains(&MINING.to_owned()), "{asked:?}");
    let main = page(&h, "/plugins/acme.esi", &owner).await.body;
    assert!(main.contains("waiting for an admin"), "{main}");
    assert!(
        main.contains(&format!("/apps/acme.esi/owners/{CHRIBBA}/approve")),
        "{main}"
    );
    // Not on the Dashboard any more.
    let dashboard = page(&h, "/dashboard", &owner).await.body;
    assert!(!dashboard.contains("corporation data"), "{dashboard}");
    assert!(!dashboard.contains("/apps/acme.esi"), "{dashboard}");
    // Offered is not approved.
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert_eq!(out, "err Error::NotADataSource");
    assert_eq!(probe(&h, "sources", &[]).await, "[]");

    // Approved there, and back on the app's page.
    let res = send(
        &h.app,
        form(
            &format!("/apps/acme.esi/owners/{CHRIBBA}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/plugins/acme.esi");
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
async fn only_add_owner_holders_offer_and_only_admins_approve(db: PgPool) {
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

    // The app's manage permission may: its holder offers their own
    // character and sees only their own, with no Approve.
    grant_to_guests(&h, &owner, "manage").await;
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
    for action in ["approve", "remove"] {
        let res = send(
            &h.app,
            form(
                &format!("/apps/acme.esi/owners/{CHRIBBA}/{action}"),
                "",
                &pilot,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{action}");
    }
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
async fn the_character_viewer_reads_only_registered_members_with_the_scope(db: PgPool) {
    let (h, owner) = member_with_viewer(db).await;
    mount_viewer_esi(&h).await;
    let mail = |who: (&'static str, i64)| {
        let h = &h;
        async move { esi_with(h, "character-mail-body", who, &[("mail_id", "77")]).await }
    };

    // Member requires the app's scopes; Chribba hasn't granted them yet.
    assert_eq!(
        mail(("character", CHRIBBA)).await,
        "err Error::NotRegistered"
    );
    let (asked, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
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
    // A Guest's character, even one whose token has the scope.
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
    let (_, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
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
    let (_, owner) = grant(&h, &owner, "/register/start", "443630591:The Mittani").await;
    await_runs(&h.db, 0).await;
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert_eq!(run_now_audits(&h.db).await.len(), 1);
    // Eleven minutes after, it would have run: another alt shows it.
    finish_runs(&h.db, "11 minutes").await;
    let owner = log_in_as(&h, "1887431749:gigX", Some(&owner)).await;
    let (_, owner) = grant(&h, &owner, "/register/start", "1887431749:gigX").await;
    await_runs(&h.db, 2).await;
    assert_eq!(
        queued_plugin_runs(&h.db).await,
        std::slice::from_ref(&schedule)
    );

    // Approving a data source runs them too, as the approving admin, a
    // minute after the last run (an admin's gap).
    finish_runs(&h.db, "61 seconds").await;
    let (_, owner) = grant(
        &h,
        &owner,
        "/profile/plugins/acme.esi/offer",
        "196379789:Chribba",
    )
    .await;
    assert!(
        queued_plugin_runs(&h.db).await.is_empty(),
        "an offer isn't approval"
    );
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/acme.esi/sources/{CHRIBBA}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
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
        serde_json::json!({ "reason": "data_source_approved", "character_id": CHRIBBA })
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sync_that_cannot_be_queued_fails_neither_registering_nor_approving(db: PgPool) {
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
    let (_, owner) = grant(&h, &owner, "/register/start", "196379789:Chribba").await;
    let out = esi_with(
        &h,
        "character-mail-body",
        ("character", CHRIBBA),
        &[("mail_id", "77")],
    )
    .await;
    assert!(out.starts_with("ok"), "{out}");
    assert!(page(&h, "/register", &owner).await.body.contains("Chribba"));

    // Approved all the same.
    let (_, owner) = grant(
        &h,
        &owner,
        "/profile/plugins/acme.esi/offer",
        "196379789:Chribba",
    )
    .await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/acme.esi/sources/{CHRIBBA}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let out = esi(&h, "corporation-mining-extractions", ("source", CHRIBBA)).await;
    assert!(out.starts_with("ok"), "{out}");

    // Nothing queued, and nothing claimed to be: the audit goes with the
    // run it records.
    await_runs(&h.db, 0).await;
    assert!(queued_plugin_runs(&h.db).await.is_empty());
    assert!(run_now_audits(&h.db).await.is_empty());
}
