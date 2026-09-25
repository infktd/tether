//! Scope compliance (F11): required scopes per state, each account's
//! tokens, who isn't compliant, the daily token check, and Corp Stats.

use chrono::{DateTime, Utc};
use tether_core::scopes::Token;
use tether_core::states::StateId;

use crate::PgPool;
use crate::accounts::AccountId;

// ---- required scopes -----------------------------------------------------

/// The scopes admins added to a state.
pub async fn admin_scopes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT scope FROM core.state_scopes WHERE state_id = $1 ORDER BY scope",
        state.0
    )
    .fetch_all(executor)
    .await
}

/// Every admin-added scope, by state.
pub async fn all_admin_scopes(pool: &PgPool) -> Result<Vec<(StateId, String)>, sqlx::Error> {
    let rows =
        sqlx::query!("SELECT state_id, scope FROM core.state_scopes ORDER BY state_id, scope")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| (StateId(r.state_id), r.scope))
        .collect())
}

/// False if the state already requires it.
pub async fn add_scope<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    scope: &str,
    by: Option<AccountId>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.state_scopes (state_id, scope, added_by) VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
        state.0,
        scope,
        by.map(|a| a.0),
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn remove_scope<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    scope: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.state_scopes WHERE state_id = $1 AND scope = $2",
        state.0,
        scope
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// An installed plugin and the user scopes Member requires for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginScopes {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
}

pub async fn plugin_scopes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<PluginScopes>, sqlx::Error> {
    sqlx::query_as!(
        PluginScopes,
        r#"SELECT id, name, user_scopes AS scopes FROM core.plugins ORDER BY name"#
    )
    .fetch_all(executor)
    .await
}

/// Records a plugin's user scopes; true if they changed (so accounts must
/// be re-evaluated).
pub async fn set_plugin_scopes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    scopes: &[String],
) -> Result<bool, sqlx::Error> {
    let mut sorted = scopes.to_vec();
    sorted.sort();
    sorted.dedup();
    let result = sqlx::query!(
        r#"
        UPDATE core.plugins SET user_scopes = $2
        WHERE id = $1 AND user_scopes IS DISTINCT FROM $2
        "#,
        plugin_id,
        &sorted,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Compliant accounts in `state` with a character whose token lacks
/// `scope`: those a new requirement would flag.
pub async fn accounts_lacking(
    pool: &PgPool,
    state: StateId,
    scope: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!" FROM core.accounts a
        WHERE a.state_id = $1 AND a.compliant
          AND EXISTS (
            SELECT 1 FROM core.characters c
            LEFT JOIN core.character_tokens t ON t.character_id = c.id
            WHERE c.account_id = a.id
              AND (t.character_id IS NULL OR t.state <> 'valid' OR NOT ($2 = ANY(t.scopes)))
          )
        "#,
        state.0,
        scope,
    )
    .fetch_one(pool)
    .await
}

// ---- tokens --------------------------------------------------------------

fn token(state: Option<String>, scopes: Option<Vec<String>>) -> Token {
    match state.as_deref() {
        None => Token::None,
        Some("revoked") => Token::Revoked,
        Some(_) => Token::Valid(scopes.unwrap_or_default()),
    }
}

/// A character on an account, with its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterToken {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    pub token: Token,
}

/// The account's characters (main first) and their tokens.
pub async fn account_tokens<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<CharacterToken>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id, c.name, c.id = a.main_character_id AS "is_main!",
               t.state AS "state?", t.scopes AS "scopes?"
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        WHERE c.account_id = $1
        ORDER BY 3 DESC, c.name
        "#,
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| CharacterToken {
            id: r.id,
            name: r.name,
            is_main: r.is_main,
            token: token(r.state, r.scopes),
        })
        .collect())
}

/// Accounts marked compliant that hold a revoked token (found revoked
/// outside the daily check, e.g. during a plugin call).
pub async fn compliant_with_revoked(pool: &PgPool) -> Result<Vec<AccountId>, sqlx::Error> {
    let ids = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT a.id FROM core.accounts a
        JOIN core.characters c ON c.account_id = a.id
        JOIN core.character_tokens t ON t.character_id = c.id
        WHERE a.compliant AND t.state = 'revoked'
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(ids.into_iter().map(AccountId).collect())
}

/// Valid tokens the daily check hasn't confirmed for `hours`, oldest first.
pub async fn tokens_due(pool: &PgPool, hours: i32, limit: i64) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT character_id FROM core.character_tokens
        WHERE state = 'valid' AND cardinality(scopes) > 0
          AND (checked_at IS NULL OR checked_at < now() - make_interval(hours => $1))
        ORDER BY checked_at NULLS FIRST
        LIMIT $2
        "#,
        hours,
        limit,
    )
    .fetch_all(pool)
    .await
}

pub async fn mark_checked(pool: &PgPool, character_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE core.character_tokens SET checked_at = now() WHERE character_id = $1",
        character_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ---- who isn't compliant -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotCompliant {
    pub account: AccountId,
    pub main_id: i64,
    pub main_name: String,
    pub state: StateId,
    pub since: Option<DateTime<Utc>>,
}

pub async fn not_compliant(pool: &PgPool) -> Result<Vec<NotCompliant>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT a.id, m.id AS main_id, m.name AS main_name,
               a.state_id, a.state_evaluated_at
        FROM core.accounts a
        JOIN core.characters m ON m.id = a.main_character_id
        WHERE NOT a.compliant
        ORDER BY m.name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| NotCompliant {
            account: AccountId(r.id),
            main_id: r.main_id,
            main_name: r.main_name,
            state: StateId(r.state_id),
            since: r.state_evaluated_at,
        })
        .collect())
}

/// The account's state, if it isn't compliant with it.
pub async fn not_compliant_state<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Option<StateId>, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "SELECT state_id FROM core.accounts WHERE id = $1 AND NOT compliant",
        account.0
    )
    .fetch_optional(executor)
    .await?;
    Ok(id.map(StateId))
}

/// A group Tether manages (`compliant`).
pub async fn managed_group<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    which: &str,
) -> Result<Option<crate::groups::GroupId>, sqlx::Error> {
    let id = sqlx::query_scalar!("SELECT id FROM core.groups WHERE managed = $1", which)
        .fetch_optional(executor)
        .await?;
    Ok(id.map(crate::groups::GroupId))
}

/// Puts the account in the group or takes it out; true if that changed.
pub async fn set_group_member(
    tx: &mut sqlx::PgConnection,
    group: crate::groups::GroupId,
    account: AccountId,
    member: bool,
) -> Result<bool, sqlx::Error> {
    let changed = if member {
        sqlx::query!(
            "INSERT INTO core.group_members (group_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            group.0,
            account.0
        )
        .execute(&mut *tx)
        .await?
    } else {
        sqlx::query!(
            "DELETE FROM core.group_members WHERE group_id = $1 AND account_id = $2",
            group.0,
            account.0
        )
        .execute(&mut *tx)
        .await?
    };
    Ok(changed.rows_affected() == 1)
}

/// Whether the character is on a Member account and registered with
/// `scope` (a valid token carrying it): what a plugin's user-scope call
/// needs (N8). Only Member requires plugin scopes, so only Members'
/// characters are read.
pub async fn character_may_serve(
    pool: &PgPool,
    character_id: i64,
    scope: &str,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "ok!"
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        JOIN core.character_tokens t ON t.character_id = c.id
        WHERE c.id = $1 AND s.builtin = 'member'
          AND t.state = 'valid' AND $2 = ANY(t.scopes)
        "#,
        character_id,
        scope,
    )
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Characters on Member accounts whose tokens carry every one of `scopes`:
/// the characters a plugin with these user scopes may use.
pub async fn serving_characters(
    pool: &PgPool,
    scopes: &[String],
) -> Result<Vec<crate::plugin_esi::CharacterRow>, sqlx::Error> {
    sqlx::query_as!(
        crate::plugin_esi::CharacterRow,
        r#"
        SELECT c.id, c.name, c.corporation_id, c.alliance_id
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        JOIN core.character_tokens t ON t.character_id = c.id
        WHERE s.builtin = 'member'
          AND t.state = 'valid' AND t.scopes @> $1
        ORDER BY c.name
        "#,
        scopes,
    )
    .fetch_all(pool)
    .await
}

// ---- Corp Stats ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpSource {
    pub character_id: i64,
    pub character_name: String,
    /// The character's current corporation.
    pub corporation_id: Option<i64>,
    pub offered_by: Option<String>,
    pub offered_at: DateTime<Utc>,
    pub approved: bool,
    /// The corporation it was approved for.
    pub approved_corporation: Option<i64>,
}

impl CorpSource {
    /// Approved, and still in the corporation it was approved for.
    pub fn in_use(&self) -> bool {
        self.approved
            && self.approved_corporation.is_some()
            && self.approved_corporation == self.corporation_id
    }
}

/// Returns false if it was already offered (an approval stays).
pub async fn offer_corp_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
    by: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.corp_sources (character_id, offered_by) VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        character_id,
        by.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Approves an offer for the character's current corporation.
pub async fn approve_corp_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
    by: AccountId,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        UPDATE core.corp_sources s
        SET approved_by = $2, approved_at = now(), corporation_id = c.corporation_id
        FROM core.characters c
        WHERE s.character_id = $1 AND c.id = s.character_id AND c.corporation_id IS NOT NULL
        RETURNING s.corporation_id AS "corporation_id!"
        "#,
        character_id,
        by.0,
    )
    .fetch_optional(executor)
    .await
}

pub async fn remove_corp_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.corp_sources WHERE character_id = $1",
        character_id
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn corp_sources(pool: &PgPool) -> Result<Vec<CorpSource>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT s.character_id, c.name AS character_name, c.corporation_id AS current,
               m.name AS "offered_by?", s.offered_at, s.approved_at, s.corporation_id
        FROM core.corp_sources s
        JOIN core.characters c ON c.id = s.character_id
        LEFT JOIN core.accounts a ON a.id = s.offered_by
        LEFT JOIN core.characters m ON m.id = a.main_character_id
        ORDER BY s.approved_at IS NOT NULL, c.name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| CorpSource {
            character_id: r.character_id,
            character_name: r.character_name,
            corporation_id: r.current,
            offered_by: r.offered_by,
            offered_at: r.offered_at,
            approved: r.approved_at.is_some(),
            approved_corporation: r.corporation_id,
        })
        .collect())
}

/// Stores a corporation's member list, replacing the last one.
pub async fn store_members(
    tx: &mut sqlx::PgConnection,
    corporation_id: i64,
    members: &[i64],
) -> Result<(), sqlx::Error> {
    let count = i32::try_from(members.len()).unwrap_or(i32::MAX);
    sqlx::query!(
        r#"
        INSERT INTO core.corp_member_lists (corporation_id, fetched_at, members)
        VALUES ($1, now(), $2)
        ON CONFLICT (corporation_id) DO UPDATE SET fetched_at = now(), members = EXCLUDED.members
        "#,
        corporation_id,
        count,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM core.corp_members WHERE corporation_id = $1",
        corporation_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO core.corp_members (corporation_id, character_id)
        SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING
        "#,
        corporation_id,
        members,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Drops member lists no approved source, still in that corporation,
/// keeps current any more.
pub async fn prune_member_lists<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        DELETE FROM core.corp_member_lists l
        WHERE NOT EXISTS (
            SELECT 1 FROM core.corp_sources s
            JOIN core.characters c ON c.id = s.character_id
            WHERE s.corporation_id = l.corporation_id AND s.approved_at IS NOT NULL
              AND c.corporation_id = s.corporation_id
        )
        -- Or nobody's main is in it any more.
        OR NOT EXISTS (
            SELECT 1 FROM core.accounts a
            JOIN core.characters m ON m.id = a.main_character_id
            JOIN core.states st ON st.id = a.state_id
            WHERE m.corporation_id = l.corporation_id AND st.builtin IS DISTINCT FROM 'guest'
        )
        "#
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberList {
    pub corporation_id: i64,
    pub corporation_name: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub members: i32,
    pub registered: i64,
}

pub async fn member_lists(pool: &PgPool) -> Result<Vec<MemberList>, sqlx::Error> {
    sqlx::query_as!(
        MemberList,
        r#"
        SELECT l.corporation_id, n.name AS "corporation_name?", l.fetched_at, l.members,
               (SELECT count(*) FROM core.corp_members m
                JOIN core.characters c ON c.id = m.character_id
                WHERE m.corporation_id = l.corporation_id) AS "registered!"
        FROM core.corp_member_lists l
        LEFT JOIN core.entity_names n ON n.id = l.corporation_id
        ORDER BY n.name NULLS LAST, l.corporation_id
        "#
    )
    .fetch_all(pool)
    .await
}

/// Members of `corporation_id` with no character in Tether, and their
/// names where known.
pub async fn unregistered(
    pool: &PgPool,
    corporation_id: i64,
) -> Result<Vec<(i64, Option<String>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT m.character_id, n.name AS "name?"
        FROM core.corp_members m
        LEFT JOIN core.characters c ON c.id = m.character_id
        LEFT JOIN core.entity_names n ON n.id = m.character_id
        WHERE m.corporation_id = $1 AND c.id IS NULL
        ORDER BY n.name NULLS LAST, m.character_id
        "#,
        corporation_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.character_id, r.name)).collect())
}

/// Whether the corporation is covered: the main of some account in a state
/// other than Guest is in it.
pub async fn corporation_covered<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    corporation_id: i64,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "found!"
        FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        JOIN core.states s ON s.id = a.state_id
        WHERE c.corporation_id = $1 AND s.builtin IS DISTINCT FROM 'guest'
        LIMIT 1
        "#,
        corporation_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}

/// Names from the names cache, for display.
pub async fn cached_names(
    pool: &PgPool,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, String>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, name FROM core.entity_names WHERE id = ANY($1)",
        ids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.name)).collect())
}

/// Corporations with members in a state other than Guest but no member
/// list yet, and their names where known.
pub async fn corporations_without_lists(
    pool: &PgPool,
) -> Result<Vec<(i64, Option<String>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT DISTINCT c.corporation_id AS "corporation_id!", n.name AS "name?"
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        LEFT JOIN core.entity_names n ON n.id = c.corporation_id
        WHERE c.corporation_id IS NOT NULL
          AND s.builtin IS DISTINCT FROM 'guest'
          AND c.corporation_id NOT IN (SELECT corporation_id FROM core.corp_member_lists)
          AND c.corporation_id NOT BETWEEN 1000000 AND 1999999
        ORDER BY 2 NULLS LAST, 1
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.corporation_id, r.name))
        .collect())
}
