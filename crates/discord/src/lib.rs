//! Discord, REST only (no gateway): one bot serves the core and every
//! plugin. Members join the server by linking their Discord account with
//! OAuth2 (`identify guilds.join`); after that only ESI affiliation drives
//! their roles, and leaving the server isn't tracked.
//!
//! The bot's REST calls go through twilight-http. The OAuth2 code exchange
//! isn't something twilight does, so it is a plain form POST here.

pub mod store;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tether_core::{Secret, hash_token};
use twilight_http::Client;
use twilight_http::api_error::ApiError;
use twilight_http::client::ClientBuilder;
use twilight_http::error::ErrorType;
use twilight_http::response::StatusCode;
use twilight_model::guild::Permissions;
use twilight_model::id::Id;

/// Where members approve linking, in their browser.
pub const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
/// `identify` to learn who they are; `guilds.join` so the bot can add them.
pub const SCOPES: &str = "identify guilds.join";
/// What the bot needs: Create Instant Invite (to add members), Kick Members
/// (members who lose access leave the server, as in AA), Manage Nicknames
/// and Manage Roles.
pub const BOT_PERMISSIONS: Permissions = Permissions::CREATE_INVITE
    .union(Permissions::KICK_MEMBERS)
    .union(Permissions::MANAGE_NICKNAMES)
    .union(Permissions::MANAGE_ROLES);

/// Moderation and server-management powers. Roles carrying any of these
/// never go to people anyone can become (Guest, Open groups), and
/// Administrator never goes to anyone.
pub const PRIVILEGED: Permissions = Permissions::ADMINISTRATOR
    .union(Permissions::MANAGE_GUILD)
    .union(Permissions::MANAGE_ROLES)
    .union(Permissions::MANAGE_CHANNELS)
    .union(Permissions::MANAGE_WEBHOOKS)
    .union(Permissions::MANAGE_GUILD_EXPRESSIONS)
    .union(Permissions::MANAGE_EVENTS)
    .union(Permissions::MANAGE_THREADS)
    .union(Permissions::MANAGE_MESSAGES)
    .union(Permissions::MANAGE_NICKNAMES)
    .union(Permissions::KICK_MEMBERS)
    .union(Permissions::BAN_MEMBERS)
    .union(Permissions::MODERATE_MEMBERS)
    .union(Permissions::MUTE_MEMBERS)
    .union(Permissions::DEAFEN_MEMBERS)
    .union(Permissions::MOVE_MEMBERS)
    .union(Permissions::MENTION_EVERYONE)
    .union(Permissions::VIEW_AUDIT_LOG)
    .union(Permissions::VIEW_GUILD_INSIGHTS);

const TIMEOUT: Duration = Duration::from_secs(10);

/// Discord error codes this crate looks at.
pub mod codes {
    pub const UNKNOWN_GUILD: u64 = 10004;
    pub const UNKNOWN_MEMBER: u64 = 10007;
    pub const UNKNOWN_ROLE: u64 = 10011;
    pub const MISSING_ACCESS: u64 = 50001;
    pub const MISSING_PERMISSIONS: u64 = 50013;
}

/// The instance's Discord application and server. The secrets come out of
/// the encrypted store only to build this.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// Also the OAuth2 client id, and the bot's user id.
    pub application_id: u64,
    pub client_secret: Secret<String>,
    pub bot_token: Secret<String>,
    pub guild_id: u64,
}

/// Where the REST API is. Tests point it at a local mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `None` is discord.com over HTTPS; `Some(host:port)` is plain HTTP.
    local: Option<String>,
}

impl Endpoints {
    pub fn discord() -> Self {
        Self { local: None }
    }

    /// A plain-HTTP stand-in at `host:port`, for tests.
    pub fn local(host_port: impl Into<String>) -> Self {
        Self {
            local: Some(host_port.into()),
        }
    }

    /// The REST API's base URL.
    pub fn api_base(&self) -> String {
        match &self.local {
            Some(host) => format!("http://{host}/api/v10"),
            None => "https://discord.com/api/v10".to_owned(),
        }
    }

    fn builder(&self) -> ClientBuilder {
        // twilight's rustls panics without a process-wide crypto provider.
        tether_net::install_crypto_provider();
        // Don't let one 401 poison the client for good: the admin may fix
        // the token and try again.
        let builder = Client::builder()
            .timeout(TIMEOUT)
            .remember_invalid_token(false);
        match &self.local {
            Some(host) => builder.proxy(host.clone(), true),
            None => builder,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiscordError {
    #[error("Discord rejected the bot token")]
    BadBotToken,
    #[error("Discord rejected the application id or client secret")]
    BadClientCredentials,
    #[error(
        "the bot token belongs to a different application ({bot}) than the application id ({application})"
    )]
    WrongApplication { bot: u64, application: u64 },
    #[error("Discord refused: {message} (code {code})")]
    Forbidden { code: u64, message: String },
    #[error("Discord couldn't find it: {message} (code {code})")]
    NotFound { code: u64, message: String },
    #[error("Discord rejected the request (HTTP {status}): {message}")]
    Rejected {
        status: u16,
        code: u64,
        message: String,
    },
    /// Worth retrying later: a timeout, a 5xx, a rate limit.
    #[error("Discord is unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected response from Discord: {0}")]
    Protocol(String),
    #[error("{0}")]
    Config(String),
}

impl DiscordError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }

    /// Discord's JSON error code, if it sent one.
    pub fn code(&self) -> Option<u64> {
        match self {
            Self::Forbidden { code, .. }
            | Self::NotFound { code, .. }
            | Self::Rejected { code, .. } => Some(*code),
            _ => None,
        }
    }
}

impl From<twilight_http::Error> for DiscordError {
    fn from(err: twilight_http::Error) -> Self {
        match err.kind() {
            ErrorType::Response { status, error, .. } => {
                let (code, message) = match error {
                    ApiError::General(general) => (general.code, general.message.clone()),
                    _ => (0, "rate limited".to_owned()),
                };
                match status.get() {
                    401 => Self::BadBotToken,
                    403 => Self::Forbidden { code, message },
                    404 => Self::NotFound { code, message },
                    429 | 500.. => Self::Unavailable(format!("HTTP {}: {message}", status.get())),
                    status => Self::Rejected {
                        status,
                        code,
                        message,
                    },
                }
            }
            ErrorType::Unauthorized => Self::BadBotToken,
            // Usually a 5xx page from a proxy in front of Discord. The body
            // isn't included: it could be anything.
            ErrorType::Parsing { .. } => {
                Self::Unavailable("Discord sent an unreadable error response".to_owned())
            }
            ErrorType::RequestError | ErrorType::RequestTimedOut | ErrorType::RequestCanceled => {
                Self::Unavailable(err.to_string())
            }
            _ => Self::Protocol(err.to_string()),
        }
    }
}

/// The member's short-lived OAuth2 access token. Used once to add them to
/// the server, then revoked; never stored.
#[derive(Debug)]
pub struct UserToken {
    pub access_token: Secret<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordUser {
    pub id: u64,
    pub username: String,
    /// The display name, when they set one.
    pub global_name: Option<String>,
}

impl DiscordUser {
    pub fn display_name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }
}

/// What a configuration can reach, for the setup page and `doctor`.
#[derive(Debug, Clone)]
pub struct GuildCheck {
    pub bot_name: String,
    pub guild_name: String,
    /// Nobody can change the server owner's nickname.
    pub owner_id: u64,
    /// Bot permissions from [`BOT_PERMISSIONS`] it doesn't have.
    pub missing_permissions: Vec<&'static str>,
    /// Highest first, without `@everyone`.
    pub roles: Vec<GuildRole>,
}

#[derive(Debug, Clone)]
pub struct GuildRole {
    pub id: u64,
    pub name: String,
    pub position: i64,
    /// Integration roles (bots, boosts) that nobody can hand out.
    pub managed: bool,
    pub administrator: bool,
    /// Carries any of [`PRIVILEGED`].
    pub privileged: bool,
    /// Below the bot's highest role and not managed: the bot can give it.
    pub assignable: bool,
}

/// A channel messages can be posted to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChannel {
    pub id: u64,
    pub name: String,
}

/// A rich card under a message (Discord's embed). Mentions in it never
/// ping anyone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: Option<String>,
    /// `0xRRGGBB`.
    pub color: Option<u32>,
    /// `(name, value)`, shown three to a row.
    pub fields: Vec<(String, String)>,
    pub footer: Option<String>,
}

impl Embed {
    fn json(&self) -> serde_json::Value {
        let mut embed = serde_json::json!({
            "title": self.title,
            "fields": self
                .fields
                .iter()
                .map(|(name, value)| serde_json::json!({ "name": name, "value": value, "inline": true }))
                .collect::<Vec<_>>(),
        });
        if let Some(description) = &self.description {
            embed["description"] = description.as_str().into();
        }
        if let Some(color) = self.color {
            embed["color"] = color.into();
        }
        if let Some(footer) = &self.footer {
            embed["footer"] = serde_json::json!({ "text": footer });
        }
        embed
    }
}

/// Who a message pings. Nothing else in it can ping anyone: mentions typed
/// into the text are ignored by Discord (`allowed_mentions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mention {
    None,
    Here,
    Everyone,
    Role(u64),
}

impl Mention {
    /// The text that makes Discord ping, placed at the start of a message.
    pub fn prefix(self) -> String {
        match self {
            Self::None => String::new(),
            Self::Here => "@here".to_owned(),
            Self::Everyone => "@everyone".to_owned(),
            Self::Role(id) => format!("<@&{id}>"),
        }
    }

    fn allowed(self) -> serde_json::Value {
        match self {
            Self::None => serde_json::json!({ "parse": [] }),
            Self::Here | Self::Everyone => serde_json::json!({ "parse": ["everyone"] }),
            Self::Role(id) => serde_json::json!({ "parse": [], "roles": [id.to_string()] }),
        }
    }
}

/// A member of the server, as far as syncing cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub roles: Vec<u64>,
    pub nick: Option<String>,
    /// Their Discord name (display name if set), for the stored link.
    pub username: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined {
    /// Added to the server with the roles.
    Added,
    /// Already in the server; the roles were added to what they had.
    AlreadyMember,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    scope: String,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

pub struct Discord {
    endpoints: Endpoints,
    http: tether_net::Outbound,
    /// The bot client, kept so twilight's rate limiter sees every call.
    /// Rebuilt when the token changes (keyed by its hash).
    bot: Mutex<Option<(Vec<u8>, Arc<Client>)>>,
    /// The last check, for syncing many members in a row.
    checked: Mutex<Option<(Instant, Vec<u8>, GuildCheck)>>,
}

impl std::fmt::Debug for Discord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discord")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl Discord {
    /// OAuth2 calls go through `http`; the bot's calls (twilight, which
    /// makes its own connections) go to `endpoints`, which must be on the
    /// same allow-list.
    pub fn new(endpoints: Endpoints, http: tether_net::Outbound) -> Result<Self, DiscordError> {
        http.allowlist()
            .check(&endpoints.api_base())
            .map_err(|err| DiscordError::Config(err.to_string()))?;
        Ok(Self {
            endpoints,
            http,
            bot: Mutex::new(None),
            checked: Mutex::new(None),
        })
    }

    /// Where to send a member's browser to link their account.
    pub fn authorize_url(application_id: u64, redirect_uri: &str, state: &str) -> String {
        let application_id = application_id.to_string();
        let params = [
            ("response_type", "code"),
            ("client_id", application_id.as_str()),
            ("scope", SCOPES),
            ("redirect_uri", redirect_uri),
            ("state", state),
            ("prompt", "none"),
        ];
        url_with(AUTHORIZE_URL, &params)
    }

    /// The link an admin opens to add the bot to the server.
    pub fn bot_invite_url(application_id: u64, guild_id: u64) -> String {
        let (application_id, guild_id) = (application_id.to_string(), guild_id.to_string());
        let permissions = BOT_PERMISSIONS.bits().to_string();
        let params = [
            ("client_id", application_id.as_str()),
            ("scope", "bot"),
            ("permissions", permissions.as_str()),
            ("guild_id", guild_id.as_str()),
            ("disable_guild_select", "true"),
        ];
        url_with(AUTHORIZE_URL, &params)
    }

    /// Trades the code from the OAuth2 callback for the member's token.
    pub async fn exchange_code(
        &self,
        config: &DiscordConfig,
        code: &str,
        redirect_uri: &str,
    ) -> Result<UserToken, DiscordError> {
        let response = self
            .http
            .post(&format!("{}/oauth2/token", self.endpoints.api_base()))
            .map_err(|err| DiscordError::Config(err.to_string()))?
            .basic_auth(config.application_id, Some(config.client_secret.expose()))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(unavailable)?;
        let status = response.status();
        let body = response.bytes().await.map_err(unavailable)?;
        if !status.is_success() {
            return Err(oauth_error(status.as_u16(), &body));
        }
        let token: TokenResponse = serde_json::from_slice(&body)
            .map_err(|_| DiscordError::Protocol("unreadable token response".to_owned()))?;
        let granted: Vec<&str> = token.scope.split_whitespace().collect();
        if let Some(missing) = SCOPES.split(' ').find(|s| !granted.contains(s)) {
            return Err(DiscordError::Rejected {
                status: status.as_u16(),
                code: 0,
                message: format!("the {missing} scope wasn't granted"),
            });
        }
        Ok(UserToken {
            access_token: Secret::new(token.access_token),
        })
    }

    /// Revokes a member's token once it has been used.
    pub async fn revoke(
        &self,
        config: &DiscordConfig,
        token: &UserToken,
    ) -> Result<(), DiscordError> {
        let response = self
            .http
            .post(&format!(
                "{}/oauth2/token/revoke",
                self.endpoints.api_base()
            ))
            .map_err(|err| DiscordError::Config(err.to_string()))?
            .basic_auth(config.application_id, Some(config.client_secret.expose()))
            .form(&[
                ("token", token.access_token.expose().as_str()),
                ("token_type_hint", "access_token"),
            ])
            .send()
            .await
            .map_err(unavailable)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.bytes().await.map_err(unavailable)?;
        Err(oauth_error(status.as_u16(), &body))
    }

    /// Who the token belongs to (`GET /users/@me`).
    pub async fn current_user(&self, token: &UserToken) -> Result<DiscordUser, DiscordError> {
        let client = self
            .endpoints
            .builder()
            .token(format!("Bearer {}", token.access_token.expose()))
            .build();
        let user = client
            .current_user()
            .await?
            .model()
            .await
            .map_err(protocol)?;
        Ok(DiscordUser {
            id: user.id.get(),
            username: user.name,
            global_name: user.global_name,
        })
    }

    /// Checks the bot token, that the bot is in the server, what it may do
    /// there, and which roles it can hand out.
    pub async fn check(&self, config: &DiscordConfig) -> Result<GuildCheck, DiscordError> {
        let bot = self.bot(config);
        let guild_id = id(config.guild_id, "server id")?;
        let me = bot.current_user().await?.model().await.map_err(protocol)?;
        if me.id.get() != config.application_id {
            return Err(DiscordError::WrongApplication {
                bot: me.id.get(),
                application: config.application_id,
            });
        }
        let guild = bot.guild(guild_id).await.map_err(not_in_guild)?;
        let guild = guild.model().await.map_err(protocol)?;
        let roles = bot
            .roles(guild_id)
            .await?
            .models()
            .await
            .map_err(protocol)?;
        let member = bot.guild_member(guild_id, me.id).await?;
        let member = member.model().await.map_err(protocol)?;

        let bot_roles = roles.iter().filter(|r| member.roles.contains(&r.id));
        let top = bot_roles.clone().map(|r| r.position).max().unwrap_or(0);
        let everyone = roles
            .iter()
            .find(|r| r.id.get() == config.guild_id)
            .map_or(Permissions::empty(), |r| r.permissions);
        let granted = bot_roles.fold(everyone, |acc, r| acc | r.permissions);
        let is_admin = granted.contains(Permissions::ADMINISTRATOR);
        let missing_permissions = [
            (Permissions::CREATE_INVITE, "Create Invite"),
            (Permissions::KICK_MEMBERS, "Kick Members"),
            (Permissions::MANAGE_NICKNAMES, "Manage Nicknames"),
            (Permissions::MANAGE_ROLES, "Manage Roles"),
        ]
        .into_iter()
        .filter(|(p, _)| !is_admin && !granted.contains(*p))
        .map(|(_, name)| name)
        .collect();
        let can_manage_roles = is_admin || granted.contains(Permissions::MANAGE_ROLES);

        let mut roles: Vec<GuildRole> = roles
            .into_iter()
            .filter(|r| r.id.get() != config.guild_id)
            .map(|r| GuildRole {
                id: r.id.get(),
                assignable: can_manage_roles && !r.managed && r.position < top,
                administrator: r.permissions.contains(Permissions::ADMINISTRATOR),
                privileged: r.permissions.intersects(PRIVILEGED),
                managed: r.managed,
                position: r.position,
                name: r.name,
            })
            .collect();
        roles.sort_by(|a, b| b.position.cmp(&a.position).then(a.id.cmp(&b.id)));
        Ok(GuildCheck {
            bot_name: me.name,
            guild_name: guild.name,
            owner_id: guild.owner_id.get(),
            missing_permissions,
            roles,
        })
    }

    /// Like [`Discord::check`], but reuses a result younger than `max_age`
    /// for the same configuration: syncing hundreds of members shouldn't ask
    /// for the server's roles hundreds of times.
    pub async fn check_cached(
        &self,
        config: &DiscordConfig,
        max_age: Duration,
    ) -> Result<GuildCheck, DiscordError> {
        let key = config_key(config);
        {
            let cached = self.checked.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((at, k, check)) = cached.as_ref()
                && *k == key
                && at.elapsed() < max_age
            {
                return Ok(check.clone());
            }
        }
        let check = self.check(config).await?;
        *self.checked.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), key, check.clone()));
        Ok(check)
    }

    /// The member's roles and nickname; `None` if they aren't in the server.
    pub async fn member(
        &self,
        config: &DiscordConfig,
        user_id: u64,
    ) -> Result<Option<Member>, DiscordError> {
        let bot = self.bot(config);
        let (guild_id, user) = (id(config.guild_id, "server id")?, id(user_id, "user id")?);
        let member = match bot.guild_member(guild_id, user).await {
            Ok(response) => response.model().await.map_err(protocol)?,
            Err(err) => {
                let err = DiscordError::from(err);
                if err.code() == Some(codes::UNKNOWN_MEMBER) {
                    return Ok(None);
                }
                return Err(err);
            }
        };
        Ok(Some(Member {
            roles: member.roles.iter().map(|r| r.get()).collect(),
            nick: member.nick,
            username: member
                .user
                .global_name
                .clone()
                .unwrap_or_else(|| member.user.name.clone()),
        }))
    }

    /// Removes a member from the server. Someone already gone is nothing
    /// to do.
    pub async fn kick(&self, config: &DiscordConfig, user_id: u64) -> Result<(), DiscordError> {
        let bot = self.bot(config);
        let (guild_id, user) = (id(config.guild_id, "server id")?, id(user_id, "user id")?);
        match bot
            .remove_guild_member(guild_id, user)
            .await
            .map_err(DiscordError::from)
        {
            Ok(_) => Ok(()),
            Err(err) if err.code() == Some(codes::UNKNOWN_MEMBER) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Sets (or with `None`, clears) the member's nickname.
    pub async fn set_nick(
        &self,
        config: &DiscordConfig,
        user_id: u64,
        nick: Option<&str>,
    ) -> Result<(), DiscordError> {
        let bot = self.bot(config);
        let (guild_id, user) = (id(config.guild_id, "server id")?, id(user_id, "user id")?);
        bot.update_guild_member(guild_id, user).nick(nick).await?;
        Ok(())
    }

    /// The server's text and announcement channels, in their order.
    pub async fn text_channels(
        &self,
        config: &DiscordConfig,
    ) -> Result<Vec<TextChannel>, DiscordError> {
        use twilight_model::channel::ChannelType;
        let bot = self.bot(config);
        let guild_id = id(config.guild_id, "server id")?;
        let mut channels = bot
            .guild_channels(guild_id)
            .await?
            .models()
            .await
            .map_err(protocol)?;
        channels.retain(|c| {
            matches!(
                c.kind,
                ChannelType::GuildText | ChannelType::GuildAnnouncement
            )
        });
        channels.sort_by_key(|c| (c.position.unwrap_or(0), c.id.get()));
        Ok(channels
            .into_iter()
            .map(|c| TextChannel {
                id: c.id.get(),
                name: c.name.unwrap_or_default(),
            })
            .collect())
    }

    /// Posts `content` (with an optional embed) to a channel, pinging only
    /// `mention`. `nonce` (up to 25 characters) makes Discord drop a repeat
    /// of the same message, so a retry after a lost response doesn't post
    /// twice. Returns the message id.
    pub async fn send_message(
        &self,
        config: &DiscordConfig,
        channel_id: u64,
        content: &str,
        embed: Option<&Embed>,
        mention: Mention,
        nonce: &str,
    ) -> Result<u64, DiscordError> {
        #[derive(Deserialize)]
        struct Created {
            id: String,
        }
        let bot = self.bot(config);
        let channel = id(channel_id, "channel id")?;
        // twilight has no enforce_nonce, so the body is written here.
        let mut body = serde_json::json!({
            "content": content,
            "allowed_mentions": mention.allowed(),
            "nonce": nonce,
            "enforce_nonce": true,
        });
        if let Some(embed) = embed {
            body["embeds"] = serde_json::json!([embed.json()]);
        }
        let payload =
            serde_json::to_vec(&body).map_err(|err| DiscordError::Protocol(err.to_string()))?;
        let response = bot.create_message(channel).payload_json(&payload).await?;
        let body = response.bytes().await.map_err(protocol)?;
        let created: Created = serde_json::from_slice(&body)
            .map_err(|_| DiscordError::Protocol("unreadable message response".to_owned()))?;
        created
            .id
            .parse()
            .map_err(|_| DiscordError::Protocol("bad message id".to_owned()))
    }

    /// Adds the member to the server with `roles`; if they are already in
    /// it, adds the roles to the ones they have.
    pub async fn join(
        &self,
        config: &DiscordConfig,
        user_id: u64,
        token: &UserToken,
        roles: &[u64],
    ) -> Result<Joined, DiscordError> {
        let bot = self.bot(config);
        let guild_id = id(config.guild_id, "server id")?;
        let user = id(user_id, "user id")?;
        let role_ids = roles
            .iter()
            .map(|r| id(*r, "role id"))
            .collect::<Result<Vec<_>, _>>()?;
        let response = bot
            .add_guild_member(guild_id, user, token.access_token.expose())
            .roles(&role_ids)
            .await?;
        if response.status() == StatusCode::CREATED {
            return Ok(Joined::Added);
        }
        // 204: already a member, and Discord ignored the roles.
        self.add_roles(config, user_id, roles).await?;
        Ok(Joined::AlreadyMember)
    }

    pub async fn add_roles(
        &self,
        config: &DiscordConfig,
        user_id: u64,
        roles: &[u64],
    ) -> Result<(), DiscordError> {
        let bot = self.bot(config);
        let (guild_id, user) = (id(config.guild_id, "server id")?, id(user_id, "user id")?);
        for role in roles {
            match bot
                .add_guild_member_role(guild_id, user, id(*role, "role id")?)
                .await
                .map_err(DiscordError::from)
            {
                Ok(_) => {}
                // One role the bot may not give (moved above it since the
                // check) mustn't stop the others.
                Err(err) if err.code() == Some(codes::MISSING_PERMISSIONS) => {
                    tracing::warn!(role_id = role, "Discord refused to give a role; skipped");
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Removes `roles` from a member, returning any the bot was refused
    /// (50013: moved above it), so one can't stop the others and the caller
    /// can decide whether to try again. Someone who has left the server, or
    /// a role that no longer exists, is nothing to do.
    pub async fn remove_roles(
        &self,
        config: &DiscordConfig,
        user_id: u64,
        roles: &[u64],
    ) -> Result<Vec<u64>, DiscordError> {
        let bot = self.bot(config);
        let (guild_id, user) = (id(config.guild_id, "server id")?, id(user_id, "user id")?);
        let mut refused = Vec::new();
        for role in roles {
            match bot
                .remove_guild_member_role(guild_id, user, id(*role, "role id")?)
                .await
                .map_err(DiscordError::from)
            {
                Ok(_) => {}
                Err(err) if err.code() == Some(codes::UNKNOWN_MEMBER) => return Ok(Vec::new()),
                Err(err) if err.code() == Some(codes::UNKNOWN_ROLE) => {}
                Err(err) if err.code() == Some(codes::MISSING_PERMISSIONS) => {
                    tracing::warn!(role_id = role, "Discord refused to take a role");
                    refused.push(*role);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(refused)
    }

    fn bot(&self, config: &DiscordConfig) -> Arc<Client> {
        let fingerprint = hash_token(config.bot_token.expose());
        let mut cached = self.bot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((key, client)) = cached.as_ref()
            && *key == fingerprint
        {
            return client.clone();
        }
        // ClientBuilder::token adds the "Bot " prefix unless it's there.
        let client = Arc::new(
            self.endpoints
                .builder()
                .token(config.bot_token.expose().trim().to_owned())
                .build(),
        );
        *cached = Some((fingerprint, client.clone()));
        client
    }
}

/// Identifies a configuration without keeping its secrets around.
fn config_key(config: &DiscordConfig) -> Vec<u8> {
    hash_token(&format!(
        "{}:{}:{}",
        config.application_id,
        config.guild_id,
        config.bot_token.expose()
    ))
}

fn id<T>(value: u64, what: &str) -> Result<Id<T>, DiscordError> {
    Id::new_checked(value).ok_or_else(|| DiscordError::Config(format!("the {what} can't be 0")))
}

fn url_with(base: &str, params: &[(&str, &str)]) -> String {
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{query}")
}

/// Percent-encodes everything but unreserved characters (RFC 3986).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn unavailable(err: reqwest::Error) -> DiscordError {
    DiscordError::Unavailable(err.without_url().to_string())
}

fn protocol(err: twilight_http::response::DeserializeBodyError) -> DiscordError {
    DiscordError::Protocol(err.to_string())
}

/// A 404/403 on the guild itself means the bot isn't in that server.
fn not_in_guild(err: twilight_http::Error) -> DiscordError {
    match DiscordError::from(err) {
        DiscordError::NotFound { .. } | DiscordError::Forbidden { .. } => DiscordError::Config(
            "The bot isn't in that server: add it with the invite link, or check the server id."
                .to_owned(),
        ),
        other => other,
    }
}

fn oauth_error(status: u16, body: &[u8]) -> DiscordError {
    let parsed: Option<OAuthError> = serde_json::from_slice(body).ok();
    match (status, parsed) {
        (401, _) => DiscordError::BadClientCredentials,
        (_, Some(e)) if e.error == "invalid_client" => DiscordError::BadClientCredentials,
        (429 | 500.., _) => DiscordError::Unavailable(format!("HTTP {status}")),
        (status, Some(e)) => DiscordError::Rejected {
            status,
            code: 0,
            message: e.error_description.unwrap_or(e.error),
        },
        (status, None) => DiscordError::Rejected {
            status,
            code: 0,
            message: "no error details".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_asks_for_identify_and_join() {
        let url = Discord::authorize_url(123, "https://a.example/discord/callback", "s+t");
        assert_eq!(
            url,
            "https://discord.com/oauth2/authorize?response_type=code&client_id=123\
             &scope=identify%20guilds.join\
             &redirect_uri=https%3A%2F%2Fa.example%2Fdiscord%2Fcallback&state=s%2Bt&prompt=none"
        );
    }

    #[test]
    fn bot_invite_asks_for_exactly_what_the_bot_uses() {
        let url = Discord::bot_invite_url(123, 456);
        // 1 (invite) + 2 (kick) + 1<<27 (nicknames) + 1<<28 (roles)
        assert!(url.contains("permissions=402653187"), "{url}");
        assert!(url.contains("scope=bot&"));
        assert!(url.contains("guild_id=456&disable_guild_select=true"));
    }

    #[test]
    fn debug_never_shows_secrets() {
        let config = DiscordConfig {
            application_id: 1,
            client_secret: Secret::new("client-secret-value".to_owned()),
            bot_token: Secret::new("bot-token-value".to_owned()),
            guild_id: 2,
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("client-secret-value"));
        assert!(!shown.contains("bot-token-value"));
    }

    #[test]
    fn oauth_errors_are_classified() {
        assert!(matches!(
            oauth_error(400, br#"{"error":"invalid_client"}"#),
            DiscordError::BadClientCredentials
        ));
        assert!(matches!(
            oauth_error(
                400,
                br#"{"error":"invalid_grant","error_description":"Invalid \"code\""}"#
            ),
            DiscordError::Rejected { status: 400, .. }
        ));
        assert!(oauth_error(502, b"<html>").is_transient());
    }
}
