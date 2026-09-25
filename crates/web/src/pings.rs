//! Fleet pings (F13): a message to a Discord channel an admin chose,
//! pinging @here, @everyone or a Tether-managed role. Only that target can
//! ping: Discord ignores user and role mentions typed into the message
//! (`allowed_mentions`), and typed @everyone and @here are defused in the
//! text, since Discord allows both or neither.
//!
//! A ping is recorded together with a delayed retry job, then sent straight
//! away; a successful send makes the job a no-op. A ping more than 15
//! minutes old is no use to anyone and is dropped instead of sent late.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tether_core::crypto::EncryptionKey;
use tether_db::accounts::{self, AccountId};
use tether_db::audit::{self, Actor};
use tether_db::pings::{self as db, NewPing, PingChannel, Target};
use tether_db::{PgPool, discord as discord_db};
use tether_discord::store;
use tether_discord::{Discord, DiscordError, Mention};
use tether_jobs::{JobError, NewJob, Registry};

use crate::AppState;
use crate::discord::{config, discord_error};
use crate::error::AppError;

pub const PING_JOB: &str = "discord.ping";
pub const MAX_MESSAGE: usize = 1500;
/// Per account.
pub const PINGS_PER_WINDOW: i64 = 5;
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
/// Retried pings older than this are dropped.
pub const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// Makes a Discord channel a ping channel. The name comes from Discord.
pub async fn add_channel(
    state: &AppState,
    actor: AccountId,
    channel_id: &str,
) -> Result<(), AppError> {
    let id: u64 = channel_id
        .trim()
        .parse()
        .map_err(|_| AppError::bad_request("Choose a channel."))?;
    let config = config(state).await?;
    let channel = state
        .discord
        .text_channels(&config)
        .await
        .map_err(discord_error)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::not_found("That isn't a text channel on the server."))?;
    let stored = i64::try_from(channel.id).map_err(AppError::internal)?;
    let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
    let mut tx = state.db.begin().await?;
    if !db::add_channel(&mut *tx, stored, guild, &channel.name).await? {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That's already a ping channel.",
        ));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.channel.add",
        Some(&format!("channel:{stored}")),
        json!({ "name": channel.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_channel(
    state: &AppState,
    actor: AccountId,
    channel_id: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let channel = db::remove_channel(&mut *tx, channel_id)
        .await?
        .ok_or_else(|| AppError::not_found("No such ping channel."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.channel.remove",
        Some(&format!("channel:{channel_id}")),
        json!({ "name": channel.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Who a ping may target: nobody, @here, @everyone, or a role Tether
/// manages (the ones mapped to tiers and groups).
pub async fn targets(state: &AppState) -> Result<Vec<Target>, AppError> {
    let mut targets = vec![Target::None, Target::Here, Target::Everyone];
    let mut seen = std::collections::HashSet::new();
    for mapping in discord_db::mappings(&state.db).await? {
        if seen.insert(mapping.role_id) {
            targets.push(Target::Role {
                id: mapping.role_id,
                name: mapping.role_name,
            });
        }
    }
    Ok(targets)
}

/// A `<select>` value: `none`, `here`, `everyone` or `role:<id>`.
pub fn target_value(target: &Target) -> String {
    match target {
        Target::None => "none".to_owned(),
        Target::Here => "here".to_owned(),
        Target::Everyone => "everyone".to_owned(),
        Target::Role { id, .. } => format!("role:{id}"),
    }
}

/// Records the ping and sends it. Returns its id.
pub async fn send(
    state: &AppState,
    actor: AccountId,
    channel_id: &str,
    target: &str,
    message: &str,
) -> Result<i64, AppError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(AppError::bad_request("Write a message."));
    }
    if message.chars().count() > MAX_MESSAGE {
        return Err(AppError::bad_request(format!(
            "Pings are at most {MAX_MESSAGE} characters."
        )));
    }
    let channel_id: i64 = channel_id
        .trim()
        .parse()
        .map_err(|_| AppError::bad_request("Choose a channel."))?;
    let config = config(state).await?;
    let channel = channels_for(state, &config)
        .await?
        .into_iter()
        .find(|c| c.channel_id == channel_id)
        .ok_or_else(|| AppError::bad_request("That isn't a ping channel."))?;
    let target = targets(state)
        .await?
        .into_iter()
        .find(|t| target_value(t) == target)
        .ok_or_else(|| AppError::bad_request("Choose who to ping."))?;
    let sender = accounts::get(&state.db, actor)
        .await?
        .ok_or_else(AppError::unauthorized)?
        .main
        .name;
    let nonce = format!(
        "tp-{}",
        &tether_core::new_token()
            .map_err(AppError::internal)?
            .expose()[..20]
    );

    let mut tx = state.db.begin().await?;
    // Counted under the sender's row lock, so parallel sends can't all
    // slip under the limit.
    db::lock_sender(&mut tx, actor).await?;
    if db::sent_since(&mut *tx, actor, WINDOW.as_secs_f64()).await? >= PINGS_PER_WINDOW {
        return Err(AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "That's {PINGS_PER_WINDOW} pings in the last {} minutes. Wait a little.",
                WINDOW.as_secs() / 60
            ),
        ));
    }
    let id = db::insert(
        &mut *tx,
        NewPing {
            account: actor,
            sender_name: &sender,
            channel: &channel,
            target: &target,
            message,
            nonce: &nonce,
        },
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "ping.send",
        Some(&format!("ping:{id}")),
        json!({ "channel": channel.name, "target": target_value(&target), "length": message.chars().count() }),
    )
    .await?;
    // The retry, queued with the ping so it can't be lost; sending now
    // makes it a no-op. Its attempts outlast STALE_AFTER, so a ping that
    // never goes out ends up marked failed.
    tether_jobs::enqueue(
        &mut *tx,
        NewJob::new(PING_JOB, json!({ "ping_id": id }))
            .max_attempts(10)
            .run_at(chrono::Utc::now() + chrono::Duration::seconds(60)),
    )
    .await?;
    tx.commit().await?;

    match deliver(&state.db, &state.key, &state.discord, id).await {
        Ok(()) => {}
        Err(JobError::Retry(reason)) => {
            tracing::info!(ping = id, reason, "fleet ping will be retried");
        }
        Err(JobError::Permanent(reason)) => {
            tracing::warn!(ping = id, reason, "fleet ping failed");
        }
        // deliver never defers; the retry job would pick it up anyway.
        Err(JobError::Defer(_)) => {}
    }
    Ok(id)
}

/// The ping channels on the configured server.
pub async fn channels_for(
    state: &AppState,
    config: &tether_discord::DiscordConfig,
) -> Result<Vec<PingChannel>, AppError> {
    let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
    Ok(db::channels(&state.db, guild).await?)
}

/// Breaks @everyone and @here typed into text with a zero-width space:
/// `allowed_mentions` can't allow @here but not @everyone.
pub(crate) fn defuse(text: &str) -> String {
    text.replace("@everyone", "@\u{200B}everyone")
        .replace("@here", "@\u{200B}here")
}

/// The message as posted.
pub fn content(ping: &db::Ping) -> String {
    let mention = mention(&ping.target).prefix();
    let mut text = String::new();
    if !mention.is_empty() {
        text.push_str(&mention);
        text.push('\n');
    }
    text.push_str(&defuse(&ping.message));
    text.push_str("\n— ");
    text.push_str(&defuse(&ping.sender_name));
    text
}

fn mention(target: &Target) -> Mention {
    match target {
        Target::None => Mention::None,
        Target::Here => Mention::Here,
        Target::Everyone => Mention::Everyone,
        Target::Role { id, .. } => u64::try_from(*id).map_or(Mention::None, Mention::Role),
    }
}

/// Sends a recorded ping, once. Transient failures are `Retry`; anything
/// else (or a ping gone stale) is recorded as failed.
pub async fn deliver(
    db_pool: &PgPool,
    key: &EncryptionKey,
    discord: &Discord,
    ping_id: i64,
) -> Result<(), JobError> {
    let Some(ping) = db::get(db_pool, ping_id, STALE_AFTER)
        .await
        .map_err(JobError::retry)?
    else {
        return Ok(());
    };
    if ping.sent_at.is_some() || ping.failed_at.is_some() {
        return Ok(());
    }
    let fail = |reason: String| async move {
        db::mark_error(db_pool, ping_id, &reason, true)
            .await
            .map_err(JobError::retry)?;
        Err::<(), _>(JobError::permanent(reason))
    };
    if ping.stale {
        return fail("Discord was unavailable for too long; not sent".to_owned()).await;
    }
    let Some(config) = store::load(db_pool, key).await.map_err(JobError::retry)? else {
        return fail("Discord isn't set up".to_owned()).await;
    };
    // Removed as a ping channel, or Tether moved to another server, since
    // it was queued.
    let guild = i64::try_from(config.guild_id).map_err(JobError::permanent)?;
    if !db::is_channel(db_pool, ping.channel_id, guild)
        .await
        .map_err(JobError::retry)?
    {
        return fail("That channel is no longer a ping channel; not sent".to_owned()).await;
    }
    let channel = u64::try_from(ping.channel_id).map_err(JobError::permanent)?;
    let nonce = ping.nonce.clone();
    match discord
        .send_message(
            &config,
            channel,
            &content(&ping),
            mention(&ping.target),
            &nonce,
        )
        .await
    {
        Ok(message_id) => {
            let message_id = i64::try_from(message_id).map_err(JobError::permanent)?;
            db::mark_sent(db_pool, ping_id, message_id)
                .await
                .map_err(JobError::retry)?;
            tracing::info!(
                ping = ping_id,
                channel = ping.channel_name,
                "fleet ping sent"
            );
            Ok(())
        }
        Err(err) if err.is_transient() => {
            db::mark_error(db_pool, ping_id, &err.to_string(), false)
                .await
                .map_err(JobError::retry)?;
            Err(JobError::retry(err))
        }
        Err(err) => fail(explain(&err)).await,
    }
}

fn explain(err: &DiscordError) -> String {
    match err.code() {
        Some(tether_discord::codes::MISSING_ACCESS | tether_discord::codes::MISSING_PERMISSIONS) => {
            "The bot can't post in that channel: give it View Channel and Send Messages there (and Mention Everyone for @everyone)".to_owned()
        }
        Some(10003) => "That channel no longer exists".to_owned(),
        _ => err.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct PingJob {
    ping_id: i64,
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    key: EncryptionKey,
    discord: Arc<Discord>,
) {
    registry.register(PING_JOB, move |job| {
        let (db, key, discord) = (db.clone(), key.clone(), discord.clone());
        async move {
            let payload: PingJob =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            deliver(&db, &key, &discord, payload.ping_id).await
        }
    });
}

/// For the admin page: ping channels and the server's text channels not
/// yet chosen.
pub async fn channel_options(
    state: &AppState,
) -> Result<(Vec<PingChannel>, Vec<tether_discord::TextChannel>), AppError> {
    let config = match config(state).await {
        Ok(config) => config,
        Err(_) => return Ok((Vec::new(), Vec::new())),
    };
    let chosen = channels_for(state, &config).await?;
    let available = match state.discord.text_channels(&config).await {
        Ok(channels) => channels
            .into_iter()
            .filter(|c| {
                !chosen
                    .iter()
                    .any(|p| i64::try_from(c.id) == Ok(p.channel_id))
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "listing Discord channels failed");
            Vec::new()
        }
    };
    Ok((chosen, available))
}
