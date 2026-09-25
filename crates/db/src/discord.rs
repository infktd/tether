//! Discord links, link attempts and role mappings.
//!
//! Discord ids are unsigned 64-bit snowflakes; they are stored as bigint,
//! which holds every snowflake Discord will issue for decades.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::accounts::AccountId;
use crate::permissions::{Grantee, grantee_from, split};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub account: AccountId,
    pub discord_user_id: i64,
    pub username: String,
    pub linked_at: DateTime<Utc>,
}

pub async fn link_for<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<Link>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT discord_user_id, username, linked_at FROM core.discord_links WHERE account_id = $1",
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| Link {
        account,
        discord_user_id: r.discord_user_id,
        username: r.username,
        linked_at: r.linked_at,
    }))
}

/// The account a Discord user is linked to, if any.
pub async fn account_for_user<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    discord_user_id: i64,
) -> Result<Option<AccountId>, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "SELECT account_id FROM core.discord_links WHERE discord_user_id = $1",
        discord_user_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(id.map(AccountId))
}

/// Links `account` to a Discord user, replacing any other user it had
/// (whose roles the table's trigger queues for removal). Fails with a
/// unique violation if that Discord user is linked to another account.
pub async fn set_link(
    tx: &mut sqlx::PgTransaction<'_>,
    account: AccountId,
    discord_user_id: i64,
    username: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.discord_links (account_id, discord_user_id, username)
        VALUES ($1, $2, $3)
        ON CONFLICT (account_id) DO UPDATE
            SET discord_user_id = EXCLUDED.discord_user_id,
                username = EXCLUDED.username,
                linked_at = now()
        "#,
        account.0,
        discord_user_id,
        username,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Removes the account's link, returning what it was.
pub async fn unlink<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<Link>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        DELETE FROM core.discord_links WHERE account_id = $1
        RETURNING discord_user_id, username, linked_at
        "#,
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| Link {
        account,
        discord_user_id: r.discord_user_id,
        username: r.username,
        linked_at: r.linked_at,
    }))
}

pub async fn insert_attempt<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: &str,
    browser_hash: &[u8],
    account: AccountId,
    ttl: Duration,
) -> Result<(), sqlx::Error> {
    // One pending attempt per account: starting again replaces it.
    sqlx::query!(
        r#"
        WITH gone AS (DELETE FROM core.discord_link_attempts WHERE account_id = $3)
        INSERT INTO core.discord_link_attempts (state, browser_hash, account_id, expires_at)
        VALUES ($1, $2, $3, now() + make_interval(secs => $4))
        "#,
        state,
        browser_hash,
        account.0,
        ttl.as_secs_f64(),
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Consumes an attempt: only once, only from the browser that started it,
/// and only before it expires. Returns the account it links.
pub async fn take_attempt<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: &str,
    browser_hash: &[u8],
) -> Result<Option<AccountId>, sqlx::Error> {
    let id = sqlx::query_scalar!(
        r#"
        DELETE FROM core.discord_link_attempts
        WHERE state = $1 AND browser_hash = $2 AND expires_at > now()
        RETURNING account_id
        "#,
        state,
        browser_hash,
    )
    .fetch_optional(executor)
    .await?;
    Ok(id.map(AccountId))
}

pub async fn prune_attempts(pool: &crate::PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM core.discord_link_attempts WHERE expires_at < now()")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMapping {
    pub id: i64,
    pub role_id: i64,
    pub role_name: String,
    pub grantee: Grantee,
}

pub async fn mappings<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<RoleMapping>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, role_id, role_name, state_id, group_id
        FROM core.discord_role_mappings
        ORDER BY role_name, id
        "#
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(RoleMapping {
                id: r.id,
                role_id: r.role_id,
                role_name: r.role_name,
                grantee: grantee_from(r.state_id, r.group_id)?,
            })
        })
        .collect())
}

/// Returns the mapping id, or `None` if the same mapping already exists.
pub async fn add_mapping<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    role_id: i64,
    role_name: &str,
    grantee: Grantee,
) -> Result<Option<i64>, sqlx::Error> {
    let (state, group) = split(grantee);
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.discord_role_mappings (role_id, role_name, state_id, group_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
        role_id,
        role_name,
        state,
        group,
    )
    .fetch_optional(executor)
    .await
}

pub async fn remove_mapping<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<RoleMapping>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        DELETE FROM core.discord_role_mappings WHERE id = $1
        RETURNING id, role_id, role_name, state_id, group_id
        "#,
        id
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.and_then(|r| {
        Some(RoleMapping {
            id: r.id,
            role_id: r.role_id,
            role_name: r.role_name,
            grantee: grantee_from(r.state_id, r.group_id)?,
        })
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleFor {
    pub role_id: i64,
    /// Every mapping that gives it is one anyone can be in (Guest, or an
    /// Open group).
    pub open_only: bool,
}

/// The roles an account should have: those mapped to its state and to its
/// groups.
pub async fn roles_for<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<RoleFor>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT m.role_id AS "role_id!",
               bool_and(COALESCE(m.state_id = core.guest_state(), false)
                        OR COALESCE(g.join_policy = 'open', false)) AS "open_only!"
        FROM core.discord_role_mappings m
        JOIN core.accounts a ON a.id = $1
        LEFT JOIN core.groups g ON g.id = m.group_id
        WHERE a.active
          AND (m.state_id = a.state_id
               -- Groups count only while the account has a main.
               OR (a.main_character_id IS NOT NULL
                   AND m.group_id IN (SELECT group_id FROM core.group_members WHERE account_id = $1)))
        GROUP BY m.role_id
        ORDER BY 1
        "#,
        account.0,
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RoleFor {
            role_id: r.role_id,
            open_only: r.open_only,
        })
        .collect())
}

/// Serializes linking and role removal for one Discord user, until the
/// transaction ends.
pub async fn lock_user(
    tx: &mut sqlx::PgTransaction<'_>,
    discord_user_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", discord_user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Every role Tether hands out: the ones it may take away.
pub async fn mapped_role_ids<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT role_id AS "role_id!" FROM core.discord_role_mappings ORDER BY 1"#
    )
    .fetch_all(executor)
    .await
}

/// Every account with Discord linked.
pub async fn linked_accounts<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!("SELECT account_id FROM core.discord_links ORDER BY account_id")
        .fetch_all(executor)
        .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

/// Queues a sync for the account if it is linked and none is waiting (the
/// same function the triggers use).
pub async fn queue_sync<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT core.discord_queue_sync($1)", account.0)
        .execute(executor)
        .await?;
    Ok(())
}

/// What the nickname template is filled from: the account's main.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainCharacter {
    pub name: String,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}

pub async fn main_character<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<MainCharacter>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT c.name, c.corporation_id, c.alliance_id
        FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        WHERE a.id = $1
        "#,
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|r| MainCharacter {
        name: r.name,
        corporation_id: r.corporation_id,
        alliance_id: r.alliance_id,
    }))
}
