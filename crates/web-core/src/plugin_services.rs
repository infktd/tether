//! Tether's side of the plugin ESI, identity, Discord and HTTP interfaces
//! (F16, N8, N10). HTTP lives in [`crate::plugin_http`].
//!
//! Every ESI call is checked here, on every call:
//! - the endpoint is in the catalogue (`tether_esi::plugin`);
//! - its scope is one the admin approved for this plugin, of the right
//!   kind (user scopes for character endpoints, data-source scopes for
//!   corporation ones);
//! - the subject is one of the plugin's characters (user; F16): its
//!   account holds one of the plugin's permissions, whatever its state,
//!   it is registered for the plugin, and its token carries every one of
//!   the plugin's user scopes; or a data source in use (corporation);
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
use tether_discord::{
    Discord, Embed as DiscordEmbed, EmbedField as DiscordField, Mention as DiscordMention, store,
};
use tether_esi::Esi;
use tether_esi::plugin::{About, Target, endpoint as find_endpoint};
use tether_esi::vault::{TokenVault, VaultError};
use tether_plugins::services::{
    Builtin, Channel, Character, DiscordError, Doctrine, DoctrineError, DownloadError,
    DownloadFile, Embed, EsiError, EsiReply, EsiResponse, FilterError, FilterValue, FilterWanted,
    Fut, Group, HttpError, HttpRequest, HttpResponse, Image, Mention, Named, NotifyError,
    NotifyLevel, Owner, Services, SharedDoctrine, SharedTimer, State, Subject, Timer, TimerError,
};

use crate::plugins::Plugins;
use crate::ratelimit::RateLimiter;

/// The largest ESI body a plugin call may return.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Discord messages per plugin per minute.
pub const SENDS_PER_MINUTE: usize = 20;
/// How long Discord refusing a plugin's bot in a channel is remembered:
/// the plugin's sends there meanwhile (its retry without the mention, the
/// rest of its outbox) get the same answer without asking Discord again,
/// and don't count against [`SENDS_PER_MINUTE`].
pub const REFUSAL_MEMORY: Duration = Duration::from_secs(60);
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
    /// The instance's public URL: plugins' HTTP User-Agent carries it, as
    /// a way to reach the operator (zKillboard asks for one).
    pub public_url: String,
    /// Taken before a plugin's migrations on an upgrade. `None` (tests)
    /// migrates without one.
    pub snapshots: Option<Arc<tether_snapshots::Snapshots>>,
    /// Apps from GitHub. `None` (tests) turns it off.
    pub github: Option<Arc<crate::plugin_github::GitHub>>,
    /// The apps bundled into this Tether's image.
    pub bundled: Arc<crate::bundled::Bundled>,
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
    refusals: Arc<Refusals>,
    throttle: Arc<ErrorThrottle>,
    http: Arc<crate::plugin_http::Http>,
    notices: Arc<crate::plugin_notify::Limits>,
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

/// Channels Discord refused a plugin's bot in, for [`REFUSAL_MEMORY`]:
/// (plugin, channel) to until when and why.
#[derive(Debug, Default)]
struct Refusals(std::sync::Mutex<std::collections::HashMap<(String, String), (Instant, String)>>);

impl Refusals {
    fn get(&self, plugin: &str, channel: &str) -> Option<String> {
        let mut refused = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        refused.retain(|_, (until, _)| *until > now);
        refused
            .get(&(plugin.to_owned(), channel.to_owned()))
            .map(|(_, why)| why.clone())
    }

    fn remember(&self, plugin: &str, channel: &str, why: &str) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).insert(
            (plugin.to_owned(), channel.to_owned()),
            (Instant::now() + REFUSAL_MEMORY, why.to_owned()),
        );
    }
}

impl std::fmt::Debug for PluginServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginServices").finish_non_exhaustive()
    }
}

impl PluginServices {
    pub fn new(
        deps: Deps,
        plugins: Weak<Plugins>,
        http: Arc<crate::plugin_http::Http>,
    ) -> Arc<Self> {
        Arc::new(Self {
            deps,
            plugins,
            sends: RateLimiter::new(SENDS_PER_MINUTE, Duration::from_secs(60)),
            refusals: Arc::default(),
            throttle: Arc::new(ErrorThrottle::new()),
            notices: Arc::default(),
            http,
        })
    }
}

/// The one app told who owns a character (`identity.owners`): Member
/// Audit, as bundled with Tether. aa-memberaudit's scopes go by the
/// owner's main, and its Character Finder shows the main and state (Jay,
/// 2026-09-26). No manifest capability asks for this: it is decided here,
/// by the id bundled apps reserve and the origin recorded at install.
pub const OWNERS_APP: &str = "tether.member-audit";

/// Whether the running plugin `id` may learn who owns characters: only
/// [`OWNERS_APP`], and only the package bundled into Tether's image. A
/// package from a file or GitHub can't take the id while it's bundled or
/// was installed bundled (`crate::bundled`), and one that did anyway
/// (Tether without bundled apps) is `signed`, so it gets nothing.
pub fn may_see_owners(id: &str, origin: tether_db::plugins::Origin) -> bool {
    id == OWNERS_APP && origin == tether_db::plugins::Origin::Bundled
}

/// Whether the running plugin `id` was approved for `groups` (the
/// viewer's groups and the groups to offer them).
fn sees_groups(plugins: &Weak<Plugins>, id: &str) -> bool {
    plugins
        .upgrade()
        .and_then(|p| p.running(id))
        .is_some_and(|r| r.manifest.capabilities.groups)
}

fn groups(rows: Vec<(tether_db::groups::GroupId, String)>) -> Vec<Group> {
    rows.into_iter()
        .map(|(id, name)| Group { id: id.0, name })
        .collect()
}

fn character(row: db::CharacterRow) -> Character {
    Character {
        id: row.id,
        name: row.name,
        corporation_id: row.corporation_id.unwrap_or(0),
        alliance_id: row.alliance_id,
    }
}

/// A state's `builtin` as plugins know it: Member, Blue or Guest. The
/// Blacklist reads as Guest: its characters are served only while
/// something grants the account one of the app's permissions.
fn builtin(builtin: Option<&str>) -> Option<Builtin> {
    match builtin {
        Some("member") => Some(Builtin::Member),
        Some("blue") => Some(Builtin::Blue),
        Some("guest" | "blacklist") => Some(Builtin::Guest),
        _ => None,
    }
}

/// A short label for the access log.
fn outcome(result: &Result<EsiReply, EsiError>) -> String {
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

#[allow(clippy::too_many_arguments)]
async fn esi_get(
    deps: &Deps,
    plugins: &Weak<Plugins>,
    throttle: &ErrorThrottle,
    plugin: &str,
    name: &str,
    subject: Subject,
    params: &[(String, String)],
    page: Option<u32>,
) -> Result<EsiReply, EsiError> {
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(EsiError::Unavailable)?;
    let endpoint = find_endpoint(name).ok_or_else(|| {
        EsiError::NotAllowed(format!("{name:?} isn't an endpoint plugins can call"))
    })?;
    if endpoint.about == About::Public {
        // No token and nobody's data: any plugin, any subject.
        let response = deps
            .esi
            .plugin_get_public(endpoint, params)
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
        return Ok(reply(body, &response));
    }
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
            let scopes = crate::compliance::allowed_plugin_scopes(approved.user.as_slice());
            let registered =
                tether_db::compliance::character_may_serve(&deps.db, plugin, id, &scopes)
                    .await
                    .map_err(unavailable)?;
            if !registered {
                // A character the bundled Member Audit keeps (its token
                // stopped working): a token problem, so the app pauses it.
                let kept = may_see_owners(plugin, running.origin)
                    && tether_db::compliance::character_kept_broken(&deps.db, plugin, id, &scopes)
                        .await
                        .map_err(unavailable)?;
                return Err(if kept {
                    EsiError::Token
                } else {
                    EsiError::NotRegistered
                });
            }
            Target {
                character_id: id,
                corporation_id: 0,
                alliance_id: None,
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
            // Alliance endpoints read the source's own alliance.
            let alliance = if endpoint.name.starts_with("alliance-") {
                db::approved_source_alliance(&deps.db, plugin, id)
                    .await
                    .map_err(unavailable)?
            } else {
                None
            };
            Target {
                character_id: id,
                corporation_id: corporation,
                alliance_id: alliance,
            }
        }
        (About::Character, _) => {
            return Err(EsiError::NotAllowed(format!(
                "{} is about a character: use one of esi::characters()",
                endpoint.name
            )));
        }
        (About::Public, _) => {
            return Err(EsiError::Unavailable);
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
    let names_structure = matches!(endpoint.name, "source-structure" | "universe-structure");
    let structure = params
        .iter()
        .find(|(k, _)| k == "structure_id")
        .and_then(|(_, v)| v.parse::<i64>().ok())
        .filter(|id| *id > 0);
    // A name kept from an earlier read (any app's, or through a member)
    // answers without asking ESI, so a structure the app's character may
    // not dock at doesn't cost an ESI error every time.
    if names_structure
        && let Some(named) =
            crate::structure_names::kept(&deps.db, structure.unwrap_or_default()).await
    {
        return Ok(EsiReply {
            response: EsiResponse {
                body: crate::structure_names::body(&named).to_string(),
                pages: 1,
            },
            extra_calls: 0,
        });
    }
    let background = matches!(
        endpoint.name,
        "corporation-asset-places" | "corporation-structure-assets"
    );
    let result = if background {
        // Every page of the corporation's assets is read in the background
        // (`tether_esi::asset_places`), each with a token the vault hands
        // out then: a read of hundreds of pages outlives one token. Each
        // token only while the app still runs and the character is still
        // its approved source in that corporation (the checks above): a
        // source withdrawn, an account blacklisted or an app removed
        // mid-read stops the read at its next page.
        let (db, vault, plugins) = (deps.db.clone(), deps.vault.clone(), plugins.clone());
        let plugin = plugin.to_owned();
        let (character, corporation, scope) =
            (target.character_id, target.corporation_id, endpoint.scope);
        let tokens: tether_esi::asset_places::TokenSource = Arc::new(move || {
            let (db, vault, plugins, plugin) =
                (db.clone(), vault.clone(), plugins.clone(), plugin.clone());
            Box::pin(async move {
                let withdrawn = |why: &str| tether_esi::EsiError::Unavailable(why.to_owned());
                if plugins.upgrade().and_then(|p| p.running(&plugin)).is_none() {
                    return Err(withdrawn("the app no longer runs"));
                }
                match db::approved_source_corporation(&db, &plugin, character).await {
                    Ok(Some(now)) if now == corporation => {}
                    Ok(_) => return Err(withdrawn("the data source was withdrawn")),
                    Err(e) => {
                        return Err(tether_esi::EsiError::Unavailable(format!(
                            "checking the data source: {e}"
                        )));
                    }
                }
                vault.access_token(character, &[scope]).await.map_err(|e| {
                    tether_esi::EsiError::Unavailable(format!("the data source's token: {e}"))
                })
            })
        });
        if endpoint.name == "corporation-structure-assets" {
            deps.esi
                .corporation_structure_assets(tokens, target.corporation_id, character)
                .await
        } else {
            deps.esi
                .corporation_asset_places(tokens, target.corporation_id, character, params)
                .await
        }
    } else {
        deps.esi
            .plugin_get(endpoint, &token, target, params, page)
            .await
    };
    let response = match result {
        Ok(response) => response,
        // ESI names a structure only to a character that may dock there:
        // refused this one, Tether asks through members who may (the name,
        // system and type only; Jay, 2026-10-05).
        Err(tether_esi::EsiError::Status(status @ (401 | 403))) if names_structure => {
            let lookup = match structure {
                Some(id) => {
                    crate::structure_names::through_members(&deps.db, &deps.esi, &deps.vault, id)
                        .await
                }
                None => crate::structure_names::Lookup::default(),
            };
            // Every refusal spent ESI's error budget: each counts against
            // the app (its own refusal is counted by the caller when it
            // gets an error back).
            for _ in 0..lookup.refused {
                throttle.error(plugin);
            }
            return match lookup.named {
                Some(named) => {
                    throttle.error(plugin);
                    Ok(EsiReply {
                        response: EsiResponse {
                            body: crate::structure_names::body(&named).to_string(),
                            pages: 1,
                        },
                        extra_calls: lookup.calls,
                    })
                }
                None => Err(EsiError::Status(status)),
            };
        }
        Err(e) => {
            return Err(match e {
                tether_esi::EsiError::Status(status) => EsiError::Status(status),
                tether_esi::EsiError::InvalidInput(why) => EsiError::Invalid(why),
                // The assets are being read: the app asks again. Not a
                // problem, nor an ESI error.
                tether_esi::EsiError::Pending => EsiError::Unavailable,
                tether_esi::EsiError::TooManyPages(pages) => EsiError::Invalid(format!(
                    "the corporation has {pages} pages of assets, more than the {} Tether reads",
                    tether_esi::asset_places::MAX_ASSET_PAGES
                )),
                other => {
                    tracing::warn!(plugin, error = %other, "plugin ESI call");
                    EsiError::Unavailable
                }
            });
        }
    };
    if names_structure {
        crate::structure_names::remember(&deps.db, &response.body).await;
    }
    let body = response.body.to_string();
    if body.len() > MAX_BODY_BYTES {
        return Err(EsiError::TooLarge);
    }
    Ok(reply(body, &response))
}

/// A write for `character`, one of `account`'s characters (the host
/// checked that, and that it's the pilot's own form post; checked again
/// here against the database): the endpoint is a write the plugin was
/// approved for, the character is one of the plugin's, and its token
/// carries the scope. Audited as the pilot before it's sent: nothing
/// reaches EVE unrecorded.
async fn esi_post(
    deps: &Deps,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    character: i64,
    account: i64,
    body: &str,
) -> Result<EsiReply, EsiError> {
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(EsiError::Unavailable)?;
    let endpoint = tether_esi::plugin::write_endpoint(name)
        .ok_or_else(|| EsiError::NotAllowed(format!("{name:?} isn't a write plugins can make")))?;
    let approved = &running.manifest.capabilities.esi;
    if !approved.user.iter().any(|s| s == endpoint.scope) {
        return Err(EsiError::NotAllowed(format!(
            "{} needs {}, which isn't one of this plugin's user scopes",
            endpoint.name, endpoint.scope
        )));
    }
    let unavailable = |e: sqlx::Error| {
        tracing::error!(plugin, error = %e, "plugin ESI checks");
        EsiError::Unavailable
    };
    let owner = db::character_account(&deps.db, character)
        .await
        .map_err(unavailable)?;
    if owner.map(|a| a.0) != Some(account) {
        return Err(EsiError::NotAllowed(
            "that isn't one of this pilot's characters".to_owned(),
        ));
    }
    let scopes = crate::compliance::allowed_plugin_scopes(approved.user.as_slice());
    if !tether_db::compliance::character_may_serve(&deps.db, plugin, character, &scopes)
        .await
        .map_err(unavailable)?
    {
        return Err(EsiError::NotRegistered);
    }
    // A body that isn't the endpoint's never goes, so it isn't audited.
    tether_esi::plugin::check_write_body(endpoint, body).map_err(|e| match e {
        tether_esi::EsiError::InvalidInput(why) => EsiError::Invalid(why),
        _ => EsiError::Unavailable,
    })?;
    let token = deps
        .vault
        .access_token(character, &[endpoint.scope])
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
    // It changes something in EVE for this pilot: on the audit log, as
    // them, before it goes. If that can't be written, it doesn't go.
    tether_db::audit::record(
        &deps.db,
        tether_db::audit::Actor::Account(tether_db::accounts::AccountId(account)),
        "plugin.esi_write",
        Some(&format!("plugin:{plugin}")),
        serde_json::json!({ "endpoint": endpoint.name, "character_id": character }),
    )
    .await
    .map_err(unavailable)?;
    let target = Target {
        character_id: character,
        corporation_id: 0,
        alliance_id: None,
    };
    let response = deps
        .esi
        .plugin_post(endpoint, &token, target, body)
        .await
        .map_err(|e| match e {
            tether_esi::EsiError::Status(status) => EsiError::Status(status),
            tether_esi::EsiError::InvalidInput(why) => EsiError::Invalid(why),
            other => {
                tracing::warn!(plugin, error = %other, "plugin ESI write");
                EsiError::Unavailable
            }
        })?;
    let body = response.body.to_string();
    if body.len() > MAX_BODY_BYTES {
        return Err(EsiError::TooLarge);
    }
    Ok(reply(body, &response))
}

fn reply(body: String, response: &tether_esi::plugin::Response) -> EsiReply {
    EsiReply {
        response: EsiResponse {
            body,
            pages: response.pages,
        },
        extra_calls: usize::try_from(response.refetched).unwrap_or(usize::MAX),
    }
}

/// Discord's limits on a card, as the WIT documents them.
const MAX_EMBED_TITLE: usize = 256;
const MAX_EMBED_DESCRIPTION: usize = 2000;
const MAX_EMBED_FIELDS: usize = 10;
const MAX_EMBED_VALUE: usize = 1024;
/// Discord refuses a message whose cards hold more than this in all.
const MAX_EMBED_TOTAL: usize = 6000;

/// A link to CCP's image server: the only images a plugin's card shows.
fn image_url(image: &Image) -> Result<String, DiscordError> {
    let (kind, id, variant, size) = match *image {
        Image::Character(id) => ("characters", id, "portrait", 64),
        Image::Corporation(id) => ("corporations", id, "logo", 64),
        Image::Alliance(id) => ("alliances", id, "logo", 64),
        Image::TypeRender(id) => ("types", id, "render", 128),
        Image::TypeIcon(id) => ("types", id, "icon", 64),
    };
    if id <= 0 {
        return Err(DiscordError::Invalid("an image id is positive".to_owned()));
    }
    Ok(format!(
        "https://images.evetech.net/{kind}/{id}/{variant}?size={size}"
    ))
}

/// A plugin's card checked against Discord's limits, its text defused.
fn card(mut embed: Embed) -> Result<DiscordEmbed, DiscordError> {
    let invalid = |why: &str| DiscordError::Invalid(why.to_owned());
    let len = |s: &str| s.chars().count();
    let within = |s: &str, max: usize| !s.trim().is_empty() && len(s) <= max;
    // Defused first, so the limits hold for what Discord gets.
    let defuse = crate::pings::defuse;
    embed.title = defuse(&embed.title);
    embed.description = embed.description.as_deref().map(defuse);
    for field in &mut embed.fields {
        field.name = defuse(&field.name);
        field.value = defuse(&field.value);
    }
    if let Some(author) = &mut embed.author {
        author.name = defuse(&author.name);
    }
    embed.footer = embed.footer.as_deref().map(defuse);
    if !within(&embed.title, MAX_EMBED_TITLE) {
        return Err(invalid("a card's title is 1 to 256 characters"));
    }
    if embed
        .description
        .as_deref()
        .is_some_and(|d| len(d) > MAX_EMBED_DESCRIPTION)
    {
        return Err(invalid("a card's description is at most 2,000 characters"));
    }
    if embed.fields.len() > MAX_EMBED_FIELDS {
        return Err(invalid("a card has at most 10 fields"));
    }
    if embed
        .fields
        .iter()
        .any(|f| !within(&f.name, MAX_EMBED_TITLE) || !within(&f.value, MAX_EMBED_VALUE))
    {
        return Err(invalid(
            "a card's field has a name of 1 to 256 characters and a value of 1 to 1,024",
        ));
    }
    if embed
        .author
        .as_ref()
        .is_some_and(|a| !within(&a.name, MAX_EMBED_TITLE))
    {
        return Err(invalid("a card's author is 1 to 256 characters"));
    }
    if embed
        .footer
        .as_deref()
        .is_some_and(|f| !within(f, MAX_EMBED_TITLE))
    {
        return Err(invalid("a card's footer is 1 to 256 characters"));
    }
    let total = len(&embed.title)
        + embed.description.as_deref().map_or(0, len)
        + embed.author.as_ref().map_or(0, |a| len(&a.name))
        + embed.footer.as_deref().map_or(0, len)
        + embed
            .fields
            .iter()
            .map(|f| len(&f.name) + len(&f.value))
            .sum::<usize>();
    if total > MAX_EMBED_TOTAL {
        return Err(invalid("a card holds at most 6,000 characters in all"));
    }
    let timestamp = match embed.timestamp {
        Some(t) => Some(
            chrono::DateTime::parse_from_rfc3339(&t)
                .map_err(|_| invalid("a card's timestamp is RFC 3339"))?
                .to_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        None => None,
    };
    Ok(DiscordEmbed {
        title: embed.title,
        description: embed.description,
        color: embed.color.map(|c| c & 0x00ff_ffff),
        author: embed
            .author
            .map(|a| Ok::<_, DiscordError>((a.name, a.icon.as_ref().map(image_url).transpose()?)))
            .transpose()?,
        thumbnail: embed.thumbnail.as_ref().map(image_url).transpose()?,
        fields: embed
            .fields
            .into_iter()
            .map(|f| DiscordField {
                name: f.name,
                value: f.value,
                inline: f.inline,
            })
            .collect(),
        footer: embed.footer,
        timestamp,
    })
}

#[allow(clippy::too_many_arguments)]
async fn discord_send(
    deps: &Deps,
    plugins: &Weak<Plugins>,
    refusals: &Refusals,
    plugin: &str,
    channel: &str,
    text: &str,
    embed: Option<Embed>,
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
    let embed = embed.map(card).transpose()?;
    // A card may have no text of its own.
    if (embed.is_none() && text.trim().is_empty()) || text.chars().count() > MAX_MESSAGE {
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
    if !content.is_empty() && !text.is_empty() {
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
            embed.as_ref(),
            target,
            &nonce,
        )
        .await
        .map(|_| ())
        .map_err(|e| {
            // Only a passing failure is worth sending again. Discord
            // refusing the bot (no access to the channel, a channel
            // deleted, a token revoked) stays until an admin fixes it, so
            // the app hears it as final and moves on to its next message.
            if e.is_transient() || matches!(e, tether_discord::DiscordError::Protocol(_)) {
                return unavailable(e.to_string());
            }
            tracing::warn!(plugin, error = %e, "plugin Discord send refused");
            let why = crate::pings::explain(&e);
            // The bot itself refused there (not this one message): its next
            // sends there would be too.
            if matches!(
                e,
                tether_discord::DiscordError::Forbidden { .. }
                    | tether_discord::DiscordError::NotFound { .. }
                    | tether_discord::DiscordError::BadBotToken
            ) {
                refusals.remember(plugin, channel, &why);
            }
            DiscordError::NotAllowed(why)
        })
}

impl Services for PluginServices {
    fn esi_get(
        &self,
        plugin: String,
        endpoint: String,
        subject: Subject,
        params: Vec<(String, String)>,
        page: Option<u32>,
    ) -> Fut<Result<EsiReply, EsiError>> {
        let (deps, plugins, throttle) = (
            self.deps.clone(),
            self.plugins.clone(),
            self.throttle.clone(),
        );
        Box::pin(async move {
            // Public endpoints don't read the subject: none is logged.
            let character = match subject {
                _ if find_endpoint(&endpoint).is_some_and(|e| e.about == About::Public) => None,
                Subject::Character(id) | Subject::DataSource(id) => Some(id),
            };
            if throttle.blocked(&plugin) {
                return Err(EsiError::Unavailable);
            }
            let result = esi_get(
                &deps, &plugins, &throttle, &plugin, &endpoint, subject, &params, page,
            )
            .await;
            // `fleet-members` answers "not in a fleet" for ESI's 404: still
            // an error ESI counted, so it counts here too.
            let not_in_fleet = endpoint == "fleet-members"
                && result.as_ref().is_ok_and(|r| {
                    serde_json::from_str::<serde_json::Value>(&r.response.body)
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
                character,
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

    fn esi_post(
        &self,
        plugin: String,
        endpoint: String,
        character: i64,
        account: i64,
        body: String,
    ) -> Fut<Result<EsiReply, EsiError>> {
        let (deps, plugins, throttle) = (
            self.deps.clone(),
            self.plugins.clone(),
            self.throttle.clone(),
        );
        // In its own task: once a write is on its way, the plugin's deadline
        // (which drops this call) can't cut it off unlogged.
        let task = tokio::spawn(async move {
            if throttle.blocked(&plugin) {
                return Err(EsiError::Unavailable);
            }
            let result = esi_post(
                &deps, &plugins, &plugin, &endpoint, character, account, &body,
            )
            .await;
            if matches!(result, Err(EsiError::Status(_))) {
                throttle.error(&plugin);
            }
            let logged_endpoint = tether_esi::plugin::write_endpoint(&endpoint)
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
        });
        Box::pin(async move { task.await.unwrap_or(Err(EsiError::Unavailable)) })
    }

    fn esi_characters(&self, plugin: String) -> Fut<Vec<Character>> {
        let db = self.deps.db.clone();
        let plugins = self.plugins.clone();
        Box::pin(async move {
            let Some(running) = plugins.upgrade().and_then(|p| p.running(&plugin)) else {
                return Vec::new();
            };
            let scopes =
                crate::compliance::allowed_plugin_scopes(&running.manifest.capabilities.esi.user);
            if scopes.is_empty() {
                return Vec::new();
            }
            // The bundled Member Audit keeps a character whose token stopped
            // working, as aa-memberaudit does: its reads answer `token`.
            let keep_broken = may_see_owners(&plugin, running.origin);
            match tether_db::compliance::serving_characters(&db, &plugin, &scopes, keep_broken)
                .await
            {
                Ok(rows) => rows.into_iter().map(character).collect(),
                Err(err) => {
                    tracing::error!(plugin, error = %err, "plugin characters");
                    Vec::new()
                }
            }
        })
    }

    fn identity_owners(&self, plugin: String) -> Fut<Option<Vec<Owner>>> {
        let db = self.deps.db.clone();
        let plugins = self.plugins.clone();
        Box::pin(async move {
            // The host passes this on only for a component loaded as the
            // allowed app (`LoadedPlugin::seeing_owners`); what holds the
            // id now must be the allowed app too.
            let running = plugins.upgrade().and_then(|p| p.running(&plugin))?;
            if !may_see_owners(&plugin, running.origin) {
                return None;
            }
            // The same characters as `esi_characters`, so none it couldn't
            // already list.
            let scopes =
                crate::compliance::allowed_plugin_scopes(&running.manifest.capabilities.esi.user);
            if scopes.is_empty() {
                return Some(Vec::new());
            }
            match tether_db::compliance::serving_owners(&db, &plugin).await {
                Ok(rows) => Some({
                    tracing::info!(plugin, owners = rows.len(), "plugin read character owners");
                    rows.into_iter()
                        .map(|r| Owner {
                            character_id: r.character_id,
                            main: character(r.main),
                            state: State {
                                builtin: builtin(r.builtin.as_deref()),
                                name: r.state,
                            },
                        })
                        .collect()
                }),
                Err(err) => {
                    tracing::error!(plugin, error = %err, "plugin character owners");
                    None
                }
            }
        })
    }

    fn identity_superuser(&self, account: i64) -> Fut<bool> {
        let db = self.deps.db.clone();
        Box::pin(async move {
            let account = tether_db::accounts::AccountId(account);
            match tether_db::accounts::get(&db, account).await {
                Ok(found) => found.is_some_and(|a| a.active && a.is_owner),
                Err(err) => {
                    tracing::error!(error = %err, "plugin viewer's superuser flag");
                    false
                }
            }
        })
    }

    fn identity_groups(&self, plugin: String, account: i64) -> Fut<Vec<Group>> {
        let db = self.deps.db.clone();
        let plugins = self.plugins.clone();
        Box::pin(async move {
            if !sees_groups(&plugins, &plugin) {
                return Vec::new();
            }
            let account = tether_db::accounts::AccountId(account);
            match tether_db::groups::of_account(&db, account).await {
                Ok(rows) => groups(rows),
                Err(err) => {
                    tracing::error!(error = %err, "plugin viewer's groups");
                    Vec::new()
                }
            }
        })
    }

    fn identity_all_groups(&self, plugin: String, account: i64) -> Fut<Vec<Group>> {
        let db = self.deps.db.clone();
        let plugins = self.plugins.clone();
        Box::pin(async move {
            if !sees_groups(&plugins, &plugin) {
                return Vec::new();
            }
            let account = tether_db::accounts::AccountId(account);
            let offered = async {
                // As core shows them: every group to group admins, all but
                // Internal ones to group managers; everyone else sees
                // Hidden and Internal groups only if they're in them.
                let held = tether_db::permissions::effective(&db, account).await?;
                let every = held.contains(tether_core::permissions::ADMIN_GROUPS);
                let not_internal = held.contains(tether_core::permissions::GROUP_MANAGEMENT);
                tether_db::groups::offered_to(&db, account, every, not_internal).await
            };
            match offered.await {
                Ok(rows) => groups(rows),
                Err(err) => {
                    tracing::error!(error = %err, "plugin groups to offer");
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
        embed: Option<Embed>,
        mention: Mention,
    ) -> Fut<Result<(), DiscordError>> {
        // Refused there a moment ago: the same answer, without asking
        // Discord again or spending one of the plugin's sends. Like a
        // rate-limited send, it reaches nothing, so it isn't logged.
        if let Some(why) = self.refusals.get(&plugin, &channel) {
            return Box::pin(async { Err(DiscordError::NotAllowed(why)) });
        }
        if self.sends.check(plugin.clone(), Instant::now()).is_err() {
            return Box::pin(async { Err(DiscordError::RateLimited) });
        }
        let (deps, plugins, refusals) = (
            self.deps.clone(),
            self.plugins.clone(),
            self.refusals.clone(),
        );
        Box::pin(async move {
            let result = discord_send(
                &deps, &plugins, &refusals, &plugin, &channel, &text, embed, mention,
            )
            .await;
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

    fn http_send(
        &self,
        plugin: String,
        request: HttpRequest,
        from_page: bool,
    ) -> Fut<Result<HttpResponse, HttpError>> {
        let (deps, plugins, http) = (self.deps.clone(), self.plugins.clone(), self.http.clone());
        // Its own task: if the plugin's call is cut off (its deadline), a
        // request already sent still finishes and reaches the log.
        let task = tokio::spawn(async move {
            crate::plugin_http::send(&deps, &plugins, &http, &plugin, request, from_page).await
        });
        Box::pin(async move { task.await.unwrap_or(Err(HttpError::Unavailable)) })
    }

    fn filters_wanted(&self, plugin: String) -> Fut<Vec<FilterWanted>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move { crate::plugin_shared::wanted(&db, &plugins, &plugin).await })
    }

    fn filters_report(
        &self,
        plugin: String,
        name: String,
        config: String,
        values: Vec<FilterValue>,
    ) -> Fut<Result<(), FilterError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_shared::report(&db, &plugins, &plugin, &name, &config, &values).await
        })
    }

    fn timers_publish(&self, plugin: String, timers: Vec<Timer>) -> Fut<Result<(), TimerError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(
            async move { crate::plugin_shared::publish(&db, &plugins, &plugin, &timers).await },
        )
    }

    fn downloads_begin(
        &self,
        plugin: String,
        name: String,
        title: String,
        permission: String,
        header: Vec<String>,
    ) -> Fut<Result<u32, DownloadError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_downloads::begin(
                &db,
                &plugins,
                &plugin,
                &name,
                &title,
                &permission,
                &header,
            )
            .await
        })
    }

    fn downloads_append(
        &self,
        plugin: String,
        name: String,
        build: u32,
        rows: Vec<Vec<String>>,
    ) -> Fut<Result<(), DownloadError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_downloads::append(&db, &plugins, &plugin, &name, build, &rows).await
        })
    }

    fn downloads_finish(
        &self,
        plugin: String,
        name: String,
        build: u32,
    ) -> Fut<Result<(), DownloadError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_downloads::finish(&db, &plugins, &plugin, &name, build).await
        })
    }

    fn downloads_files(&self, plugin: String) -> Fut<Vec<DownloadFile>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move { crate::plugin_downloads::files(&db, &plugins, &plugin).await })
    }

    fn notify_account(
        &self,
        plugin: String,
        account: i64,
        title: String,
        message: String,
        level: NotifyLevel,
    ) -> Fut<Result<bool, NotifyError>> {
        let (db, plugins, limits) = (
            self.deps.db.clone(),
            self.plugins.clone(),
            self.notices.clone(),
        );
        Box::pin(async move {
            crate::plugin_notify::to_account(
                &db, &plugins, &limits, &plugin, account, &title, &message, level,
            )
            .await
        })
    }

    fn notify_holders(
        &self,
        plugin: String,
        permission: String,
        title: String,
        message: String,
        level: NotifyLevel,
        except: Option<i64>,
    ) -> Fut<Result<u32, NotifyError>> {
        let (db, plugins, limits) = (
            self.deps.db.clone(),
            self.plugins.clone(),
            self.notices.clone(),
        );
        Box::pin(async move {
            crate::plugin_notify::to_holders(
                &db,
                &plugins,
                &limits,
                &plugin,
                &permission,
                &title,
                &message,
                level,
                except,
            )
            .await
        })
    }

    fn doctrines_publish(
        &self,
        plugin: String,
        doctrines: Vec<Doctrine>,
        see_all: Option<String>,
    ) -> Fut<Result<(), DoctrineError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_shared::publish_doctrines(
                &db,
                &plugins,
                &plugin,
                &doctrines,
                see_all.as_deref(),
            )
            .await
        })
    }

    fn doctrines_published(
        &self,
        plugin: String,
        account: Option<i64>,
    ) -> Fut<Result<Vec<SharedDoctrine>, DoctrineError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_shared::published_doctrines(&db, &plugins, &plugin, account).await
        })
    }

    fn timers_published(
        &self,
        plugin: String,
        viewer_corporation: Option<i64>,
    ) -> Fut<Result<Vec<SharedTimer>, TimerError>> {
        let (db, plugins) = (self.deps.db.clone(), self.plugins.clone());
        Box::pin(async move {
            crate::plugin_shared::published(&db, &plugins, &plugin, viewer_corporation).await
        })
    }
}
