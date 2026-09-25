//! Keeping Discord in step with states and groups (F12): each linked member
//! gets exactly the Tether-managed roles their state and groups map to, and
//! the nickname template if one is set. Roles Tether doesn't manage are
//! left alone, and leaving the server isn't tracked.
//!
//! Database triggers queue `discord.sync_member` whenever an account's
//! state, main, groups or main's corporation change, and `discord.sync_all`
//! when a mapping is added or removed. A daily sync catches anything done
//! by hand in Discord.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tether_core::crypto::EncryptionKey;
use tether_core::nickname::{self, Parts};
use tether_core::states::EntityKind;
use tether_db::accounts::AccountId;
use tether_db::discord as db;
use tether_db::{PgPool, settings};
use tether_discord::store;
use tether_discord::{Discord, DiscordConfig, DiscordError, codes};
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, NewJob, Registry};

use crate::discord::{CHECK_TTL, grantable};

pub const SYNC_MEMBER_JOB: &str = "discord.sync_member";
pub const SYNC_ALL_JOB: &str = "discord.sync_all";

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        "discord.sync_all",
        SYNC_ALL_JOB,
        Duration::from_secs(24 * 60 * 60),
    )]
}

/// What syncing needs, shared by the job handlers.
#[derive(Clone)]
pub struct SyncContext {
    pub db: PgPool,
    pub key: EncryptionKey,
    pub discord: Arc<Discord>,
    pub esi: Esi,
}

#[derive(Debug, Deserialize)]
struct SyncMember {
    account_id: i64,
    /// Roles whose mapping was just removed: still taken back from anyone
    /// who no longer gets them some other way.
    #[serde(default)]
    removed_role_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct SyncAll {
    #[serde(default)]
    removed_role_id: Option<i64>,
}

/// Anything that might be fixed later is retried: an outage, and also a
/// bad bot token or incomplete setup, since a dead job would forget any
/// role it was meant to take back.
pub(crate) fn discord_failure(err: DiscordError) -> JobError {
    match err {
        DiscordError::Unavailable(_) | DiscordError::BadBotToken | DiscordError::Config(_) => {
            JobError::retry(err)
        }
        err => JobError::permanent(err),
    }
}

/// The nickname for the account, if a template is set. Tickers come from
/// ESI through the cache (stale ones if ESI is down).
async fn nickname_for(
    ctx: &SyncContext,
    account: AccountId,
) -> Result<Option<String>, tether_esi::names::NamesError> {
    let Some(template) = settings::get_string(&ctx.db, settings::DISCORD_NICKNAME_TEMPLATE)
        .await?
        .filter(|t| !t.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(main) = db::main_character(&ctx.db, account).await? else {
        return Ok(None);
    };
    let mut corp = String::new();
    if let Some(id) = main.corporation_id {
        corp = tether_esi::names::ticker(
            &ctx.db,
            &ctx.esi,
            id,
            EntityKind::Corporation,
            Priority::Bulk,
        )
        .await?;
    }
    let mut alliance = String::new();
    if let Some(id) = main.alliance_id {
        alliance =
            tether_esi::names::ticker(&ctx.db, &ctx.esi, id, EntityKind::Alliance, Priority::Bulk)
                .await?;
    }
    Ok(Some(nickname::render(
        &template,
        &Parts {
            name: &main.name,
            corp: &corp,
            alliance: &alliance,
        },
    )))
}

/// What roles syncing left the member with, for the nickname step.
struct Synced {
    user: u64,
    nick: Option<String>,
    owner_id: u64,
    /// Roles this job was asked to take back (their mapping is gone) that
    /// Discord refused: nothing else remembers them.
    unremoved: usize,
}

/// Brings one linked member's roles, then nickname, in line. Roles come
/// first and never wait on ESI; the nickname is best-effort.
pub async fn sync_member(
    ctx: &SyncContext,
    account: AccountId,
    removed_role_ids: &[i64],
) -> Result<(), JobError> {
    let Some(config) = store::load(&ctx.db, &ctx.key)
        .await
        .map_err(JobError::retry)?
    else {
        return Ok(());
    };
    let Some(link) = db::link_for(&ctx.db, account)
        .await
        .map_err(JobError::retry)?
    else {
        return Ok(());
    };

    // Under the user's lock, like linking and role removal.
    let mut tx = ctx.db.begin().await.map_err(JobError::retry)?;
    db::lock_user(&mut tx, link.discord_user_id)
        .await
        .map_err(JobError::retry)?;
    let still_linked = db::link_for(&mut *tx, account)
        .await
        .map_err(JobError::retry)?
        .is_some_and(|l| l.discord_user_id == link.discord_user_id);
    if !still_linked {
        // Relinked or unlinked meanwhile; that queued its own work.
        return Ok(());
    }
    let wanted = db::roles_for(&mut *tx, account)
        .await
        .map_err(JobError::retry)?;
    let removed: HashSet<u64> = removed_role_ids
        .iter()
        .filter_map(|r| u64::try_from(*r).ok())
        .collect();
    let managed: HashSet<u64> = db::mapped_role_ids(&mut *tx)
        .await
        .map_err(JobError::retry)?
        .into_iter()
        .filter_map(|r| u64::try_from(r).ok())
        .chain(removed.iter().copied())
        .collect();
    let synced = sync_roles(
        ctx,
        &config,
        account,
        link.discord_user_id,
        &wanted,
        &managed,
        &removed,
    )
    .await;
    tx.commit().await.map_err(JobError::retry)?;
    let Some(synced) = synced? else {
        return Ok(());
    };

    let nick = match nickname_for(ctx, account).await {
        Ok(nick) => nick,
        Err(err) => {
            tracing::warn!(account = account.0, error = %err, "no nickname this time: tickers unavailable");
            None
        }
    };
    // Still linked to the same user? An unlink or relink since the roles
    // step has queued its own work.
    let unchanged = db::link_for(&ctx.db, account)
        .await
        .map_err(JobError::retry)?
        .is_some_and(|l| l.discord_user_id == link.discord_user_id);
    if let Some(nick) = nick
        && unchanged
        && synced.user != synced.owner_id
        && synced.nick.as_deref() != Some(nick.as_str())
    {
        match ctx
            .discord
            .set_nick(&config, synced.user, Some(&nick))
            .await
        {
            Ok(()) => {}
            // Their top role is above the bot's: Discord won't allow it,
            // and retrying won't change that.
            Err(err) if err.code() == Some(codes::MISSING_PERMISSIONS) => {
                tracing::warn!(
                    account = account.0,
                    "the bot may not change this member's nickname"
                );
            }
            Err(err) => return Err(discord_failure(err)),
        }
    }
    if synced.unremoved > 0 {
        return Err(JobError::retry(format!(
            "Discord refused to take {} role(s) whose mapping was removed; move the bot's role above them",
            synced.unremoved
        )));
    }
    Ok(())
}

/// Adds the roles the member is due and takes managed roles they aren't
/// mapped to. A mapped role that `grantable` holds back (now too powerful,
/// or above the bot) is neither given nor taken. `None` if they aren't in
/// the server.
async fn sync_roles(
    ctx: &SyncContext,
    config: &DiscordConfig,
    account: AccountId,
    discord_user_id: i64,
    wanted: &[db::RoleFor],
    managed: &HashSet<u64>,
    removed: &HashSet<u64>,
) -> Result<Option<Synced>, JobError> {
    let user = u64::try_from(discord_user_id).map_err(JobError::permanent)?;
    let check = ctx
        .discord
        .check_cached(config, CHECK_TTL)
        .await
        .map_err(discord_failure)?;
    let desired: HashSet<u64> = grantable(wanted, &check, account).into_iter().collect();
    let mapped_to_them: HashSet<u64> = wanted
        .iter()
        .filter_map(|w| u64::try_from(w.role_id).ok())
        .collect();
    let Some(member) = ctx
        .discord
        .member(config, user)
        .await
        .map_err(discord_failure)?
    else {
        // Not in the server (never joined, or left): nothing to do.
        return Ok(None);
    };
    let assignable = |role: &u64| check.roles.iter().any(|r| r.id == *role && r.assignable);
    let current: HashSet<u64> = member.roles.iter().copied().collect();
    let mut add: Vec<u64> = desired.difference(&current).copied().collect();
    let mut remove: Vec<u64> = current
        .iter()
        .filter(|r| managed.contains(r) && !mapped_to_them.contains(r))
        .copied()
        .collect();
    let (removable, stuck): (Vec<u64>, Vec<u64>) = remove.drain(..).partition(|r| assignable(r));
    for role in &stuck {
        tracing::warn!(
            account = account.0,
            role_id = role,
            "can't take a role the bot may no longer manage"
        );
    }
    let mut remove = removable;
    add.sort_unstable();
    remove.sort_unstable();
    ctx.discord
        .add_roles(config, user, &add)
        .await
        .map_err(discord_failure)?;
    let refused = ctx
        .discord
        .remove_roles(config, user, &remove)
        .await
        .map_err(discord_failure)?;
    let unremoved = stuck
        .iter()
        .chain(&refused)
        .filter(|r| removed.contains(r))
        .count();
    if !add.is_empty() || !remove.is_empty() {
        tracing::info!(
            account = account.0,
            added = add.len(),
            removed = remove.len(),
            "Discord roles synced"
        );
    }
    Ok(Some(Synced {
        user,
        nick: member.nick,
        owner_id: check.owner_id,
        unremoved,
    }))
}

/// Queues a sync for every linked member.
pub async fn sync_all(
    db_pool: &PgPool,
    removed_role_id: Option<i64>,
) -> Result<usize, sqlx::Error> {
    let accounts = db::linked_accounts(db_pool).await?;
    let removed: Vec<i64> = removed_role_id.into_iter().collect();
    for account in &accounts {
        tether_jobs::enqueue(
            db_pool,
            NewJob::new(
                SYNC_MEMBER_JOB,
                json!({ "account_id": account.0, "removed_role_ids": removed }),
            )
            .max_attempts(10),
        )
        .await?;
    }
    Ok(accounts.len())
}

pub fn register_jobs(registry: &mut Registry, ctx: SyncContext) {
    let member_ctx = ctx.clone();
    registry.register(SYNC_MEMBER_JOB, move |job| {
        let ctx = member_ctx.clone();
        async move {
            let payload: SyncMember =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            sync_member(
                &ctx,
                AccountId(payload.account_id),
                &payload.removed_role_ids,
            )
            .await
        }
    });
    registry.register(SYNC_ALL_JOB, move |job| {
        let db = ctx.db.clone();
        async move {
            let payload: SyncAll =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            let queued = sync_all(&db, payload.removed_role_id)
                .await
                .map_err(JobError::retry)?;
            tracing::info!(members = queued, "queued Discord syncs");
            Ok(())
        }
    });
}
