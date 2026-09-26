//! Discord setup, role mappings and linking, against a mock Discord serving
//! the fixtures in tests/fixtures/discord/.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;
use wiremock::matchers::{body_json, body_string_contains, header as has_header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LINK: &str = "__Host-tether_discord";
const BOT: &str = "111111111111111111";
const GUILD: &str = "222222222222222222";
const USER: &str = "333333333333333333";
const MEMBER_ROLE: &str = "500000000000000003";
const BLUE_ROLE: &str = "500000000000000004";
const SETTINGS: &str = "application_id=111111111111111111&guild_id=222222222222222222\
                        &client_secret=client-secret-value&bot_token=bot-token-value";

fn form(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn page(h: &Harness, uri: &str, token: &str) -> Res {
    send(&h.app, get(uri, &[(SESSION, token)])).await
}

fn fixture(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/../../tests/fixtures/discord/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

/// The bot's view of the server: what the settings check and role mapping ask.
async fn mount_bot(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .and(has_header("authorization", "Bot bot-token-value"))
        .respond_with(ok("bot_user"))
        .mount(server)
        .await;
    for (route, name) in [
        (format!("/api/v10/guilds/{GUILD}"), "guild"),
        (format!("/api/v10/guilds/{GUILD}/roles"), "roles"),
        (
            format!("/api/v10/guilds/{GUILD}/members/{BOT}"),
            "bot_member",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ok(name))
            .mount(server)
            .await;
    }
}

/// A member approving at Discord: token exchange, who they are, revocation.
async fn mount_member_oauth(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains(
            "redirect_uri=https%3A%2F%2Ftether.test%2Fdiscord%2Fcallback",
        ))
        .respond_with(ok("token"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .and(has_header(
            "authorization",
            "Bearer user-access-token-fixture",
        ))
        .respond_with(ok("member_user"))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token/revoke"))
        .and(body_string_contains("token=user-access-token-fixture"))
        .respond_with(ResponseTemplate::new(200))
        .named("revoke")
        .mount(server)
        .await;
}

async fn owner_and_pilot(h: &Harness) -> (String, String) {
    let owner = log_in_owner(h, "196379789:Chribba").await;
    let pilot = log_in_as(h, "443630591:The Mittani", None).await;
    (owner, pilot)
}

/// Guests don't join the server through Tether; other states do.
async fn make_member(h: &Harness, token: &str) {
    let account = me(h, token).await["account_id"].as_i64().unwrap();
    sqlx::query("UPDATE core.accounts SET state_id = 1 WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
}

/// Owner and pilot (both Member), with Discord set up.
async fn set_up(h: &Harness) -> (String, String) {
    mount_bot(&h.discord_server).await;
    let (owner, pilot) = owner_and_pilot(h).await;
    make_member(h, &owner).await;
    make_member(h, &pilot).await;
    let saved = send(&h.app, form("/admin/discord", SETTINGS, &owner)).await;
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    // Nickname syncing (on by default, as AA) off: tests that want it
    // turn it on.
    let off = send(&h.app, form("/admin/discord/options", "", &owner)).await;
    assert_eq!(off.status, StatusCode::SEE_OTHER, "{}", off.body);
    (owner, pilot)
}

/// Turns nickname syncing on with a format for Member.
async fn name_format(h: &Harness, owner: &str, format: &str) -> Res {
    send(
        &h.app,
        form("/admin/discord/options", "sync_names=on", owner),
    )
    .await;
    let body = format!(
        "state_id=1&format={}",
        format
            .replace('%', "%25")
            .replace('[', "%5B")
            .replace(']', "%5D")
            .replace('{', "%7B")
            .replace('}', "%7D")
            .replace(' ', "+")
            .replace(':', "%3A")
    );
    send(&h.app, form("/admin/discord/names", &body, owner)).await
}

async fn map(h: &Harness, owner: &str, role: &str, grantee: &str) -> Res {
    send(
        &h.app,
        form(
            "/admin/discord/mappings",
            &format!("role_id={role}&grantee={grantee}"),
            owner,
        ),
    )
    .await
}

/// Starts linking; returns the state Discord would echo and the link cookie.
async fn start_link(h: &Harness, token: &str) -> (String, String) {
    let res = send(&h.app, form("/services/discord/link", "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location = res.location().to_owned();
    assert!(
        location.starts_with("https://discord.com/oauth2/authorize?"),
        "{location}"
    );
    assert_eq!(query_param(&location, "client_id"), BOT);
    assert_eq!(query_param(&location, "scope"), "identify%20guilds.join");
    let cookie = res.set_cookie(LINK).unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("Secure"),
        "{cookie}"
    );
    (
        query_param(&location, "state").to_owned(),
        res.cookie_value(LINK),
    )
}

async fn callback(h: &Harness, state: &str, cookies: &[(&str, &str)]) -> Res {
    send(
        &h.app,
        get(
            &format!("/discord/callback?code=the-code&state={state}"),
            cookies,
        ),
    )
    .await
}

async fn jobs_of_kind(db: &PgPool, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT payload FROM core.jobs WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_discord_page_needs_the_permission(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;

    let signed_out = send(&h.app, get("/admin/discord", &[])).await;
    assert_eq!(signed_out.location(), "/login");
    assert_eq!(
        page(&h, "/admin/discord", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    let posted = send(&h.app, form("/admin/discord", SETTINGS, &pilot)).await;
    assert_eq!(posted.status, StatusCode::FORBIDDEN);

    let res = page(&h, "/admin/discord", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("https://tether.test/discord/callback"));
    assert!(
        res.body.contains(r#"href="/admin/discord""#),
        "sidebar link"
    );
    // No bot invite link until the ids are saved.
    assert!(!res.body.contains("scope=bot"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn settings_are_checked_with_discord_then_stored_encrypted(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;

    // Discord rejects the token: nothing is saved.
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"code": 0, "message": "401: Unauthorized"})),
        )
        .up_to_n_times(1)
        .mount(&h.discord_server)
        .await;
    let refused = send(&h.app, form("/admin/discord", SETTINGS, &owner)).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(
        refused.body.contains("Discord rejected the bot token"),
        "{}",
        refused.body
    );
    assert!(!refused.body.contains("bot-token-value"));
    assert!(
        tether_discord::store::load(&h.db, &h.key)
            .await
            .unwrap()
            .is_none()
    );

    let missing = send(
        &h.app,
        form(
            "/admin/discord",
            "application_id=111111111111111111&guild_id=222222222222222222",
            &owner,
        ),
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert!(missing.body.contains("Enter the client secret."));

    mount_bot(&h.discord_server).await;
    let saved = send(&h.app, form("/admin/discord", SETTINGS, &owner)).await;
    assert_eq!(saved.location(), "/admin/discord");

    let res = page(&h, "/admin/discord", &owner).await;
    assert!(
        res.body.contains("Example Miner&#39;s Alliance"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Saved; leave blank to keep"));
    assert!(res.body.contains("scope=bot"), "invite link");
    assert!(!res.body.contains("bot-token-value") && !res.body.contains("client-secret-value"));

    // Only sealed bytes in the database, and no secrets in the audit log.
    let sealed: Vec<Vec<u8>> = sqlx::query_scalar("SELECT sealed FROM core.secrets")
        .fetch_all(&h.db)
        .await
        .unwrap();
    assert_eq!(sealed.len(), 2);
    for bytes in &sealed {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("bot-token-value") && !text.contains("client-secret-value"));
    }
    let audit: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'discord.settings'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audit["bot_token_changed"], true);
    assert!(!audit.to_string().contains("token-value"));

    // Saving again with blank secrets keeps them.
    let resaved = send(
        &h.app,
        form(
            "/admin/discord",
            "application_id=111111111111111111&guild_id=222222222222222222&client_secret=&bot_token=",
            &owner,
        ),
    )
    .await;
    assert_eq!(resaved.status, StatusCode::SEE_OTHER, "{}", resaved.body);
    let config = tether_discord::store::load(&h.db, &h.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config.bot_token.expose(), "bot-token-value");
    assert_eq!(config.client_secret.expose(), "client-secret-value");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn roles_are_mapped_only_when_the_bot_can_give_them(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;

    assert_eq!(
        map(&h, &owner, MEMBER_ROLE, "state:1").await.status,
        StatusCode::SEE_OTHER
    );
    let listed = page(&h, "/admin/discord", &owner).await;
    assert!(listed.body.contains("State: Member"), "{}", listed.body);
    // Roles the bot can't give are offered, but disabled.
    assert!(listed.body.contains("Director (has Administrator)"));
    assert!(listed.body.contains("Tether (managed by an integration)"));

    let director = map(&h, &owner, "500000000000000001", "state:1").await;
    assert_eq!(director.status, StatusCode::BAD_REQUEST);
    assert!(director.body.contains("Administrator"));
    let managed = map(&h, &owner, "500000000000000002", "state:1").await;
    assert_eq!(managed.status, StatusCode::BAD_REQUEST);
    assert!(
        managed.body.contains("The bot can&#39;t give Tether"),
        "{}",
        managed.body
    );
    let unknown = map(&h, &owner, "123", "state:1").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let again = map(&h, &owner, MEMBER_ROLE, "state:1").await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let no_group = map(&h, &owner, MEMBER_ROLE, "group:999").await;
    assert_eq!(no_group.status, StatusCode::NOT_FOUND);
    let pilot_try = map(&h, &pilot, BLUE_ROLE, "state:3").await;
    assert_eq!(pilot_try.status, StatusCode::FORBIDDEN);

    let id: i64 = sqlx::query_scalar("SELECT id FROM core.discord_role_mappings")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let removed = send(
        &h.app,
        form(&format!("/admin/discord/mappings/{id}/remove"), "", &owner),
    )
    .await;
    assert_eq!(removed.location(), "/admin/discord");
    assert!(
        !page(&h, "/admin/discord", &owner)
            .await
            .body
            .contains("State: Member")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn linking_adds_the_member_to_the_server_with_their_roles(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    // The pilot is Member, and in a group.
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    let group = send(
        &h.app,
        form(
            "/admin/groups",
            "name=Miners&description=&internal=on&hidden=on",
            &owner,
        ),
    )
    .await
    .location()
    .to_owned();
    send(
        &h.app,
        form(&format!("{group}/members"), "character=the+mittani", &owner),
    )
    .await;
    let group_id = group.rsplit('/').next().unwrap();
    map(&h, &owner, BLUE_ROLE, &format!("group:{group_id}")).await;

    let profile = page(&h, "/services", &pilot).await;
    assert!(profile.body.contains("Link Discord"), "{}", profile.body);

    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(has_header("authorization", "Bot bot-token-value"))
        .and(body_json(serde_json::json!({
            "access_token": "user-access-token-fixture",
            "roles": [MEMBER_ROLE, BLUE_ROLE],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&h.discord_server)
        .await;

    let (state, browser) = start_link(&h, &pilot).await;
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/services");
    assert!(res.set_cookie(LINK).unwrap().contains("Max-Age=0"));

    let profile = page(&h, "/services", &pilot).await;
    assert!(profile.body.contains("Unpercieved"), "{}", profile.body);
    assert!(profile.body.contains("Unlink"));
    let audit: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'discord.link'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audit["discord_user_id"], USER);
    assert_eq!(audit["roles"], 2);
    assert!(!audit.to_string().contains("access-token"));

    // The token was revoked, and the state can't be used twice.
    let revoked = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v10/oauth2/token/revoke")
        .count();
    assert_eq!(revoked, 1);
    let replay = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_already_in_the_server_gets_the_roles_added(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.discord_server)
        .await;

    let (state, browser) = start_link(&h, &pilot).await;
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.location(), "/services", "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_callback_only_finishes_for_the_browser_and_account_that_started(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    mount_member_oauth(&h.discord_server).await;
    let (state, browser) = start_link(&h, &pilot).await;

    // Another browser (no link cookie, or someone else's).
    let no_cookie = callback(&h, &state, &[(SESSION, &pilot)]).await;
    assert_eq!(no_cookie.status, StatusCode::BAD_REQUEST);
    let wrong_cookie = callback(&h, &state, &[(SESSION, &pilot), (LINK, "0000")]).await;
    assert_eq!(wrong_cookie.status, StatusCode::BAD_REQUEST);
    // The right browser, but signed in as someone else by now.
    let other_account = callback(&h, &state, &[(SESSION, &owner), (LINK, &browser)]).await;
    assert_eq!(other_account.status, StatusCode::BAD_REQUEST);
    assert!(other_account.body.contains("expired or was already used"));

    let cancelled = send(
        &h.app,
        get(
            "/discord/callback?error=access_denied&state=x",
            &[(SESSION, &pilot)],
        ),
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::BAD_REQUEST);
    assert!(cancelled.body.contains("Linking was cancelled."));

    // Discord was never asked to exchange anything.
    let exchanged = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v10/oauth2/token")
        .count();
    assert_eq!(exchanged, 0);
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM core.discord_links")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(links, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_discord_account_links_to_one_pilot_only(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;

    let (state, browser) = start_link(&h, &owner).await;
    let taken = callback(&h, &state, &[(SESSION, &owner), (LINK, &browser)]).await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert!(taken.body.contains("linked to another pilot"));
    let revoked = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v10/oauth2/token/revoke")
        .count();
    assert_eq!(revoked, 2, "revoked even when refused");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unlinking_queues_taking_the_roles_away(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    map(&h, &owner, BLUE_ROLE, "state:2").await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;

    let res = send(&h.app, form("/services/discord/unlink", "", &pilot)).await;
    assert_eq!(res.location(), "/services");
    assert!(
        page(&h, "/services", &pilot)
            .await
            .body
            .contains("Link Discord")
    );
    let jobs = jobs_of_kind(&h.db, "discord.remove_member").await;
    assert_eq!(
        jobs,
        [serde_json::json!({ "discord_user_id": 333_333_333_333_333_333_i64 })]
    );

    // The job removes them from the server (AA).
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    tether_web::discord::remove_member(&h.db, &h.key, &h.discord, 333_333_333_333_333_333, true)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_the_bot_may_not_kick_loses_tethers_roles_instead(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    send(&h.app, form("/services/discord/unlink", "", &pilot)).await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
            ),
        )
        .expect(1)
        .mount(&h.discord_server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    tether_web::discord::remove_member(&h.db, &h.key, &h.discord, 333_333_333_333_333_333, true)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn stripping_skips_someone_who_linked_again(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&h.discord_server)
        .await;
    for _ in 0..2 {
        let (state, browser) = start_link(&h, &pilot).await;
        callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
        send(&h.app, form("/services/discord/unlink", "", &pilot)).await;
    }
    let (state, browser) = start_link(&h, &pilot).await;
    callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    // Unlinked twice, linked again: both queued strips are no-ops now.
    assert_eq!(jobs_of_kind(&h.db, "discord.remove_member").await.len(), 2);
    tether_web::discord::remove_member(&h.db, &h.key, &h.discord, 333_333_333_333_333_333, true)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn deleting_an_account_queues_taking_its_roles_away(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = set_up(&h).await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    let account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    sqlx::query("DELETE FROM core.accounts WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(jobs_of_kind(&h.db, "discord.remove_member").await.len(), 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_profile_has_no_discord_card_until_it_is_set_up(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = owner_and_pilot(&h).await;
    let profile = page(&h, "/services", &pilot).await;
    assert!(!profile.body.contains("Link Discord"));
    let link = send(&h.app, form("/services/discord/link", "", &pilot)).await;
    assert_eq!(link.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn guests_cannot_join_the_server(db: PgPool) {
    let h = harness(db, true).await;
    mount_bot(&h.discord_server).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    send(&h.app, form("/admin/discord", SETTINGS, &owner)).await;

    let profile = page(&h, "/services", &pilot).await;
    assert!(profile.body.contains("Your access doesn't include Discord"));
    assert!(!profile.body.contains("Link Discord"));
    let refused = send(&h.app, form("/services/discord/link", "", &pilot)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    // Started as Member, dropped to Guest before coming back from Discord.
    make_member(&h, &pilot).await;
    mount_member_oauth(&h.discord_server).await;
    let (state, browser) = start_link(&h, &pilot).await;
    let account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    sqlx::query("UPDATE core.accounts SET state_id = 3 WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let joins = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "PUT")
        .count();
    assert_eq!(joins, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn moderation_roles_never_go_to_guest_or_open_groups(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = set_up(&h).await;
    const FC: &str = "500000000000000006";
    let open = send(
        &h.app,
        form("/admin/groups", "name=Anyone&description=&open=on", &owner),
    )
    .await
    .location()
    .rsplit('/')
    .next()
    .unwrap()
    .to_owned();

    let to_guest = map(&h, &owner, FC, "state:3").await;
    assert_eq!(to_guest.status, StatusCode::BAD_REQUEST);
    assert!(
        to_guest.body.contains("moderation or server-management"),
        "{}",
        to_guest.body
    );
    let to_open = map(&h, &owner, FC, &format!("group:{open}")).await;
    assert_eq!(to_open.status, StatusCode::BAD_REQUEST);
    // Plain roles can go to anyone; moderation roles to Members.
    assert_eq!(
        map(&h, &owner, BLUE_ROLE, "state:3").await.status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        map(&h, &owner, FC, "state:1").await.status,
        StatusCode::SEE_OTHER
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_role_that_became_too_powerful_is_not_handed_out(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    map(&h, &owner, BLUE_ROLE, "state:1").await;

    // Someone gives the Blue role Administrator in Discord afterwards.
    let mut roles = fixture("roles");
    for role in roles.as_array_mut().unwrap() {
        if role["id"] == BLUE_ROLE {
            role["permissions"] = "8".into();
        }
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/roles")))
        .respond_with(ResponseTemplate::new(200).set_body_json(roles))
        .with_priority(1)
        .mount(&h.discord_server)
        .await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(body_json(serde_json::json!({
            "access_token": "user-access-token-fixture",
            "roles": [MEMBER_ROLE],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.location(), "/services", "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_failed_join_leaves_no_link(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = set_up(&h).await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
            ),
        )
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.status, StatusCode::BAD_GATEWAY);
    assert!(
        res.body.contains("isn&#39;t allowed to add you"),
        "{}",
        res.body
    );
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM core.discord_links")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(links, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_partial_join_is_undone_and_its_roles_queued_for_removal(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = set_up(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    map(&h, &owner, BLUE_ROLE, "state:1").await;
    mount_member_oauth(&h.discord_server).await;
    // Already in the server: the first role is added, the second refused.
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .mount(&h.discord_server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{BLUE_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(502))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(&h, &pilot).await;
    let res = callback(&h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.status, StatusCode::BAD_GATEWAY, "{}", res.body);

    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM core.discord_links")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(links, 0);
    // They were in the server before: only Tether's roles go, no kick.
    assert_eq!(
        jobs_of_kind(&h.db, "discord.remove_member").await,
        [serde_json::json!({ "discord_user_id": 333_333_333_333_333_333_i64, "kick": false })]
    );
    let undone: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'discord.unlink'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(undone["reason"], "joining the server failed");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn starting_again_replaces_the_pending_link(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = set_up(&h).await;
    for _ in 0..5 {
        start_link(&h, &pilot).await;
    }
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM core.discord_link_attempts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(pending, 1);
}

// ---- role sync and nicknames ----------------------------------------------

fn sync_context(h: &Harness) -> tether_web::discord_sync::SyncContext {
    tether_web::discord_sync::SyncContext {
        db: h.db.clone(),
        key: h.key.clone(),
        discord: h.discord.clone(),
        esi: h.esi.clone(),
    }
}

/// Links the pilot (a Member) with a join Discord accepts.
async fn linked_pilot(h: &Harness) -> (String, String, i64) {
    let (owner, pilot) = set_up(h).await;
    mount_member_oauth(&h.discord_server).await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .mount(&h.discord_server)
        .await;
    let (state, browser) = start_link(h, &pilot).await;
    let res = callback(h, &state, &[(SESSION, &pilot), (LINK, &browser)]).await;
    assert_eq!(res.location(), "/services", "{}", res.body);
    let account = me(h, &pilot).await["account_id"].as_i64().unwrap();
    (owner, pilot, account)
}

/// The member as Discord has them now.
async fn mount_member(h: &Harness, roles: &[&str], nick: Option<&str>) {
    let mut member = fixture("added_member");
    member["roles"] = serde_json::json!(roles);
    member["nick"] = serde_json::json!(nick);
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(member))
        .mount(&h.discord_server)
        .await;
}

async fn mount_role_edits(h: &Harness) {
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(wiremock::matchers::path_regex(format!(
                "^/api/v10/guilds/{GUILD}/members/{USER}/roles/[0-9]+$"
            )))
            .respond_with(ResponseTemplate::new(204))
            .mount(&h.discord_server)
            .await;
    }
}

async fn role_edits(h: &Harness) -> Vec<(String, String)> {
    h.discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().contains("/roles/"))
        .map(|r| {
            (
                r.method.to_string(),
                r.url.path().rsplit('/').next().unwrap().to_owned(),
            )
        })
        .collect()
}

async fn clear_jobs(db: &PgPool) {
    sqlx::query("DELETE FROM core.jobs")
        .execute(db)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn changes_to_a_linked_member_queue_one_sync(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    clear_jobs(&h.db).await;

    // State changes, twice: one sync waits, not two.
    for state_id in [2_i64, 3] {
        sqlx::query("UPDATE core.accounts SET state_id = $2 WHERE id = $1")
            .bind(account)
            .bind(state_id)
            .execute(&h.db)
            .await
            .unwrap();
    }
    let expected = [serde_json::json!({ "account_id": account })];
    assert_eq!(jobs_of_kind(&h.db, "discord.sync_member").await, expected);

    // Group membership.
    clear_jobs(&h.db).await;
    let group = send(
        &h.app,
        form(
            "/admin/groups",
            "name=Miners&description=&internal=on&hidden=on",
            &owner,
        ),
    )
    .await
    .location()
    .to_owned();
    send(
        &h.app,
        form(&format!("{group}/members"), "character=the+mittani", &owner),
    )
    .await;
    assert_eq!(jobs_of_kind(&h.db, "discord.sync_member").await, expected);

    // The main moves corporation (the nickname shows its ticker).
    clear_jobs(&h.db).await;
    sqlx::query("UPDATE core.characters SET corporation_id = 98133756 WHERE id = 443630591")
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(jobs_of_kind(&h.db, "discord.sync_member").await, expected);

    // Unlinked accounts don't queue anything.
    clear_jobs(&h.db).await;
    let owner_account = me(&h, &owner).await["account_id"].as_i64().unwrap();
    sqlx::query("UPDATE core.accounts SET state_id = 2 WHERE id = $1")
        .bind(owner_account)
        .execute(&h.db)
        .await
        .unwrap();
    assert!(jobs_of_kind(&h.db, "discord.sync_member").await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn syncing_gives_and_takes_managed_roles_and_leaves_others_alone(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    map(&h, &owner, BLUE_ROLE, "state:2").await;
    // Has Blue (managed, no longer due) and a role Tether doesn't manage.
    mount_member(&h, &[BLUE_ROLE, "700000000000000000"], None).await;
    mount_role_edits(&h).await;

    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    let mut edits = role_edits(&h).await;
    edits.sort();
    assert_eq!(
        edits,
        [
            ("DELETE".to_owned(), BLUE_ROLE.to_owned()),
            ("PUT".to_owned(), MEMBER_ROLE.to_owned()),
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_member_who_left_the_alliance_is_removed_from_the_server(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    mount_member(&h, &[MEMBER_ROLE], None).await;
    mount_role_edits(&h).await;
    sqlx::query("UPDATE core.accounts SET state_id = 3 WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();

    clear_jobs(&h.db).await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    // Guest has no Discord access: unlinked, and the queued job removes
    // them from the server (AA), rather than roles being edited.
    assert!(role_edits(&h).await.is_empty());
    assert_eq!(jobs_of_kind(&h.db, "discord.remove_member").await.len(), 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_role_whose_mapping_was_removed_is_taken_back(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM core.discord_role_mappings")
        .fetch_one(&h.db)
        .await
        .unwrap();
    clear_jobs(&h.db).await;
    send(
        &h.app,
        form(&format!("/admin/discord/mappings/{id}/remove"), "", &owner),
    )
    .await;
    let queued = jobs_of_kind(&h.db, "discord.sync_all").await;
    assert_eq!(
        queued,
        [serde_json::json!({ "removed_role_id": 500_000_000_000_000_003_i64 })]
    );

    // sync_all fans out one sync per linked member, carrying the role.
    assert_eq!(
        tether_web::discord_sync::sync_all(&h.db, Some(500_000_000_000_000_003))
            .await
            .unwrap(),
        1
    );
    let member_jobs = jobs_of_kind(&h.db, "discord.sync_member").await;
    assert_eq!(
        member_jobs,
        [
            serde_json::json!({ "account_id": account, "removed_role_ids": [500_000_000_000_000_003_i64] })
        ]
    );

    mount_member(&h, &[MEMBER_ROLE], None).await;
    mount_role_edits(&h).await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[500_000_000_000_000_003],
    )
    .await
    .unwrap();
    assert_eq!(
        role_edits(&h).await,
        [("DELETE".to_owned(), MEMBER_ROLE.to_owned())]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn nicknames_follow_the_name_formatter(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;

    let bad = name_format(&h, &owner, "[{corp}] {name}").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert!(bad.body.contains("isn&#39;t a field"), "{}", bad.body);

    clear_jobs(&h.db).await;
    let saved = name_format(&h, &owner, "[{corp_ticker}] {character_name:.20}").await;
    assert_eq!(saved.location(), "/admin/discord");
    assert!(!jobs_of_kind(&h.db, "discord.sync_all").await.is_empty());
    assert!(
        page(&h, "/admin/discord", &owner)
            .await
            .body
            .contains(r#"value="[{corp_ticker}] {character_name:.20}""#)
    );

    // The Mittani's corporation is the State War Academy (SWA).
    let corp = std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/esi/corporations_1000167.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/corporations/1000167"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(corp, "application/json"))
        .expect(1)
        .mount(&h.esi_server)
        .await;
    mount_member(&h, &[], None).await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(body_json(
            serde_json::json!({ "nick": "[SWA] The Mittani" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    let ctx = sync_context(&h);
    tether_web::discord_sync::sync_member(&ctx, tether_db::accounts::AccountId(account), &[])
        .await
        .unwrap();
    // The ticker is cached for the next sync.
    let ticker: String =
        sqlx::query_scalar("SELECT ticker FROM core.entity_tickers WHERE id = 1000167")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(ticker, "SWA");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn syncing_someone_not_in_the_server_does_nothing(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({"code": 10007, "message": "Unknown Member"})),
        )
        .mount(&h.discord_server)
        .await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    assert!(role_edits(&h).await.is_empty());
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_role_held_back_as_too_powerful_is_not_taken_either(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    const FC: &str = "500000000000000006";
    map(&h, &owner, FC, "state:1").await;
    // A Discord admin gives Fleet Commander Administrator afterwards.
    let mut roles = fixture("roles");
    for role in roles.as_array_mut().unwrap() {
        if role["id"] == FC {
            role["permissions"] = "8".into();
        }
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/roles")))
        .respond_with(ResponseTemplate::new(200).set_body_json(roles))
        .with_priority(1)
        .mount(&h.discord_server)
        .await;
    mount_member(&h, &[FC], None).await;
    mount_role_edits(&h).await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    assert!(role_edits(&h).await.is_empty(), "neither given nor taken");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn roles_sync_even_when_esi_is_down(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    name_format(&h, &owner, "[{corp_ticker}] {character_name}").await;
    Mock::given(method("GET"))
        .and(path("/corporations/1000167"))
        .respond_with(
            ResponseTemplate::new(503).set_body_raw(r#"{"error":"downtime"}"#, "application/json"),
        )
        .mount(&h.esi_server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("added_member")))
        .expect(0)
        .mount(&h.discord_server)
        .await;
    mount_member(&h, &[], None).await;
    mount_role_edits(&h).await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        role_edits(&h).await,
        [("PUT".to_owned(), MEMBER_ROLE.to_owned())]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_refused_takeback_is_retried_not_counted_as_done(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot, _) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    name_format(&h, &owner, "{character_name}").await;
    Mock::given(method("DELETE"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
            ),
        )
        .mount(&h.discord_server)
        .await;
    // Unlinking clears the Tether nickname too.
    Mock::given(method("PATCH"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(body_json(serde_json::json!({ "nick": null })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    send(&h.app, form("/services/discord/unlink", "", &pilot)).await;
    let err = tether_web::discord::remove_member(
        &h.db,
        &h.key,
        &h.discord,
        333_333_333_333_333_333,
        true,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, tether_jobs::JobError::Retry(m) if m.contains("refused")),
        "{err:?}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sync_whose_removed_role_is_refused_retries(db: PgPool) {
    let h = harness(db, true).await;
    let (_, _, account) = linked_pilot(&h).await;
    mount_member(&h, &[MEMBER_ROLE], None).await;
    Mock::given(method("DELETE"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
            ),
        )
        .mount(&h.discord_server)
        .await;
    let err = tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[500_000_000_000_000_003],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, tether_jobs::JobError::Retry(m) if m.contains("refused")),
        "{err:?}"
    );
}

// ---- fleet pings ------------------------------------------------------------

const PING_CHANNEL: &str = "600000000000000001";

/// Discord set up, a ping channel chosen, and the Member role mapped.
async fn pings_ready(h: &Harness) -> (String, String) {
    let (owner, pilot) = set_up(h).await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/channels")))
        .respond_with(ok("channels"))
        .mount(&h.discord_server)
        .await;
    let added = send(
        &h.app,
        form(
            "/admin/discord/channels",
            &format!("channel_id={PING_CHANNEL}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(added.location(), "/admin/discord", "{}", added.body);
    map(h, &owner, MEMBER_ROLE, "state:1").await;
    (owner, pilot)
}

async fn ping(h: &Harness, token: &str, target: &str, message: &str) -> Res {
    let body = format!(
        "channel_id={PING_CHANNEL}&target={}&message={}",
        target.replace(':', "%3A"),
        message.replace(' ', "+")
    );
    send(&h.app, form("/pings", &body, token)).await
}

fn message_posted(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({ "id": id, "channel_id": PING_CHANNEL }))
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn fleet_pings_need_the_permission(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    assert_eq!(send(&h.app, get("/pings", &[])).await.location(), "/login");
    assert_eq!(
        page(&h, "/pings", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ping(&h, &pilot, "none", "hi").await.status,
        StatusCode::FORBIDDEN
    );
    assert!(
        !page(&h, "/dashboard", &pilot)
            .await
            .body
            .contains(r#"href="/pings""#)
    );

    let res = page(&h, "/pings", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("No ping channels for you yet"));
    assert!(res.body.contains(r#"href="/pings""#), "nav link");

    // Anyone can be Guest or join an Open group: pinging stays off-limits.
    let refused = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            "permission=fleet.ping&grantee=state:3",
            &owner,
        ),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert!(
        refused.body.contains("fleet.ping can&#39;t go to Guest"),
        "{}",
        refused.body
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_choose_the_ping_channels(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    let admin = page(&h, "/admin/discord", &owner).await;
    assert!(admin.body.contains("#fleet-pings"));
    // Offered: the other text channel, not voice channels or categories.
    assert!(
        admin
            .body
            .contains(r#"<option value="600000000000000002">#announcements</option>"#)
    );
    assert!(!admin.body.contains("Comms"));

    let again = send(
        &h.app,
        form(
            "/admin/discord/channels",
            &format!("channel_id={PING_CHANNEL}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let voice = send(
        &h.app,
        form(
            "/admin/discord/channels",
            "channel_id=600000000000000004",
            &owner,
        ),
    )
    .await;
    assert_eq!(voice.status, StatusCode::NOT_FOUND);

    let removed = send(
        &h.app,
        form(
            &format!("/admin/discord/channels/{PING_CHANNEL}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(removed.location(), "/admin/discord");
    assert!(
        page(&h, "/pings", &owner)
            .await
            .body
            .contains("No ping channels for you yet")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_ping_posts_to_the_channel_and_pings_only_its_target(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    let targets = page(&h, "/pings", &owner).await.body;
    assert!(
        targets.contains(&format!(
            r#"<option value="role:{MEMBER_ROLE}">@Member</option>"#
        )),
        "{targets}"
    );

    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "content": format!("<@&{MEMBER_ROLE}>\nForm up @\u{200B}everyone\n— Chribba"),
            "allowed_mentions": { "parse": [], "roles": [MEMBER_ROLE] },
            "enforce_nonce": true,
        })))
        .respond_with(message_posted("900000000000000001"))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    let res = ping(
        &h,
        &owner,
        &format!("role:{MEMBER_ROLE}"),
        "Form up @everyone",
    )
    .await;
    assert_eq!(res.location(), "/pings", "{}", res.body);
    // A random nonce, the same one any retry would send.
    let sent = &h.discord_server.received_requests().await.unwrap();
    let body: serde_json::Value = sent
        .iter()
        .find(|r| r.url.path().ends_with("/messages"))
        .unwrap()
        .body_json()
        .unwrap();
    let nonce: String = sqlx::query_scalar("SELECT nonce FROM core.fleet_pings")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(body["nonce"], nonce.as_str());
    assert!(nonce.starts_with("tp-") && nonce.len() <= 25, "{nonce}");

    let listed = page(&h, "/pings", &owner).await.body;
    assert!(listed.contains("Sent"), "{listed}");
    assert!(
        listed.contains("#fleet-pings<div class=\"text-xs text-muted-foreground\">@Member</div>"),
        "{listed}"
    );
    let audit: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'ping.send'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(audit["target"], format!("role:{MEMBER_ROLE}"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pings_are_checked_and_rate_limited(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(message_posted("900000000000000001"))
        .mount(&h.discord_server)
        .await;

    assert_eq!(
        ping(&h, &owner, "none", "").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        ping(&h, &owner, "role:123", "hi").await.status,
        StatusCode::BAD_REQUEST
    );
    let long = "x".repeat(1501);
    assert_eq!(
        ping(&h, &owner, "none", &long).await.status,
        StatusCode::BAD_REQUEST
    );
    let other_channel = send(
        &h.app,
        form(
            "/pings",
            "channel_id=600000000000000002&target=none&message=hi",
            &owner,
        ),
    )
    .await;
    assert_eq!(other_channel.status, StatusCode::BAD_REQUEST);

    for _ in 0..5 {
        assert_eq!(
            ping(&h, &owner, "here", "go").await.status,
            StatusCode::SEE_OTHER
        );
    }
    let limited = ping(&h, &owner, "here", "go").await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    // What they typed survives the refusal.
    assert!(limited.body.contains(">go</textarea>"), "{}", limited.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_ping_waits_out_a_discord_outage_but_not_forever(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&h.discord_server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(message_posted("900000000000000002"))
        .mount(&h.discord_server)
        .await;

    assert_eq!(
        ping(&h, &owner, "none", "late").await.status,
        StatusCode::SEE_OTHER
    );
    assert!(page(&h, "/pings", &owner).await.body.contains("Retrying"));
    let jobs = jobs_of_kind(&h.db, "discord.ping").await;
    assert_eq!(jobs, [serde_json::json!({ "ping_id": 1 })]);
    tether_web::pings::deliver(&h.db, &h.key, &h.discord, 1)
        .await
        .unwrap();
    assert!(page(&h, "/pings", &owner).await.body.contains("Sent"));
    // Delivered once: a second run does nothing.
    tether_web::pings::deliver(&h.db, &h.key, &h.discord, 1)
        .await
        .unwrap();

    // A ping still stuck after 15 minutes is dropped, not sent late.
    assert_eq!(
        ping(&h, &owner, "none", "stale").await.status,
        StatusCode::SEE_OTHER
    );
    sqlx::query("UPDATE core.fleet_pings SET sent_at = NULL, created_at = now() - interval '20 minutes' WHERE id = 2")
        .execute(&h.db)
        .await
        .unwrap();
    let err = tether_web::pings::deliver(&h.db, &h.key, &h.discord, 2)
        .await
        .unwrap_err();
    assert!(
        matches!(err, tether_jobs::JobError::Permanent(_)),
        "{err:?}"
    );
    assert!(
        page(&h, "/pings", &owner)
            .await
            .body
            .contains("unavailable for too long")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_ping_the_bot_may_not_post_says_why(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"code": 50013, "message": "Missing Permissions"}),
            ),
        )
        .mount(&h.discord_server)
        .await;
    assert_eq!(
        ping(&h, &owner, "everyone", "hi").await.status,
        StatusCode::SEE_OTHER
    );
    let listed = page(&h, "/pings", &owner).await.body;
    assert!(listed.contains("Failed"));
    assert!(
        listed.contains("give it View Channel and Send Messages"),
        "{listed}"
    );
    // The queued retry finds it closed and does nothing.
    tether_web::pings::deliver(&h.db, &h.key, &h.discord, 1)
        .await
        .unwrap();
    let posts = h
        .discord_server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/messages"))
        .count();
    assert_eq!(posts, 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_here_ping_cannot_smuggle_in_everyone(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "content": "@here\nUndock now, not you @\u{200B}everyone or @\u{200B}here\n— Chribba",
            "allowed_mentions": { "parse": ["everyone"] },
        })))
        .respond_with(message_posted("900000000000000003"))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    let res = ping(&h, &owner, "here", "Undock now, not you @everyone or @here").await;
    assert_eq!(res.location(), "/pings", "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn parallel_sends_cannot_beat_the_rate_limit(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(message_posted("900000000000000001"))
        .mount(&h.discord_server)
        .await;
    let mut sends = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (app, owner) = (h.app.clone(), owner.clone());
        sends.spawn(async move {
            send(
                &app,
                form(
                    "/pings",
                    &format!("channel_id={PING_CHANNEL}&target=here&message=go"),
                    &owner,
                ),
            )
            .await
            .status
        });
    }
    let mut statuses = Vec::new();
    while let Some(status) = sends.join_next().await {
        statuses.push(status.unwrap());
    }
    let sent = statuses
        .iter()
        .filter(|s| **s == StatusCode::SEE_OTHER)
        .count();
    let limited = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((sent, limited), (5, 3), "{statuses:?}");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM core.fleet_pings")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(rows, 5);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_queued_ping_to_a_removed_channel_is_not_sent(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&h.discord_server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(message_posted("900000000000000004"))
        .expect(0)
        .mount(&h.discord_server)
        .await;
    ping(&h, &owner, "none", "wrong channel").await;
    send(
        &h.app,
        form(
            &format!("/admin/discord/channels/{PING_CHANNEL}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    let err = tether_web::pings::deliver(&h.db, &h.key, &h.discord, 1)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, tether_jobs::JobError::Permanent(m) if m.contains("no longer a ping channel")),
        "{err:?}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_ping_nothing_will_send_shows_as_failed(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&h.discord_server)
        .await;
    ping(&h, &owner, "none", "lost").await;
    assert!(page(&h, "/pings", &owner).await.body.contains("Retrying"));
    // Its retries ran out before the cutoff closed it.
    sqlx::query("UPDATE core.fleet_pings SET created_at = now() - interval '20 minutes'")
        .execute(&h.db)
        .await
        .unwrap();
    let listed = page(&h, "/pings", &owner).await.body;
    assert!(listed.contains("Failed"), "{listed}");
    assert!(listed.contains("unavailable for too long"));
}

// ---- services -------------------------------------------------------------

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_services_page_shows_discord_by_access(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot) = set_up(&h).await;
    let page_ = page(&h, "/services", &pilot).await;
    assert_eq!(page_.status, StatusCode::OK);
    assert!(page_.body.contains("Link Discord"), "{}", page_.body);
    let nav = page(&h, "/dashboard", &pilot).await;
    assert!(nav.body.contains(r#"href="/services""#));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn losing_discord_access_unlinks_notifies_and_removes(db: PgPool) {
    let h = harness(db, true).await;
    let (_, pilot, account) = linked_pilot(&h).await;
    clear_jobs(&h.db).await;
    // Back to Guest, which has no Discord access.
    sqlx::query("UPDATE core.accounts SET state_id = 3 WHERE id = $1")
        .bind(account)
        .execute(&h.db)
        .await
        .unwrap();
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    assert!(
        tether_db::discord::link_for(&h.db, tether_db::accounts::AccountId(account))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        jobs_of_kind(&h.db, "discord.remove_member").await,
        [serde_json::json!({ "discord_user_id": 333_333_333_333_333_333_i64 })]
    );
    let notices = tether_db::notifications::list(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
    assert!(
        notices
            .iter()
            .any(|n| n.title == "Discord Account Disabled"),
        "{notices:?}"
    );
    let _ = pilot;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn changing_who_has_discord_access_checks_everyone(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, _) = linked_pilot(&h).await;
    clear_jobs(&h.db).await;
    let grant: i64 = sqlx::query_scalar(
        "SELECT id FROM core.permission_grants WHERE permission = 'discord.access_discord' AND state_id = 2",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    let res = send(
        &h.app,
        form(&format!("/admin/permissions/{grant}/revoke"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(jobs_of_kind(&h.db, "discord.sync_all").await.len(), 1);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn unmapped_roles_go_when_the_setting_is_on(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _, account) = linked_pilot(&h).await;
    map(&h, &owner, MEMBER_ROLE, "state:1").await;
    send(
        &h.app,
        form(
            "/admin/groups/reserved",
            "name=Allied&reason=Discord+only",
            &owner,
        ),
    )
    .await;
    let res = send(
        &h.app,
        form("/admin/discord/options", "strip_unmapped=on", &owner),
    )
    .await;
    assert_eq!(res.location(), "/admin/discord");
    // Member (mapped), Allied (a reserved name), Fleet Commander (can
    // @everyone: staff), Server Booster (Discord's own).
    mount_member(
        &h,
        &[
            MEMBER_ROLE,
            "500000000000000004",
            "500000000000000006",
            "500000000000000005",
        ],
        None,
    )
    .await;
    mount_role_edits(&h).await;
    let sync = || async {
        tether_web::discord_sync::sync_member(
            &sync_context(&h),
            tether_db::accounts::AccountId(account),
            &[],
        )
        .await
        .unwrap();
    };
    sync().await;
    assert!(role_edits(&h).await.is_empty());
    // Not reserved any more: it goes.
    send(
        &h.app,
        form("/admin/groups/reserved/remove", "name=Allied", &owner),
    )
    .await;
    sync().await;
    assert_eq!(
        role_edits(&h).await,
        [("DELETE".to_owned(), "500000000000000004".to_owned())]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_sync_refreshes_the_stored_discord_name(db: PgPool) {
    let h = harness(db, true).await;
    let (_, _, account) = linked_pilot(&h).await;
    sqlx::query("UPDATE core.discord_links SET username = 'old name'")
        .execute(&h.db)
        .await
        .unwrap();
    mount_member(&h, &[], None).await;
    tether_web::discord_sync::sync_member(
        &sync_context(&h),
        tether_db::accounts::AccountId(account),
        &[],
    )
    .await
    .unwrap();
    let link = tether_db::discord::link_for(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.username, "Unpercieved");
}

// ---- fleet pings: aa-fleetpings' fields and limits --------------------------

fn enc(text: &str) -> String {
    text.replace('%', "%25")
        .replace(' ', "+")
        .replace(':', "%3A")
        .replace('#', "%23")
        .replace('/', "%2F")
        .replace('&', "%26")
}

async fn add_option(h: &Harness, token: &str, body: &str) -> Res {
    send(&h.app, form("/admin/pings/options", body, token)).await
}

async fn option_id(h: &Harness, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM core.ping_options WHERE name = $1")
        .bind(name)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_detailed_ping_posts_an_embed_and_copy_text(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = pings_ready(&h).await;
    let res = add_option(
        &h,
        &owner,
        &format!("kind=fleet_type&name=Roaming&color={}", enc("#00FF00")),
    )
    .await;
    assert_eq!(res.location(), "/admin/pings", "{}", res.body);
    let res = add_option(
        &h,
        &owner,
        &format!(
            "kind=doctrine&name=Caracals&link={}",
            enc("https://doctrines.example/caracals")
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/pings", "{}", res.body);

    let fields = format!(
        "channel_id={PING_CHANNEL}&target=here&pre_ping=on&fleet_type=Roaming&fc_name=Chribba\
         &fleet_name={}&formup_location=Jita&formup_time=2026-09-30T19%3A00&comms={}\
         &doctrine=caracals&srp=yes&message={}",
        enc("Sunday roam"),
        enc("Mumble: Fleet 1"),
        enc("Bring points @everyone")
    );
    // The copy-paste text first: nothing is sent.
    let preview = send(&h.app, form("/pings/preview", &fields, &owner)).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    for line in [
        "Pre-Ping: Roaming Fleet",
        "FC: Chribba",
        "Fleet Name: Sunday roam",
        "Formup Location: Jita",
        "Formup Time: 2026-09-30 19:00 EVE",
        "Comms: Mumble: Fleet 1",
        "Doctrine: caracals",
        "SRP: Yes",
    ] {
        assert!(preview.body.contains(line), "{line}: {}", preview.body);
    }

    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "content": "@here\n**Pre-Ping: Roaming Fleet**",
            "allowed_mentions": { "parse": ["everyone"] },
            "embeds": [{
                "title": "Sunday roam",
                "color": 0x00ff00,
                "footer": { "text": "Sent by Chribba via Tether" },
            }],
        })))
        .respond_with(message_posted("900000000000000009"))
        .expect(1)
        .mount(&h.discord_server)
        .await;
    let res = send(&h.app, form("/pings", &fields, &owner)).await;
    assert_eq!(res.location(), "/pings", "{}", res.body);
    let sent = h.discord_server.received_requests().await.unwrap();
    let body: serde_json::Value = sent
        .iter()
        .find(|r| r.url.path().ends_with("/messages"))
        .unwrap()
        .body_json()
        .unwrap();
    let embed = &body["embeds"][0];
    let field = |name: &str| {
        embed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .map(|f| f["value"].as_str().unwrap().to_owned())
    };
    assert_eq!(
        field("Doctrine").as_deref(),
        Some("[caracals](https://doctrines.example/caracals)")
    );
    assert_eq!(field("SRP").as_deref(), Some("Yes"));
    let description = embed["description"].as_str().unwrap();
    assert!(
        description.contains("@\u{200B}everyone"),
        "defused: {description}"
    );
    assert!(
        description.contains("<t:"),
        "a Discord timestamp: {description}"
    );
    let listed = page(&h, "/pings", &owner).await.body;
    assert!(
        listed.contains("Pre-Ping: Roaming Fleet: Sunday roam"),
        "{listed}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn limits_decide_who_may_use_what(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = pings_ready(&h).await;
    let granted = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            "permission=fleet.ping&grantee=state:1",
            &owner,
        ),
    )
    .await;
    assert_eq!(granted.status, StatusCode::SEE_OTHER, "{}", granted.body);
    Mock::given(method("POST"))
        .and(path(format!("/api/v10/channels/{PING_CHANNEL}/messages")))
        .respond_with(message_posted("900000000000000001"))
        .mount(&h.discord_server)
        .await;
    add_option(&h, &owner, "kind=fleet_type&name=CTA").await;
    add_option(&h, &owner, "kind=doctrine&name=Supers").await;
    let fcs = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"FCs"}"#),
    )
    .await;
    let fcs: serde_json::Value = serde_json::from_str(&fcs.body).unwrap();
    let fcs = fcs["id"].as_i64().unwrap();
    for item in [
        format!("option:{}", option_id(&h, "CTA").await),
        format!("option:{}", option_id(&h, "Supers").await),
        "everyone".to_owned(),
    ] {
        let res = send(
            &h.app,
            form(
                "/admin/pings/restrictions",
                &format!("item={}&grantee=group%3A{fcs}", enc(&item)),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.location(), "/admin/pings", "{item}: {}", res.body);
    }

    // The pilot isn't an FC: none of it is offered, and none of it goes
    // through when posted anyway.
    let form_page = page(&h, "/pings", &pilot).await.body;
    assert!(!form_page.contains(">CTA<"), "{form_page}");
    assert!(!form_page.contains(r#"value="everyone""#), "{form_page}");
    for (fields, why) in [
        ("fleet_type=CTA&message=go", "Choose one of the fleet types"),
        ("doctrine=supers&message=go", "open to you"),
        ("target=everyone&message=go", "Choose who to ping"),
    ] {
        let body = if fields.starts_with("target=") {
            format!("channel_id={PING_CHANNEL}&{fields}")
        } else {
            format!("channel_id={PING_CHANNEL}&target=none&{fields}")
        };
        let res = send(&h.app, form("/pings", &body, &pilot)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{fields}");
        assert!(res.body.contains(why), "{fields}: {}", res.body);
    }

    // In the group, they are.
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{fcs}/members"),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/pings",
            &format!("channel_id={PING_CHANNEL}&target=everyone&fleet_type=CTA&doctrine=Supers"),
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.location(), "/pings", "{}", res.body);
    // A lookalike with an invisible character doesn't pass for a closed
    // doctrine either.
    let res = send(
        &h.app,
        form(
            "/pings",
            &format!("channel_id={PING_CHANNEL}&target=none&doctrine=Sup%E2%80%8Bers"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    // A limited channel disappears for everyone else.
    let res = send(
        &h.app,
        form(
            "/admin/pings/restrictions",
            &format!("item=channel%3A{PING_CHANNEL}&grantee=state%3A2"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/pings", "{}", res.body);
    let pilot_view = page(&h, "/pings", &pilot).await.body;
    assert!(pilot_view.contains("No ping channels for you yet"));
    // Nor its history.
    assert!(!pilot_view.contains("CTA Fleet"), "{pilot_view}");
    assert!(page(&h, "/pings", &owner).await.body.contains("CTA Fleet"));
    // Limits are stored by their canonical name, whatever was posted.
    let res = send(
        &h.app,
        form(
            "/admin/pings/restrictions",
            &format!("item=channel%3A0{PING_CHANNEL}&grantee=state%3A1"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/pings", "{}", res.body);
    let items: Vec<String> =
        sqlx::query_scalar("SELECT item FROM core.ping_restrictions WHERE item LIKE 'channel:%'")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert!(
        items
            .iter()
            .all(|i| i == &format!("channel:{PING_CHANNEL}")),
        "{items:?}"
    );
    let res = send(
        &h.app,
        form(
            "/admin/pings/restrictions",
            &format!("item=channel%3A{PING_CHANNEL}&grantee=group%3A999999"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn mass_pings_can_be_switched_off_and_settings_are_checked(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = pings_ready(&h).await;
    assert_eq!(
        page(&h, "/admin/pings", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    let res = send(&h.app, form("/admin/pings/settings", "", &owner)).await;
    assert_eq!(res.location(), "/admin/pings", "{}", res.body);
    let form_page = page(&h, "/pings", &owner).await.body;
    assert!(!form_page.contains(r#"value="here""#), "{form_page}");
    let res = send(
        &h.app,
        form(
            "/pings",
            &format!("channel_id={PING_CHANNEL}&target=here&message=go"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = send(
        &h.app,
        form(
            "/pings",
            &format!("channel_id={PING_CHANNEL}&target=none&formup_time=-5000-01-01T00%3A00"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    for (body, why) in [
        ("kind=fleet_type&name=X&color=green", "six hex digits"),
        (
            "kind=doctrine&name=Y&link=http%3A%2F%2Fexample.com",
            "https://",
        ),
        ("kind=nonsense&name=Z", "Choose what to add"),
        ("kind=comms&name=", "Give it a name"),
    ] {
        let res = add_option(&h, &owner, body).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(res.body.contains(why), "{body}: {}", res.body);
    }
    let res = send(
        &h.app,
        form(
            "/admin/pings/restrictions",
            "item=option%3A999&grantee=state%3A1",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
