//! Discord setup, role mappings and account linking (F12). Callers check
//! permissions; changes are audited in the same transaction.
//!
//! Linking: the member approves `identify guilds.join` at Discord, the
//! callback trades the code for their access token, learns who they are,
//! and the bot adds them to the server with the roles mapped to their state
//! and groups. The token is then revoked; it is never stored.
//!
//! Only pilots with Discord access (a permission, AA's
//! `discord.access_discord`) join the server through Tether; losing it
//! unlinks them, and an unlinked member is removed from the server.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use serde_json::json;
use tether_core::crypto::EncryptionKey;
use tether_core::{Secret, hash_token, new_token};
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::discord as db;
use tether_db::permissions::Grantee;
use tether_db::{PgPool, groups};
use tether_discord::store::{self, StoreError};
use tether_discord::{Discord, DiscordConfig, DiscordError, GuildCheck, Joined, UserToken};
use tether_jobs::{JobError, Registry};

use crate::AppState;
use crate::auth::{cookie, removal};
use crate::error::{AppError, is_foreign_key_violation, is_unique_violation};

pub const REMOVE_MEMBER_JOB: &str = "discord.remove_member";
/// Binds a pending link to the browser that started it.
pub const LINK_COOKIE: &str = "__Host-tether_discord";
const LINK_TTL: Duration = Duration::from_secs(10 * 60);

/// What the settings form sends. Blank secrets keep the saved ones.
#[derive(Default, Deserialize)]
pub struct SettingsInput {
    #[serde(default)]
    pub application_id: String,
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub bot_token: String,
}

impl std::fmt::Debug for SettingsInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsInput")
            .field("application_id", &self.application_id)
            .field("guild_id", &self.guild_id)
            .field("client_secret", &"[redacted]")
            .field("bot_token", &"[redacted]")
            .finish()
    }
}

fn store_error(err: StoreError) -> AppError {
    AppError::internal(err)
}

/// A Discord failure as the admin or member should see it. Messages never
/// include secrets (see `DiscordError`).
pub fn discord_error(err: DiscordError) -> AppError {
    tracing::warn!(error = %err, "Discord request failed");
    if err.is_transient() {
        return AppError::new(
            StatusCode::BAD_GATEWAY,
            "Discord didn't answer. Try again in a moment.",
        );
    }
    AppError::bad_request(format!("{err}."))
}

fn parse_id(value: &str, what: &'static str) -> Result<u64, AppError> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            AppError::bad_request(format!(
                "The {what} is a number; copy it from Discord (see the steps above)."
            ))
        })
}

pub async fn stored(state: &AppState) -> Result<store::Stored, AppError> {
    store::stored(&state.db, &state.key)
        .await
        .map_err(store_error)
}

/// The configuration, or 503 if Discord isn't set up.
pub async fn config(state: &AppState) -> Result<DiscordConfig, AppError> {
    store::load(&state.db, &state.key)
        .await
        .map_err(store_error)?
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "Discord isn't set up yet."))
}

pub async fn is_configured(state: &AppState) -> Result<bool, AppError> {
    Ok(stored(state).await?.config().is_some())
}

/// Discord access is a permission (AA's `discord.access_discord`,
/// granted to Member and Blue by default).
pub async fn may_join(state: &AppState, account: AccountId) -> Result<bool, AppError> {
    has_access(&state.db, account).await
}

pub async fn has_access(db: &PgPool, account: AccountId) -> Result<bool, AppError> {
    Ok(tether_db::permissions::effective(db, account)
        .await?
        .contains(tether_core::permissions::DISCORD_ACCESS))
}

fn guests_cannot_join() -> AppError {
    AppError::new(
        StatusCode::FORBIDDEN,
        "Your access doesn't include the Discord service. It comes from your main's state (Member and Blue have it by default) and your groups.",
    )
}

/// Validates the settings against Discord, then saves them.
pub async fn save_settings(
    state: &AppState,
    actor: AccountId,
    input: &SettingsInput,
) -> Result<GuildCheck, AppError> {
    crate::sudo::check(crate::sudo::Action::DiscordSettings)?;
    let application_id = parse_id(&input.application_id, "application id")?;
    let guild_id = parse_id(&input.guild_id, "server id")?;
    let saved = stored(state).await?;
    let (client_secret, secret_changed) = match input.client_secret.trim() {
        "" => (saved.client_secret, false),
        new => (Some(Secret::new(new.to_owned())), true),
    };
    let (bot_token, token_changed) = match input.bot_token.trim() {
        "" => (saved.bot_token, false),
        new => (Some(Secret::new(new.to_owned())), true),
    };
    let client_secret =
        client_secret.ok_or_else(|| AppError::bad_request("Enter the client secret."))?;
    let bot_token = bot_token.ok_or_else(|| AppError::bad_request("Enter the bot token."))?;
    if client_secret.expose().len() > 200 || bot_token.expose().len() > 200 {
        return Err(AppError::bad_request(
            "That's too long for a client secret or bot token.",
        ));
    }
    let config = DiscordConfig {
        application_id,
        client_secret,
        bot_token,
        guild_id,
    };
    let check = state.discord.check(&config).await.map_err(discord_error)?;

    let mut tx = state.db.begin().await?;
    store::save(&mut tx, &state.key, &config)
        .await
        .map_err(store_error)?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "discord.settings",
        None,
        json!({
            "application_id": application_id.to_string(),
            "guild_id": guild_id.to_string(),
            "client_secret_changed": secret_changed,
            "bot_token_changed": token_changed,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(check)
}

/// Sets a state's Name Formatter format (empty returns it to AA's default,
/// `{character_name}`) and queues a sync of every linked member.
pub async fn save_name_format(
    state: &AppState,
    actor: AccountId,
    state_id: tether_core::states::StateId,
    format: &str,
) -> Result<(), AppError> {
    let format = format.trim();
    if !format.is_empty() {
        tether_core::nickname::validate(format).map_err(AppError::bad_request)?;
    }
    let mut tx = state.db.begin().await?;
    if tether_db::states::get(&mut *tx, state_id)
        .await?
        .is_none_or(|s| s.is_blacklist())
    {
        return Err(AppError::not_found("No such state."));
    }
    db::set_name_format(&mut *tx, state_id, (!format.is_empty()).then_some(format)).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "discord.name_format",
        Some(&format!("state:{}", state_id.0)),
        json!({ "format": format }),
    )
    .await?;
    queue_sync_all(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// The Discord service's two switches, as AA's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// AA's `DISCORD_SYNC_NAMES`: set members' nicknames.
    pub sync_names: bool,
    /// Remove every role Tether doesn't map to a member (except Discord's
    /// own roles and reserved group names).
    pub strip_unmapped: bool,
}

pub async fn options(db: &PgPool) -> Result<Options, AppError> {
    Ok(Options {
        sync_names: tether_db::settings::get_bool_or(
            db,
            tether_db::settings::DISCORD_SYNC_NAMES,
            true,
        )
        .await?,
        strip_unmapped: tether_db::settings::get_bool(
            db,
            tether_db::settings::DISCORD_STRIP_UNMAPPED,
        )
        .await?,
    })
}

pub async fn save_options(
    state: &AppState,
    actor: AccountId,
    new: Options,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    tether_db::settings::set(
        &mut *tx,
        tether_db::settings::DISCORD_SYNC_NAMES,
        json!(new.sync_names),
    )
    .await?;
    tether_db::settings::set(
        &mut *tx,
        tether_db::settings::DISCORD_STRIP_UNMAPPED,
        json!(new.strip_unmapped),
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "discord.options",
        None,
        json!({ "sync_names": new.sync_names, "strip_unmapped": new.strip_unmapped }),
    )
    .await?;
    queue_sync_all(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn queue_sync_all(tx: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    tether_jobs::enqueue(
        &mut *tx,
        tether_jobs::NewJob::new(crate::discord_sync::SYNC_ALL_JOB, json!({})).max_attempts(10),
    )
    .await?;
    Ok(())
}

/// Maps a Discord role to a state or group. Only roles the bot can give;
/// never Administrator; and moderation or management roles never to Guest
/// or an Open group, which anyone can be in.
pub async fn add_mapping(
    state: &AppState,
    actor: AccountId,
    role_id: &str,
    grantee: Grantee,
) -> Result<(), AppError> {
    let role_id = parse_id(role_id, "role")?;
    let config = config(state).await?;
    let check = state.discord.check(&config).await.map_err(discord_error)?;
    let role = check
        .roles
        .into_iter()
        .find(|r| r.id == role_id)
        .ok_or_else(|| AppError::not_found("That role isn't on the server."))?;
    if role.administrator {
        return Err(AppError::bad_request(
            "Tether won't hand out a role with Administrator.",
        ));
    }
    if !role.assignable {
        return Err(AppError::bad_request(format!(
            "The bot can't give {}: it belongs to an integration, or sits above the bot's own role. \
             In Server Settings → Roles, drag the bot's role above it.",
            role.name
        )));
    }
    let stored_id = i64::try_from(role.id).map_err(AppError::internal)?;
    let mut tx = state.db.begin().await?;
    let open = match grantee {
        Grantee::State(id) => tether_db::states::get(&mut *tx, id)
            .await?
            .filter(|s| !s.is_blacklist())
            .ok_or_else(|| AppError::not_found("No such state."))?
            .is_guest(),
        Grantee::Group(group) => {
            groups::lock(&mut tx, group, false).await?;
            let group = groups::get(&mut *tx, group)
                .await?
                .ok_or_else(|| AppError::not_found("No such group."))?;
            group.flags.anyone_can_join()
        }
    };
    if role.privileged && open {
        return Err(AppError::bad_request(format!(
            "{} has moderation or server-management permissions, so it can't go to Guest or an Open group: anyone can be in those.",
            role.name
        )));
    }
    let id = match db::add_mapping(&mut *tx, stored_id, &role.name, grantee).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "That role is already mapped there.",
            ));
        }
        Err(err) if is_foreign_key_violation(&err) => {
            return Err(AppError::not_found("No such group."));
        }
        Err(err) => return Err(err.into()),
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "discord.mapping.add",
        Some(&format!("mapping:{id}")),
        mapping_details(stored_id, &role.name, grantee),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_mapping(state: &AppState, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let mapping = db::remove_mapping(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No such mapping."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "discord.mapping.remove",
        Some(&format!("mapping:{id}")),
        mapping_details(mapping.role_id, &mapping.role_name, mapping.grantee),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

fn mapping_details(role_id: i64, role_name: &str, grantee: Grantee) -> serde_json::Value {
    match grantee {
        Grantee::State(state) => {
            json!({ "role_id": role_id.to_string(), "role": role_name, "state_id": state.0 })
        }
        Grantee::Group(group) => {
            json!({ "role_id": role_id.to_string(), "role": role_name, "group_id": group.0 })
        }
    }
}

/// How long a guild check is reused while linking and syncing.
pub(crate) const CHECK_TTL: Duration = Duration::from_secs(60);

/// The roles to give an account now, checked against the server as it is:
/// a role that has since gained Administrator, moved above the bot, or
/// gained moderation powers while only Guest or Open groups get it, is
/// skipped (and logged) rather than handed out.
pub(crate) fn grantable(
    wanted: &[db::RoleFor],
    check: &GuildCheck,
    account: AccountId,
) -> Vec<u64> {
    let mut roles = Vec::new();
    for want in wanted {
        let Ok(id) = u64::try_from(want.role_id) else {
            continue;
        };
        match check.roles.iter().find(|r| r.id == id) {
            Some(r) if r.assignable && !r.administrator && !(r.privileged && want.open_only) => {
                roles.push(id);
            }
            _ => tracing::warn!(
                account = account.0,
                role_id = id,
                "not giving a mapped Discord role: it's gone, the bot can't give it, or it's now too powerful for who it's mapped to"
            ),
        }
    }
    roles
}

async fn grantable_roles(
    state: &AppState,
    config: &DiscordConfig,
    account: AccountId,
) -> Result<Vec<u64>, AppError> {
    let wanted = db::roles_for(&state.db, account).await?;
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    // Fresh, not cached: a role that just gained Administrator must not
    // go out to someone linking now.
    let check = state.discord.check(config).await.map_err(discord_error)?;
    Ok(grantable(&wanted, &check, account))
}

/// Starts linking: returns the cookie jar and where to send the browser.
pub async fn begin_link(
    state: &AppState,
    account: AccountId,
    jar: CookieJar,
) -> Result<(CookieJar, String), AppError> {
    let config = config(state).await?;
    if !may_join(state, account).await? {
        return Err(guests_cannot_join());
    }
    let browser = new_token().map_err(AppError::internal)?;
    let oauth_state = new_token().map_err(AppError::internal)?;
    db::insert_attempt(
        &state.db,
        oauth_state.expose(),
        &hash_token(browser.expose()),
        account,
        LINK_TTL,
    )
    .await?;
    let url = Discord::authorize_url(
        config.application_id,
        &state.site.discord_callback_url(),
        oauth_state.expose(),
    );
    Ok((jar.add(cookie(LINK_COOKIE, &browser, LINK_TTL)?), url))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

impl std::fmt::Debug for CallbackQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackQuery")
            .field("code", &self.code.as_ref().map(|_| "[redacted]"))
            .field("state", &self.state.as_ref().map(|_| "[redacted]"))
            .field("error", &self.error)
            .finish()
    }
}

/// Finishes linking for the signed-in `account`. Returns the jar (link
/// cookie removed) and the result.
pub async fn finish_link(
    state: &AppState,
    account: Option<AccountId>,
    jar: CookieJar,
    query: CallbackQuery,
) -> (CookieJar, Result<Joined, AppError>) {
    let browser = jar.get(LINK_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(removal(LINK_COOKIE));
    (jar, link(state, account, browser, query).await)
}

async fn link(
    state: &AppState,
    account: Option<AccountId>,
    browser: Option<String>,
    query: CallbackQuery,
) -> Result<Joined, AppError> {
    let expired = || {
        AppError::bad_request(
            "This link has expired or was already used. Start again from your profile.",
        )
    };
    if let Some(error) = query.error {
        tracing::info!(error, "Discord linking cancelled or refused");
        return Err(AppError::bad_request("Linking was cancelled."));
    }
    let (Some(code), Some(oauth_state), Some(browser)) = (query.code, query.state, browser) else {
        return Err(expired());
    };
    let Some(attempt) = db::take_attempt(&state.db, &oauth_state, &hash_token(&browser)).await?
    else {
        return Err(expired());
    };
    // The account that started the link must be the one finishing it.
    if account != Some(attempt) {
        return Err(expired());
    }
    let config = config(state).await?;
    let token = state
        .discord
        .exchange_code(&config, &code, &state.site.discord_callback_url())
        .await
        .map_err(discord_error)?;
    let result = link_with_token(state, attempt, &config, &token).await;
    // Done with it either way; failing to revoke only leaves it to expire.
    if let Err(err) = state.discord.revoke(&config, &token).await {
        tracing::warn!(error = %err, "revoking the Discord token failed");
    }
    result
}

async fn link_with_token(
    state: &AppState,
    account: AccountId,
    config: &DiscordConfig,
    token: &UserToken,
) -> Result<Joined, AppError> {
    let taken = || {
        AppError::new(
            StatusCode::CONFLICT,
            "That Discord account is linked to another pilot's account. Unlink it there first.",
        )
    };
    // The state may have changed since the link started.
    if !may_join(state, account).await? {
        return Err(guests_cannot_join());
    }
    let user = state
        .discord
        .current_user(token)
        .await
        .map_err(discord_error)?;
    let user_id = i64::try_from(user.id).map_err(AppError::internal)?;
    let roles = grantable_roles(state, config, account).await?;

    // Claim the link in a short transaction, under the user's lock so a
    // queued role removal can't interleave. No transaction is held across
    // Discord calls: a slow Discord must not tie up database connections.
    let mut tx = state.db.begin().await?;
    db::lock_user(&mut tx, user_id).await?;
    if db::account_for_user(&mut *tx, user_id)
        .await?
        .is_some_and(|a| a != account)
    {
        return Err(taken());
    }
    let relinking_same_user = db::link_for(&mut *tx, account)
        .await?
        .is_some_and(|l| l.discord_user_id == user_id);
    match db::set_link(&mut tx, account, user_id, user.display_name()).await {
        Ok(()) => {}
        Err(err) if is_unique_violation(&err) => return Err(taken()),
        Err(err) => return Err(err.into()),
    }
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "discord.link",
        Some(&format!("account:{}", account.0)),
        json!({
            "discord_user_id": user.id.to_string(),
            "username": user.username,
            "roles": roles.len(),
        }),
    )
    .await?;
    tx.commit().await?;

    // Someone already in the server who fails to link keeps their place:
    // only Tether's roles are undone. If Discord can't say, assume so.
    let already_in = state
        .discord
        .member(config, user.id)
        .await
        .map_or(true, |m| m.is_some());
    match state.discord.join(config, user.id, token, &roles).await {
        Ok(joined) => {
            tracing::info!(
                account = account.0,
                discord_user_id = user.id,
                ?joined,
                "Discord linked"
            );
            // Sets the nickname, and tidies roles if they were already in
            // the server.
            db::queue_sync(&state.db, account).await?;
            Ok(joined)
        }
        Err(err) => {
            // Some roles may have been given (or Discord applied a join we
            // timed out on). Undo the link: its delete trigger queues taking
            // them back. Someone relinking the account they already had
            // keeps it, and with it the roles they're due.
            if !relinking_same_user {
                undo_link(state, account, user_id, !already_in).await?;
            }
            Err(join_error(err))
        }
    }
}

/// Undoes a link whose join failed. `kick`: they weren't in the server
/// before, so they leave it; otherwise only Tether's roles go.
async fn undo_link(
    state: &AppState,
    account: AccountId,
    user_id: i64,
    kick: bool,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    db::lock_user(&mut tx, user_id).await?;
    if db::link_for(&mut *tx, account)
        .await?
        .is_some_and(|l| l.discord_user_id == user_id)
    {
        db::unlink(&mut *tx, account).await?;
        if !kick {
            db::keep_in_server(&mut *tx, user_id).await?;
        }
        audit::record(
            &mut *tx,
            Actor::System,
            "discord.unlink",
            Some(&format!("account:{}", account.0)),
            json!({ "discord_user_id": user_id.to_string(), "reason": "joining the server failed" }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn join_error(err: DiscordError) -> AppError {
    match err.code() {
        Some(
            tether_discord::codes::MISSING_PERMISSIONS | tether_discord::codes::MISSING_ACCESS,
        ) => {
            tracing::warn!(error = %err, "the bot can't add members or give roles");
            AppError::new(
                StatusCode::BAD_GATEWAY,
                "The Discord bot isn't allowed to add you or give your roles. Ask an admin to check the Discord page.",
            )
        }
        _ => discord_error(err),
    }
}

/// Unlinks the account. The table's trigger queues removing its roles.
pub async fn unlink(state: &AppState, account: AccountId) -> Result<bool, AppError> {
    let mut tx = state.db.begin().await?;
    let Some(link) = db::unlink(&mut *tx, account).await? else {
        return Ok(false);
    };
    audit::record(
        &mut *tx,
        Actor::Account(account),
        "discord.unlink",
        Some(&format!("account:{}", account.0)),
        json!({ "discord_user_id": link.discord_user_id.to_string(), "username": link.username }),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct RemoveMember {
    discord_user_id: i64,
    /// False when a failed link undoes itself for someone who was already
    /// in the server: only Tether's roles go.
    #[serde(default = "yes")]
    kick: bool,
}

/// Removes a Discord user who is no longer linked from the server (AA:
/// losing access, or unlinking, kicks). If the bot may not kick them (no
/// Kick Members, or they're the server owner), it takes every role Tether
/// hands out instead. Holds the user's lock throughout, so a relink either
/// finished first (and is left alone) or waits until this is done. This
/// holds a connection across Discord calls; that's fine because job
/// workers (JOB_WORKERS, default 4) stay well below the pool size.
pub async fn remove_member(
    db_pool: &PgPool,
    key: &EncryptionKey,
    discord: &Discord,
    discord_user_id: i64,
    kick: bool,
) -> Result<(), JobError> {
    let Some(config) = store::load(db_pool, key).await.map_err(JobError::retry)? else {
        tracing::info!(discord_user_id, "Discord isn't set up; nobody to remove");
        return Ok(());
    };
    let user = u64::try_from(discord_user_id).map_err(JobError::permanent)?;
    let mut tx = db_pool.begin().await.map_err(JobError::retry)?;
    db::lock_user(&mut tx, discord_user_id)
        .await
        .map_err(JobError::retry)?;
    if db::account_for_user(&mut *tx, discord_user_id)
        .await
        .map_err(JobError::retry)?
        .is_some()
    {
        return Ok(());
    }
    let result = remove(db_pool, discord, &config, user, kick).await;
    tx.commit().await.map_err(JobError::retry)?;
    result
}

async fn remove(
    db_pool: &PgPool,
    discord: &Discord,
    config: &DiscordConfig,
    user: u64,
    kick: bool,
) -> Result<(), JobError> {
    let check = discord
        .check_cached(config, CHECK_TTL)
        .await
        .map_err(crate::discord_sync::discord_failure)?;
    if kick && user != check.owner_id {
        match discord.kick(config, user).await {
            Ok(()) => {
                tracing::info!(discord_user_id = user, "removed from the Discord server");
                return Ok(());
            }
            Err(err) if err.code() == Some(tether_discord::codes::MISSING_PERMISSIONS) => {
                tracing::warn!(
                    discord_user_id = user,
                    "the bot may not kick this member (give it Kick Members, above their roles); taking Tether's roles instead"
                );
            }
            Err(err) => return Err(crate::discord_sync::discord_failure(err)),
        }
    }
    let roles: Vec<u64> = db::mapped_role_ids(db_pool)
        .await
        .map_err(JobError::retry)?
        .into_iter()
        .filter_map(|r| u64::try_from(r).ok())
        .collect();
    strip(db_pool, discord, config, user, &roles).await
}

async fn strip(
    db_pool: &PgPool,
    discord: &Discord,
    config: &DiscordConfig,
    user: u64,
    roles: &[u64],
) -> Result<(), JobError> {
    let refused = discord
        .remove_roles(config, user, roles)
        .await
        .map_err(crate::discord_sync::discord_failure)?;
    // A Tether nickname ("[NMU] Name") would still say they belong.
    let sync_names =
        tether_db::settings::get_bool_or(db_pool, tether_db::settings::DISCORD_SYNC_NAMES, true)
            .await
            .map_err(JobError::retry)?;
    if sync_names {
        match discord.set_nick(config, user, None).await {
            Ok(()) => {}
            Err(err)
                if matches!(
                    err.code(),
                    Some(
                        tether_discord::codes::UNKNOWN_MEMBER
                            | tether_discord::codes::MISSING_PERMISSIONS
                    )
                ) => {}
            Err(err) => return Err(crate::discord_sync::discord_failure(err)),
        }
    }
    // Nothing else remembers these roles must go: retry (and in the end
    // dead-letter, where an admin sees it) rather than count it as done.
    if !refused.is_empty() {
        return Err(JobError::retry(format!(
            "Discord refused to take {} role(s); move the bot's role above them",
            refused.len()
        )));
    }
    Ok(())
}

/// Takes Discord away from an account that lost access (AA): unlinks it,
/// which removes it from the server, audited and notified.
pub async fn revoke_access(db_pool: &PgPool, account: AccountId) -> Result<bool, sqlx::Error> {
    let Some(seen) = db::link_for(db_pool, account).await? else {
        return Ok(false);
    };
    let mut tx = db_pool.begin().await?;
    // Under the user's lock, like linking: a relink to someone else, or
    // access regained, since the check is left alone.
    db::lock_user(&mut tx, seen.discord_user_id).await?;
    if tether_db::permissions::effective_in(&mut tx, account)
        .await?
        .contains(tether_core::permissions::DISCORD_ACCESS)
    {
        return Ok(false);
    }
    let Some(link) = db::unlink_user(&mut *tx, account, seen.discord_user_id).await? else {
        return Ok(false);
    };
    audit::record(
        &mut *tx,
        Actor::System,
        "discord.access_removed",
        Some(&format!("account:{}", account.0)),
        json!({ "discord_user_id": link.discord_user_id.to_string(), "username": link.username }),
    )
    .await?;
    tether_db::notifications::notify(
        &mut tx,
        account,
        tether_db::notifications::Level::Warning,
        "Discord Account Disabled",
        Some("Your Discord account was disabled as you no longer meet the access requirements."),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(account = account.0, "Discord access removed");
    Ok(true)
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    key: EncryptionKey,
    discord: Arc<Discord>,
) {
    registry.register(REMOVE_MEMBER_JOB, move |job| {
        let (db, key, discord) = (db.clone(), key.clone(), discord.clone());
        async move {
            let payload: RemoveMember =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            remove_member(&db, &key, &discord, payload.discord_user_id, payload.kick).await
        }
    });
}
