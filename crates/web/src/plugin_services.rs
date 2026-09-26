//! Tether's side of the plugin ESI, identity and Discord interfaces (F16,
//! N8, N10).
//!
//! Every ESI call is checked here, on every call:
//! - the endpoint is in the catalogue (`tether_esi::plugin`);
//! - its scope is one the admin approved for this plugin, of the right
//!   kind (user scopes for character endpoints, data-source scopes for
//!   corporation ones);
//! - the subject is a Member's character registered with the scope (user;
//!   F11), or an approved data source (corporation);
//! - its token carries the scope.
//!
//! The host fills in the ids; the plugin only names the endpoint and the
//! subject, and never sees a token. Every call, allowed or not, goes to
//! the plugin's access log.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use tether_core::crypto::EncryptionKey;
use tether_db::permissions::Grantee;
use tether_db::{PgPool, discord as discord_db, plugin_esi as db};
use tether_discord::{Discord, Mention as DiscordMention, store};
use tether_esi::Esi;
use tether_esi::plugin::{About, Target, endpoint as find_endpoint};
use tether_esi::vault::{TokenVault, VaultError};
use tether_plugins::services::{
    Channel, Character, DiscordError, EsiError, EsiResponse, Fut, Mention, Named, Services, Subject,
};

use crate::plugins::Plugins;
use crate::ratelimit::RateLimiter;

/// The largest ESI body a plugin call may return.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Discord messages per plugin per minute.
pub const SENDS_PER_MINUTE: usize = 20;
pub const MAX_MESSAGE: usize = 1500;
/// Plugin ESI calls are kept this long in the access log...
pub const ACCESS_LOG_DAYS: i32 = 90;
/// ...and at most this many per plugin.
pub const ACCESS_LOG_KEEP: i64 = 100_000;
/// ESI errors a plugin may cause in [`ERROR_WINDOW`] before its calls are
/// refused for the rest of it: they spend the error budget Tether shares.
pub const MAX_ERRORS: usize = 30;
pub const ERROR_WINDOW: Duration = Duration::from_secs(5 * 60);

/// What the services need from the rest of Tether.
#[derive(Clone)]
pub struct Deps {
    pub db: PgPool,
    pub esi: Esi,
    pub vault: Arc<TokenVault>,
    pub discord: Arc<Discord>,
    pub key: EncryptionKey,
}

impl std::fmt::Debug for Deps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deps").finish_non_exhaustive()
    }
}

pub struct PluginServices {
    deps: Deps,
    plugins: Weak<Plugins>,
    sends: RateLimiter<String>,
    throttle: Arc<ErrorThrottle>,
}

/// Plugins that cause too many ESI errors are refused for a while: the
/// error budget is Tether's, shared with its own syncs.
#[derive(Debug)]
struct ErrorThrottle {
    errors: RateLimiter<String>,
    blocked: std::sync::Mutex<std::collections::HashMap<String, Instant>>,
}

impl ErrorThrottle {
    fn new() -> Self {
        Self {
            errors: RateLimiter::new(MAX_ERRORS, ERROR_WINDOW),
            blocked: std::sync::Mutex::default(),
        }
    }

    fn blocked(&self, plugin: &str) -> bool {
        let mut blocked = self.blocked.lock().unwrap_or_else(|p| p.into_inner());
        match blocked.get(plugin) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                blocked.remove(plugin);
                false
            }
            None => false,
        }
    }

    fn error(&self, plugin: &str) {
        if self
            .errors
            .check(plugin.to_owned(), Instant::now())
            .is_err()
        {
            tracing::warn!(
                plugin,
                "plugin caused too many ESI errors; refusing its calls for a while"
            );
            self.blocked
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(plugin.to_owned(), Instant::now() + ERROR_WINDOW);
        }
    }
}

impl std::fmt::Debug for PluginServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginServices").finish_non_exhaustive()
    }
}

impl PluginServices {
    pub fn new(deps: Deps, plugins: Weak<Plugins>) -> Arc<Self> {
        Arc::new(Self {
            deps,
            plugins,
            sends: RateLimiter::new(SENDS_PER_MINUTE, Duration::from_secs(60)),
            throttle: Arc::new(ErrorThrottle::new()),
        })
    }
}

fn character(row: db::CharacterRow) -> Character {
    Character {
        id: row.id,
        name: row.name,
        corporation_id: row.corporation_id.unwrap_or(0),
        alliance_id: row.alliance_id,
    }
}

/// A short label for the access log.
fn outcome(result: &Result<EsiResponse, EsiError>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(EsiError::NotAllowed(_)) => "not allowed".to_owned(),
        Err(EsiError::NotRegistered) => "not registered".to_owned(),
        Err(EsiError::NotADataSource) => "not a data source".to_owned(),
        Err(EsiError::Token) => "no usable token".to_owned(),
        Err(EsiError::Status(status)) => format!("ESI {status}"),
        Err(EsiError::Invalid(_)) => "invalid".to_owned(),
        Err(EsiError::TooLarge) => "too large".to_owned(),
        Err(EsiError::Unavailable) => "unavailable".to_owned(),
    }
}

async fn esi_get(
    deps: &Deps,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    subject: Subject,
    params: &[(String, String)],
    page: Option<u32>,
) -> Result<EsiResponse, EsiError> {
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(EsiError::Unavailable)?;
    let endpoint = find_endpoint(name).ok_or_else(|| {
        EsiError::NotAllowed(format!("{name:?} isn't an endpoint plugins can call"))
    })?;
    let approved = &running.manifest.capabilities.esi;
    let unavailable = |e: sqlx::Error| {
        tracing::error!(plugin, error = %e, "plugin ESI checks");
        EsiError::Unavailable
    };
    let target = match (endpoint.about, subject) {
        (About::Character, Subject::Character(id)) => {
            if !approved.user.iter().any(|s| s == endpoint.scope) {
                return Err(EsiError::NotAllowed(format!(
                    "{} needs {}, which isn't one of this plugin's user scopes",
                    endpoint.name, endpoint.scope
                )));
            }
            let registered =
                tether_db::compliance::character_may_serve(&deps.db, id, endpoint.scope)
                    .await
                    .map_err(unavailable)?;
            if !registered {
                return Err(EsiError::NotRegistered);
            }
            Target {
                character_id: id,
                corporation_id: 0,
            }
        }
        (About::Corporation, Subject::DataSource(id)) => {
            if !approved.data_source.iter().any(|s| s == endpoint.scope) {
                return Err(EsiError::NotAllowed(format!(
                    "{} needs {}, which isn't one of this plugin's data-source scopes",
                    endpoint.name, endpoint.scope
                )));
            }
            let corporation = db::approved_source_corporation(&deps.db, plugin, id)
                .await
                .map_err(unavailable)?
                .ok_or(EsiError::NotADataSource)?;
            Target {
                character_id: id,
                corporation_id: corporation,
            }
        }
        (About::Character, _) => {
            return Err(EsiError::NotAllowed(format!(
                "{} is about a character: use one of esi::characters()",
                endpoint.name
            )));
        }
        (About::Corporation, _) => {
            return Err(EsiError::NotAllowed(format!(
                "{} is about a corporation: use a data source",
                endpoint.name
            )));
        }
    };
    let token = deps
        .vault
        .access_token(target.character_id, &[endpoint.scope])
        .await
        .map_err(|e| match e {
            VaultError::NoToken | VaultError::Revoked | VaultError::MissingScopes(_) => {
                EsiError::Token
            }
            other => {
                tracing::warn!(plugin, error = %other, "plugin ESI token");
                EsiError::Unavailable
            }
        })?;
    let response = deps
        .esi
        .plugin_get(endpoint, &token, target, params, page)
        .await
        .map_err(|e| match e {
            tether_esi::EsiError::Status(status) => EsiError::Status(status),
            tether_esi::EsiError::InvalidInput(why) => EsiError::Invalid(why),
            other => {
                tracing::warn!(plugin, error = %other, "plugin ESI call");
                EsiError::Unavailable
            }
        })?;
    let body = response.body.to_string();
    if body.len() > MAX_BODY_BYTES {
        return Err(EsiError::TooLarge);
    }
    Ok(EsiResponse {
        body,
        pages: response.pages,
    })
}

async fn discord_send(
    deps: &Deps,
    plugins: &Weak<Plugins>,
    plugin: &str,
    channel: &str,
    text: &str,
    mention: Mention,
) -> Result<(), DiscordError> {
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(DiscordError::Unavailable)?;
    if !running
        .manifest
        .capabilities
        .discord
        .iter()
        .any(|a| a == "send_message")
    {
        return Err(DiscordError::NotAllowed(
            "this plugin wasn't approved to send Discord messages".to_owned(),
        ));
    }
    if text.trim().is_empty() || text.chars().count() > MAX_MESSAGE {
        return Err(DiscordError::Invalid(format!(
            "a message is 1 to {MAX_MESSAGE} characters"
        )));
    }
    let unavailable = |e: String| {
        tracing::warn!(plugin, error = e, "plugin Discord send");
        DiscordError::Unavailable
    };
    let config = store::load(&deps.db, &deps.key)
        .await
        .map_err(|e| unavailable(e.to_string()))?
        .ok_or_else(|| DiscordError::NotAllowed("Discord isn't set up".to_owned()))?;
    let guild = i64::try_from(config.guild_id).map_err(|e| unavailable(e.to_string()))?;
    let channel_id: i64 = channel
        .parse()
        .map_err(|_| DiscordError::NotAllowed("not one of this plugin's channels".to_owned()))?;
    let assigned = db::channels(&deps.db, plugin, guild)
        .await
        .map_err(|e| unavailable(e.to_string()))?;
    if !assigned.iter().any(|(id, _)| *id == channel_id) {
        return Err(DiscordError::NotAllowed(
            "not one of this plugin's channels".to_owned(),
        ));
    }
    let target = match mention {
        Mention::None => DiscordMention::None,
        Mention::State(name) => {
            // One answer for "no such state" and "no role": plugins don't
            // learn which states exist.
            let no_role =
                || DiscordError::NotAllowed("no Discord role is mapped to that state".to_owned());
            let name = name.trim();
            if name.chars().count() > tether_core::states::MAX_NAME {
                return Err(no_role());
            }
            let state = tether_db::states::by_name(&deps.db, name)
                .await
                .map_err(|e| unavailable(e.to_string()))?
                .ok_or_else(no_role)?;
            let mappings = discord_db::mappings(&deps.db)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            let role = mappings
                .iter()
                .find(|m| m.grantee == Grantee::State(state.id))
                .ok_or_else(no_role)?;
            DiscordMention::Role(
                u64::try_from(role.role_id).map_err(|e| unavailable(e.to_string()))?,
            )
        }
    };
    let mut content = target.prefix();
    if !content.is_empty() {
        content.push(' ');
    }
    content.push_str(&crate::pings::defuse(text));
    let nonce = tether_core::new_token()
        .map_err(|e| unavailable(e.to_string()))?
        .expose()
        .chars()
        .take(25)
        .collect::<String>();
    deps.discord
        .send_message(
            &config,
            u64::try_from(channel_id).map_err(|e| unavailable(e.to_string()))?,
            &content,
            None,
            target,
            &nonce,
        )
        .await
        .map(|_| ())
        .map_err(|e| unavailable(e.to_string()))
}

impl Services for PluginServices {
    fn esi_get(
        &self,
        plugin: String,
        endpoint: String,
        subject: Subject,
        params: Vec<(String, String)>,
        page: Option<u32>,
    ) -> Fut<Result<EsiResponse, EsiError>> {
        let (deps, plugins, throttle) = (
            self.deps.clone(),
            self.plugins.clone(),
            self.throttle.clone(),
        );
        Box::pin(async move {
            let character = match subject {
                Subject::Character(id) | Subject::DataSource(id) => id,
            };
            if throttle.blocked(&plugin) {
                return Err(EsiError::Unavailable);
            }
            let result = esi_get(&deps, &plugins, &plugin, &endpoint, subject, &params, page).await;
            // `fleet-members` answers "not in a fleet" for ESI's 404: still
            // an error ESI counted, so it counts here too.
            let not_in_fleet = endpoint == "fleet-members"
                && result.as_ref().is_ok_and(|r| {
                    serde_json::from_str::<serde_json::Value>(&r.body)
                        .is_ok_and(|v| v["in_fleet"] == serde_json::Value::Bool(false))
                });
            if matches!(result, Err(EsiError::Status(_))) || not_in_fleet {
                throttle.error(&plugin);
            }
            // Only the catalogue's own names: a plugin's text isn't logged.
            let logged_endpoint = find_endpoint(&endpoint)
                .map_or("(unknown endpoint)", |e| e.name)
                .to_owned();
            if let Err(err) = db::log_access(
                &deps.db,
                &plugin,
                Some(character),
                &logged_endpoint,
                &outcome(&result),
            )
            .await
            {
                tracing::error!(plugin, error = %err, "plugin access log");
            }
            result
        })
    }

    fn esi_characters(&self, plugin: String) -> Fut<Vec<Character>> {
        let db = self.deps.db.clone();
        let plugins = self.plugins.clone();
        Box::pin(async move {
            let Some(running) = plugins.upgrade().and_then(|p| p.running(&plugin)) else {
                return Vec::new();
            };
            let scopes = &running.manifest.capabilities.esi.user;
            if scopes.is_empty() {
                return Vec::new();
            }
            match tether_db::compliance::serving_characters(&db, scopes).await {
                Ok(rows) => rows.into_iter().map(character).collect(),
                Err(err) => {
                    tracing::error!(plugin, error = %err, "plugin characters");
                    Vec::new()
                }
            }
        })
    }

    fn esi_data_sources(&self, plugin: String) -> Fut<Vec<Character>> {
        let db = self.deps.db.clone();
        Box::pin(async move {
            match db::data_sources(&db, &plugin).await {
                Ok(rows) => rows
                    .into_iter()
                    .filter(db::DataSource::in_use)
                    .map(|d| character(d.character))
                    .collect(),
                Err(err) => {
                    tracing::error!(plugin, error = %err, "plugin data sources");
                    Vec::new()
                }
            }
        })
    }

    fn esi_names(&self, plugin: String, ids: Vec<i64>) -> Fut<Result<Vec<Named>, EsiError>> {
        let esi = self.deps.esi.clone();
        Box::pin(async move {
            esi.names(&ids, tether_esi::Priority::Bulk)
                .await
                .map(|names| {
                    names
                        .into_iter()
                        .map(|n| Named {
                            id: n.id,
                            name: n.name,
                            category: n.category,
                        })
                        .collect()
                })
                .map_err(|e| {
                    tracing::warn!(plugin, error = %e, "plugin ESI names");
                    match e {
                        tether_esi::EsiError::Status(status) => EsiError::Status(status),
                        _ => EsiError::Unavailable,
                    }
                })
        })
    }

    fn discord_channels(&self, plugin: String) -> Fut<Vec<Channel>> {
        let deps = self.deps.clone();
        Box::pin(async move {
            let Ok(Some(config)) = store::load(&deps.db, &deps.key).await else {
                return Vec::new();
            };
            let Ok(guild) = i64::try_from(config.guild_id) else {
                return Vec::new();
            };
            db::channels(&deps.db, &plugin, guild)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, name)| Channel {
                    id: id.to_string(),
                    name,
                })
                .collect()
        })
    }

    fn discord_send(
        &self,
        plugin: String,
        channel: String,
        text: String,
        mention: Mention,
    ) -> Fut<Result<(), DiscordError>> {
        if self.sends.check(plugin.clone(), Instant::now()).is_err() {
            return Box::pin(async { Err(DiscordError::RateLimited) });
        }
        let (deps, plugins) = (self.deps.clone(), self.plugins.clone());
        Box::pin(async move {
            let result = discord_send(&deps, &plugins, &plugin, &channel, &text, mention).await;
            let outcome = if result.is_ok() {
                "ok"
            } else {
                "refused or failed"
            };
            if let Err(err) = db::log_access(&deps.db, &plugin, None, "discord:send", outcome).await
            {
                tracing::error!(plugin, error = %err, "plugin access log");
            }
            result
        })
    }
}
