#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The Discord client against a mock Discord built from recorded-style
//! fixtures in tests/fixtures/discord/.

use tether_core::Secret;
use tether_discord::{Discord, DiscordConfig, DiscordError, Endpoints, Joined, UserToken};
use wiremock::matchers::{body_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BOT: &str = "111111111111111111";
const GUILD: &str = "222222222222222222";
const USER: &str = "333333333333333333";
const MEMBER_ROLE: u64 = 500_000_000_000_000_003;
const ALLIED_ROLE: u64 = 500_000_000_000_000_004;

fn fixture(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/../../tests/fixtures/discord/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn config() -> DiscordConfig {
    DiscordConfig {
        application_id: BOT.parse().unwrap(),
        client_secret: Secret::new("client-secret".to_owned()),
        bot_token: Secret::new("bot-token".to_owned()),
        guild_id: GUILD.parse().unwrap(),
    }
}

async fn mock_discord() -> (MockServer, Discord) {
    let server = MockServer::start().await;
    let discord = Discord::new(
        Endpoints::local(server.address().to_string()),
        tether_net::Outbound::new(
            tether_net::Allowlist::production().with_local(&server.address().to_string()),
            "tether-test",
            std::time::Duration::from_secs(10),
        )
        .unwrap(),
    )
    .unwrap();
    (server, discord)
}

fn user_token() -> UserToken {
    UserToken {
        access_token: Secret::new("user-access-token".to_owned()),
    }
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(fixture(name))
}

fn api_error(status: u16, code: u64, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(serde_json::json!({ "code": code, "message": message }))
}

async fn mock_guild(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .and(header("authorization", "Bot bot-token"))
        .respond_with(ok("bot_user"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}")))
        .respond_with(ok("guild"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/roles")))
        .respond_with(ok("roles"))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{BOT}")))
        .respond_with(ok("bot_member"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn check_reports_the_server_and_which_roles_the_bot_can_give() {
    let (server, discord) = mock_discord().await;
    mock_guild(&server).await;

    let check = discord.check(&config()).await.unwrap();
    assert_eq!(check.bot_name, "Tether");
    assert_eq!(check.guild_name, "New Miner's Union");
    assert!(check.missing_permissions.is_empty());
    let roles: Vec<(&str, bool, bool, bool)> = check
        .roles
        .iter()
        .map(|r| (r.name.as_str(), r.assignable, r.administrator, r.privileged))
        .collect();
    assert_eq!(
        roles,
        [
            // Above the bot's own role.
            ("Director", false, true, true),
            // Integration roles.
            ("Tether", false, false, true),
            // Mention Everyone.
            ("Fleet Commander", true, false, true),
            ("Member", true, false, false),
            ("Allied", true, false, false),
            ("Server Booster", false, false, false),
        ]
    );
}

#[tokio::test]
async fn check_names_what_is_wrong() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .respond_with(api_error(401, 0, "401: Unauthorized"))
        .mount(&server)
        .await;
    assert!(matches!(
        discord.check(&config()).await,
        Err(DiscordError::BadBotToken)
    ));

    let (server, discord) = mock_discord().await;
    mock_guild(&server).await;
    let mut other_app = config();
    other_app.application_id = 999;
    assert!(matches!(
        discord.check(&other_app).await,
        Err(DiscordError::WrongApplication {
            application: 999,
            ..
        })
    ));

    let (server, discord) = mock_discord().await;
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .respond_with(ok("bot_user"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}")))
        .respond_with(api_error(403, 50001, "Missing Access"))
        .mount(&server)
        .await;
    let err = discord.check(&config()).await.unwrap_err();
    assert!(
        err.to_string().contains("The bot isn't in that server"),
        "{err}"
    );
}

#[tokio::test]
async fn the_code_exchange_authenticates_the_app_and_requires_both_scopes() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token"))
        // base64("111111111111111111:client-secret")
        .and(header(
            "authorization",
            "Basic MTExMTExMTExMTExMTExMTExOmNsaWVudC1zZWNyZXQ=",
        ))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=the-code"))
        .and(body_string_contains(
            "redirect_uri=https%3A%2F%2Fa.example%2Fdiscord%2Fcallback",
        ))
        .respond_with(ok("token"))
        .expect(1)
        .mount(&server)
        .await;
    let token = discord
        .exchange_code(&config(), "the-code", "https://a.example/discord/callback")
        .await
        .unwrap();
    assert_eq!(token.access_token.expose(), "user-access-token-fixture");
    assert!(!format!("{token:?}").contains("user-access-token-fixture"));

    let (server, discord) = mock_discord().await;
    let mut narrow = fixture("token");
    narrow["scope"] = "identify".into();
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(narrow))
        .mount(&server)
        .await;
    let err = discord
        .exchange_code(&config(), "c", "https://a.example/cb")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("guilds.join"), "{err}");

    let (server, discord) = mock_discord().await;
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"invalid_client"})),
        )
        .mount(&server)
        .await;
    assert!(matches!(
        discord
            .exchange_code(&config(), "c", "https://a.example/cb")
            .await,
        Err(DiscordError::BadClientCredentials)
    ));
}

#[tokio::test]
async fn current_user_uses_the_members_bearer_token() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .and(header("authorization", "Bearer user-access-token"))
        .respond_with(ok("member_user"))
        .mount(&server)
        .await;
    let user = discord.current_user(&user_token()).await.unwrap();
    assert_eq!(user.id, USER.parse::<u64>().unwrap());
    assert_eq!(user.username, "unpercieved");
    assert_eq!(user.display_name(), "Unpercieved");
}

#[tokio::test]
async fn joining_adds_the_member_with_roles() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(header("authorization", "Bot bot-token"))
        .and(body_json(serde_json::json!({
            "access_token": "user-access-token",
            "roles": [MEMBER_ROLE.to_string(), ALLIED_ROLE.to_string()],
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&server)
        .await;
    let joined = discord
        .join(
            &config(),
            USER.parse().unwrap(),
            &user_token(),
            &[MEMBER_ROLE, ALLIED_ROLE],
        )
        .await
        .unwrap();
    assert_eq!(joined, Joined::Added);
}

#[tokio::test]
async fn joining_when_already_in_the_server_adds_the_roles_one_by_one() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    for role in [MEMBER_ROLE, ALLIED_ROLE] {
        Mock::given(method("PUT"))
            .and(path(format!(
                "/api/v10/guilds/{GUILD}/members/{USER}/roles/{role}"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let joined = discord
        .join(
            &config(),
            USER.parse().unwrap(),
            &user_token(),
            &[MEMBER_ROLE, ALLIED_ROLE],
        )
        .await
        .unwrap();
    assert_eq!(joined, Joined::AlreadyMember);
}

#[tokio::test]
async fn removing_roles_from_someone_who_left_is_done_and_outages_are_transient() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(api_error(404, 10007, "Unknown Member"))
        .expect(1)
        .mount(&server)
        .await;
    discord
        .remove_roles(
            &config(),
            USER.parse().unwrap(),
            &[MEMBER_ROLE, ALLIED_ROLE],
        )
        .await
        .unwrap();

    let (server, discord) = mock_discord().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
        .mount(&server)
        .await;
    let err = discord
        .remove_roles(&config(), USER.parse().unwrap(), &[MEMBER_ROLE])
        .await
        .unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert!(!err.to_string().contains("bad gateway"));

    // A role the bot may not take is skipped; the rest still go.
    let (server, discord) = mock_discord().await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{MEMBER_ROLE}"
        )))
        .respond_with(api_error(403, 50013, "Missing Permissions"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/api/v10/guilds/{GUILD}/members/{USER}/roles/{ALLIED_ROLE}"
        )))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let refused = discord
        .remove_roles(
            &config(),
            USER.parse().unwrap(),
            &[MEMBER_ROLE, ALLIED_ROLE],
        )
        .await
        .unwrap();
    assert_eq!(refused, [MEMBER_ROLE]);

    // Other refusals still fail.
    let (server, discord) = mock_discord().await;
    Mock::given(method("DELETE"))
        .respond_with(api_error(403, 50001, "Missing Access"))
        .mount(&server)
        .await;
    let err = discord
        .remove_roles(&config(), USER.parse().unwrap(), &[MEMBER_ROLE])
        .await
        .unwrap_err();
    assert!(!err.is_transient());
    assert_eq!(err.code(), Some(50001));
}

#[tokio::test]
async fn revoking_sends_the_token_to_discord() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("POST"))
        .and(path("/api/v10/oauth2/token/revoke"))
        .and(body_string_contains("token=user-access-token"))
        .and(body_string_contains("token_type_hint=access_token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    discord.revoke(&config(), &user_token()).await.unwrap();
}

#[tokio::test]
async fn member_reads_roles_and_nick_and_none_when_they_left() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("added_member")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .respond_with(api_error(404, 10007, "Unknown Member"))
        .mount(&server)
        .await;
    let member = discord
        .member(&config(), USER.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.roles, [MEMBER_ROLE]);
    assert_eq!(member.nick, None);
    assert!(
        discord
            .member(&config(), USER.parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn set_nick_patches_the_member() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/v10/guilds/{GUILD}/members/{USER}")))
        .and(body_json(
            serde_json::json!({ "nick": "[SWA] The Mittani" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("added_member")))
        .expect(1)
        .mount(&server)
        .await;
    discord
        .set_nick(&config(), USER.parse().unwrap(), Some("[SWA] The Mittani"))
        .await
        .unwrap();
}

#[tokio::test]
async fn check_cached_asks_discord_once() {
    let (server, discord) = mock_discord().await;
    mock_guild(&server).await;
    let ttl = std::time::Duration::from_secs(60);
    discord.check_cached(&config(), ttl).await.unwrap();
    discord.check_cached(&config(), ttl).await.unwrap();
    let roles_calls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/roles"))
        .count();
    assert_eq!(roles_calls, 1);
    // A different configuration isn't served from the cache.
    let mut other = config();
    other.bot_token = Secret::new("other-token".to_owned());
    assert!(discord.check_cached(&other, ttl).await.is_err());
}

#[tokio::test]
async fn text_channels_lists_text_and_announcement_channels_in_order() {
    let (server, discord) = mock_discord().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v10/guilds/{GUILD}/channels")))
        .respond_with(ok("channels"))
        .mount(&server)
        .await;
    let channels = discord.text_channels(&config()).await.unwrap();
    let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["announcements", "fleet-pings"]);
}

#[tokio::test]
async fn messages_ping_only_the_chosen_target_and_carry_a_nonce() {
    use tether_discord::Mention;
    let (server, discord) = mock_discord().await;
    Mock::given(method("POST"))
        .and(path("/api/v10/channels/600000000000000001/messages"))
        .and(has_content_type_json())
        .and(body_json(serde_json::json!({
            "content": "<@&500000000000000003> Form up @everyone",
            "allowed_mentions": { "parse": [], "roles": ["500000000000000003"] },
            "nonce": "tether-ping-7",
            "enforce_nonce": true,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "900000000000000001", "channel_id": "600000000000000001"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mention = Mention::Role(MEMBER_ROLE);
    let content = format!("{} Form up @everyone", mention.prefix());
    let id = discord
        .send_message(
            &config(),
            600_000_000_000_000_001,
            &content,
            mention,
            "tether-ping-7",
        )
        .await
        .unwrap();
    assert_eq!(id, 900_000_000_000_000_001);
    assert_eq!(Mention::Here.prefix(), "@here");
    assert_eq!(Mention::None.prefix(), "");
}

fn has_content_type_json() -> wiremock::matchers::HeaderExactMatcher {
    header("content-type", "application/json")
}

#[test]
fn endpoints_must_be_on_the_allow_list() {
    let net = tether_net::Outbound::new(
        tether_net::Allowlist::production(),
        "tether-test",
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert!(Discord::new(Endpoints::discord(), net.clone()).is_ok());
    let err = Discord::new(Endpoints::local("evil.example:80"), net).unwrap_err();
    assert!(
        err.to_string().contains("isn't an allowed destination"),
        "{err}"
    );
}
