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

/// An installed plugin and its user scopes: what registering a character
/// for it grants.
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

/// The user scopes an installed plugin asks pilots for, as recorded.
pub async fn plugin_user_scopes<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        "SELECT user_scopes FROM core.plugins WHERE id = $1",
        plugin_id
    )
    .fetch_optional(executor)
    .await?
    .unwrap_or_default())
}

/// How many states require the plugin (its registration, with its scopes).
pub async fn states_requiring<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.state_apps WHERE plugin_id = $1"#,
        plugin_id
    )
    .fetch_one(executor)
    .await
}

/// Unregisters every character from the plugin; how many were.
pub async fn clear_app_characters<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "DELETE FROM core.app_characters WHERE plugin_id = $1",
        plugin_id
    )
    .execute(executor)
    .await?
    .rows_affected())
}

/// Records a plugin's user scopes; true if they changed.
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

/// Compliant accounts in `state` with a character whose token lacks any
/// of `scopes` (a revoked one counts with the scopes it carried): those
/// new requirements would flag.
pub async fn accounts_lacking(
    pool: &PgPool,
    state: StateId,
    scopes: &[String],
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!" FROM core.accounts a
        WHERE a.state_id = $1 AND a.compliant
          AND EXISTS (
            SELECT 1 FROM core.characters c
            LEFT JOIN core.character_tokens t ON t.character_id = c.id
            WHERE c.account_id = a.id
              AND (t.character_id IS NULL OR NOT (t.scopes @> $2))
          )
        "#,
        state.0,
        scopes,
    )
    .fetch_one(pool)
    .await
}

/// Compliant accounts in `state` with a character not registered for the
/// app, or whose token lacks one of its `scopes`: those requiring the app
/// would flag.
pub async fn accounts_lacking_app(
    pool: &PgPool,
    state: StateId,
    plugin_id: &str,
    scopes: &[String],
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!" FROM core.accounts a
        WHERE a.state_id = $1 AND a.compliant
          AND EXISTS (
            SELECT 1 FROM core.characters c
            LEFT JOIN core.character_tokens t ON t.character_id = c.id
            WHERE c.account_id = a.id
              AND (t.character_id IS NULL OR NOT (t.scopes @> $3)
                   OR NOT EXISTS (SELECT 1 FROM core.app_characters r
                                  WHERE r.plugin_id = $2 AND r.character_id = c.id))
          )
        "#,
        state.0,
        plugin_id,
        scopes,
    )
    .fetch_one(pool)
    .await
}

// ---- apps a state requires ---------------------------------------------------

/// The apps `state` requires every character to be registered for, with
/// their user scopes.
pub async fn state_apps<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
) -> Result<Vec<PluginScopes>, sqlx::Error> {
    sqlx::query_as!(
        PluginScopes,
        r#"
        SELECT p.id, p.name, p.user_scopes AS scopes
        FROM core.state_apps a JOIN core.plugins p ON p.id = a.plugin_id
        WHERE a.state_id = $1 ORDER BY p.name
        "#,
        state.0
    )
    .fetch_all(executor)
    .await
}

/// Every state's required apps, as (state, app id).
pub async fn all_state_apps(pool: &PgPool) -> Result<Vec<(StateId, String)>, sqlx::Error> {
    let rows = sqlx::query!("SELECT state_id, plugin_id FROM core.state_apps ORDER BY state_id")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (StateId(r.state_id), r.plugin_id))
        .collect())
}

/// False if the state already requires it.
pub async fn add_state_app<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    plugin_id: &str,
    by: Option<AccountId>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.state_apps (state_id, plugin_id, added_by) VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
        state.0,
        plugin_id,
        by.map(|a| a.0),
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Drops every state's requirement of the app (its uninstall), returning
/// the states, for the audit log.
pub async fn remove_app_from_states<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM core.state_apps WHERE plugin_id = $1 RETURNING state_id",
        plugin_id
    )
    .fetch_all(executor)
    .await
}

pub async fn remove_state_app<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    state: StateId,
    plugin_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.state_apps WHERE state_id = $1 AND plugin_id = $2",
        state.0,
        plugin_id
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The account's registrations: (app id, character id).
pub async fn account_app_registrations<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT r.plugin_id, r.character_id FROM core.app_characters r
        JOIN core.characters c ON c.id = r.character_id
        WHERE c.account_id = $1
        "#,
        account.0
    )
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.plugin_id, r.character_id))
        .collect())
}

/// Registering a character for its state (the checklist) registers it for
/// the apps the state requires, whose scopes its token now carries, while
/// the account holds one of the app's permissions (the checklist says so).
/// Returns the apps.
pub async fn register_for_state_apps<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    character_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.app_characters (plugin_id, character_id, registered_by)
        SELECT sa.plugin_id, c.id, $1
        FROM core.characters c
        JOIN core.accounts acc ON acc.id = c.account_id
        JOIN core.state_apps sa ON sa.state_id = acc.state_id
        JOIN core.plugins p ON p.id = sa.plugin_id
        JOIN core.character_tokens t ON t.character_id = c.id
        WHERE c.id = $2 AND c.account_id = $1 AND t.state = 'valid'
          AND cardinality(p.user_scopes) > 0 AND t.scopes @> p.user_scopes
          AND core.holds_app_permission($1, sa.plugin_id)
        FOR SHARE OF c
        ON CONFLICT DO NOTHING
        RETURNING plugin_id
        "#,
        account.0,
        character_id,
    )
    .fetch_all(executor)
    .await
}

// ---- tokens --------------------------------------------------------------

fn token(state: Option<String>, scopes: Option<Vec<String>>) -> Token {
    match state.as_deref() {
        None => Token::None,
        Some("revoked") => Token::Revoked(scopes.unwrap_or_default()),
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
        SELECT c.id, c.name, COALESCE(c.id = a.main_character_id, false) AS "is_main!",
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

/// How many tokens are stored.
pub async fn token_count(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let n = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM core.character_tokens"#)
        .fetch_one(pool)
        .await?;
    Ok(usize::try_from(n).unwrap_or(usize::MAX))
}

/// Characters whose token is revoked (by EVE, or because the character
/// changed EVE account), with the recorded reason.
pub async fn revoked_characters(
    pool: &PgPool,
) -> Result<Vec<(i64, Option<String>, Option<chrono::DateTime<chrono::Utc>>)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT character_id, revoked_reason, revoked_at FROM core.character_tokens WHERE state = 'revoked'"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.character_id, r.revoked_reason, r.revoked_at))
        .collect())
}

/// Valid tokens the ownership check hasn't confirmed for `hours`, oldest
/// first. Every token, scopes or not: each one proves who owns a
/// character.
pub async fn tokens_due(pool: &PgPool, hours: i32, limit: i64) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT character_id FROM core.character_tokens
        WHERE state = 'valid'
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

/// Whether the character is one of the app's characters (N8, F16): it is
/// registered for the app (`core.app_characters`, as aa-memberaudit's own
/// character list), its account holds one of the app's permissions (as
/// Alliance Auth gates apps, whatever the state), and its token is valid
/// and carries every one of `scopes`, the app's user scopes. What an app's
/// user-scope call needs.
pub async fn character_may_serve(
    pool: &PgPool,
    plugin_id: &str,
    character_id: i64,
    scopes: &[String],
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "ok!"
        FROM core.characters c
        JOIN core.character_tokens t ON t.character_id = c.id
        JOIN core.app_characters r ON r.character_id = c.id AND r.plugin_id = $2
        WHERE c.id = $1 AND core.holds_app_permission(c.account_id, $2)
          AND t.state = 'valid' AND t.scopes @> $3
        "#,
        character_id,
        plugin_id,
        scopes,
    )
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// The app's characters: registered for it, on accounts holding one of its
/// permissions, with tokens carrying every one of `scopes` (its user
/// scopes). With `keep_broken` (the bundled Member Audit, as
/// aa-memberaudit keeps a character whose token stopped working and only
/// pauses its updates), also those whose token is revoked, deleted or short
/// of a scope, though not one sold (its owner hash changed).
pub async fn serving_characters(
    pool: &PgPool,
    plugin_id: &str,
    scopes: &[String],
    keep_broken: bool,
) -> Result<Vec<crate::plugin_esi::CharacterRow>, sqlx::Error> {
    sqlx::query_as!(
        crate::plugin_esi::CharacterRow,
        r#"
        SELECT c.id, c.name, c.corporation_id, c.alliance_id
        FROM core.characters c
        JOIN core.app_characters r ON r.character_id = c.id AND r.plugin_id = $1
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        WHERE core.holds_app_permission(c.account_id, $1)
          AND ((t.state = 'valid' AND t.scopes @> $2)
               OR ($3 AND (t.state IS DISTINCT FROM 'revoked'
                           OR t.revoked_reason IS DISTINCT FROM 'owner hash changed')))
        ORDER BY c.name
        "#,
        plugin_id,
        scopes,
        keep_broken,
    )
    .fetch_all(pool)
    .await
}

/// Whether the character is one [`serving_characters`] lists only with
/// `keep_broken`: registered for the app, on an account holding one of its
/// permissions, not sold, and its token not usable for `scopes`.
pub async fn character_kept_broken(
    pool: &PgPool,
    plugin_id: &str,
    character_id: i64,
    scopes: &[String],
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "ok!"
        FROM core.characters c
        JOIN core.app_characters r ON r.character_id = c.id AND r.plugin_id = $2
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        WHERE c.id = $1 AND core.holds_app_permission(c.account_id, $2)
          AND (t.state = 'valid' AND t.scopes @> $3) IS NOT TRUE
          AND (t.state IS DISTINCT FROM 'revoked'
               OR t.revoked_reason IS DISTINCT FROM 'owner hash changed')
        "#,
        character_id,
        plugin_id,
        scopes,
    )
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Whether the account holds one of the app's permissions: whether it
/// may register characters for the app.
pub async fn holds_app_permission<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    plugin_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT core.holds_app_permission($1, $2) AS "holds!""#,
        account.0,
        plugin_id,
    )
    .fetch_one(executor)
    .await
}

/// The account's characters registered for the app.
pub async fn registered_for_app<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    plugin_id: &str,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT r.character_id FROM core.app_characters r
        JOIN core.characters c ON c.id = r.character_id
        WHERE c.account_id = $1 AND r.plugin_id = $2
        "#,
        account.0,
        plugin_id,
    )
    .fetch_all(executor)
    .await
}

/// Registers the account's character for the app; true if it wasn't yet.
/// Only a character on `by`'s own account.
pub async fn register_app_character<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
    by: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        INSERT INTO core.app_characters (plugin_id, character_id, registered_by)
        SELECT $1, c.id, $3 FROM core.characters c WHERE c.id = $2 AND c.account_id = $3
        FOR SHARE
        ON CONFLICT DO NOTHING
        "#,
        plugin_id,
        character_id,
        by.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Unregisters the account's own character from the app; true if it was
/// registered.
pub async fn unregister_app_character<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
    account: AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        DELETE FROM core.app_characters r
        USING core.characters c
        WHERE r.plugin_id = $1 AND r.character_id = $2
          AND c.id = r.character_id AND c.account_id = $3
        "#,
        plugin_id,
        character_id,
        account.0,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Who owns one of [`serving_characters`]: the account's main and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingOwner {
    pub character_id: i64,
    pub main: crate::plugin_esi::CharacterRow,
    pub state: String,
    /// The state's `builtin` (member, blue, guest, blacklist), if any.
    pub builtin: Option<String>,
}

/// The owners of [`serving_characters`] (the same characters, those kept
/// with a broken token included), for the one app that may know them
/// (Member Audit, as aa-memberaudit's scopes go by the owner's main).
/// Accounts without a main are left out.
pub async fn serving_owners(
    pool: &PgPool,
    plugin_id: &str,
) -> Result<Vec<ServingOwner>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.id, m.id AS main_id, m.name AS main_name,
               m.corporation_id AS main_corporation_id, m.alliance_id AS main_alliance_id,
               s.name AS state, s.builtin
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        JOIN core.characters m ON m.id = a.main_character_id
        JOIN core.app_characters r ON r.character_id = c.id AND r.plugin_id = $1
        WHERE core.holds_app_permission(a.id, $1)
          AND (t.state IS DISTINCT FROM 'revoked'
               OR t.revoked_reason IS DISTINCT FROM 'owner hash changed')
        ORDER BY c.id
        "#,
        plugin_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ServingOwner {
            character_id: r.id,
            main: crate::plugin_esi::CharacterRow {
                id: r.main_id,
                name: r.main_name,
                corporation_id: r.main_corporation_id,
                alliance_id: r.main_alliance_id,
            },
            state: r.state,
            builtin: r.builtin,
        })
        .collect())
}

// ---- Corp Stats ----------------------------------------------------------

/// Corporations a state covers: the main of some account in a state other
/// than Guest (or the Blacklist) is in it. Only their member lists are
/// read and kept. Never NPC corporations (ids 1000000 to 1999999): no
/// member reads those, and their rosters aren't anyone's to list.
pub async fn covered_corporations(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT DISTINCT c.corporation_id AS "corporation_id!"
        FROM core.accounts a
        JOIN core.characters c ON c.id = a.main_character_id
        JOIN core.states s ON s.id = a.state_id
        WHERE c.corporation_id IS NOT NULL
          AND c.corporation_id NOT BETWEEN 1000000 AND 1999999
          AND s.builtin IS DISTINCT FROM 'guest' AND s.builtin IS DISTINCT FROM 'blacklist'
        ORDER BY 1
        "#
    )
    .fetch_all(pool)
    .await
}

/// The characters whose tokens may read `corporation_id`'s member list:
/// registered characters in it (a valid token carrying `scope`) on active
/// Member accounts, the one that read it last first, then the tokens
/// confirmed most recently.
pub async fn member_list_readers(
    pool: &PgPool,
    corporation_id: i64,
    scope: &str,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT c.id
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        JOIN core.states s ON s.id = a.state_id
        JOIN core.character_tokens t ON t.character_id = c.id
        LEFT JOIN core.corp_member_lists l ON l.corporation_id = c.corporation_id
        WHERE c.corporation_id = $1 AND a.active AND s.builtin = 'member'
          AND t.state = 'valid' AND $2 = ANY(t.scopes)
        ORDER BY c.id IS NOT DISTINCT FROM l.source_character_id DESC,
                 t.checked_at DESC NULLS LAST, c.id
        "#,
        corporation_id,
        scope,
    )
    .fetch_all(pool)
    .await
}

/// The character's corporation, if a state covers it (as
/// [`covered_corporations`]) and it has no member list yet.
pub async fn corporation_without_list<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT c.corporation_id AS "corporation_id!"
        FROM core.characters c
        WHERE c.id = $1 AND c.corporation_id IS NOT NULL
          AND c.corporation_id NOT BETWEEN 1000000 AND 1999999
          AND NOT EXISTS (SELECT 1 FROM core.corp_member_lists l
                          WHERE l.corporation_id = c.corporation_id)
          AND EXISTS (
            SELECT 1 FROM core.accounts a
            JOIN core.characters m ON m.id = a.main_character_id
            JOIN core.states s ON s.id = a.state_id
            WHERE m.corporation_id = c.corporation_id
              AND s.builtin IS DISTINCT FROM 'guest' AND s.builtin IS DISTINCT FROM 'blacklist'
          )
        "#,
        character_id
    )
    .fetch_optional(executor)
    .await
}

/// Stores a corporation's member list, replacing the last one, and which
/// character's token read it.
pub async fn store_members(
    tx: &mut sqlx::PgConnection,
    corporation_id: i64,
    members: &[i64],
    read_by: i64,
) -> Result<(), sqlx::Error> {
    let count = i32::try_from(members.len()).unwrap_or(i32::MAX);
    sqlx::query!(
        r#"
        INSERT INTO core.corp_member_lists (corporation_id, fetched_at, members, source_character_id)
        VALUES ($1, now(), $2, $3)
        ON CONFLICT (corporation_id) DO UPDATE
        SET fetched_at = now(), members = EXCLUDED.members,
            source_character_id = EXCLUDED.source_character_id
        "#,
        corporation_id,
        count,
        read_by,
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

/// Drops the member lists of corporations no state covers any more
/// (nobody's main in a state other than Guest is in them).
pub async fn prune_member_lists<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        DELETE FROM core.corp_member_lists l
        WHERE NOT EXISTS (
            SELECT 1 FROM core.accounts a
            JOIN core.characters m ON m.id = a.main_character_id
            JOIN core.states st ON st.id = a.state_id
            WHERE m.corporation_id = l.corporation_id AND st.builtin IS DISTINCT FROM 'guest' AND st.builtin IS DISTINCT FROM 'blacklist'
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
          AND s.builtin IS DISTINCT FROM 'guest' AND s.builtin IS DISTINCT FROM 'blacklist'
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

/// A character registered with an app whose token can't be used for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError {
    pub account: AccountId,
    pub character_id: i64,
    pub name: String,
    /// EVE refused its login (not deleted by its pilot, nor short of a
    /// scope): logging in again keeps it on the account.
    pub refused: bool,
}

/// aa-memberaudit's token-error check (`Character.fetch_token`): marks
/// and returns the characters registered with `plugin_id` that have no
/// token it can use (revoked or deleted, or short of one of its scopes)
/// and whose pilot hasn't been told since it last worked. Only pilots
/// who still hold one of the app's permissions; never a sold character
/// (AA's orphans aren't told either: it leaves the account at once).
pub async fn take_token_errors(
    conn: &mut sqlx::PgConnection,
    plugin_id: &str,
) -> Result<Vec<TokenError>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        UPDATE core.app_characters r SET token_error_notified_at = now()
        FROM core.characters c
        JOIN core.plugins p ON p.id = $1
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        WHERE r.plugin_id = $1 AND r.character_id = c.id
          AND r.token_error_notified_at IS NULL
          AND c.account_id IS NOT NULL
          AND core.holds_app_permission(c.account_id, $1)
          AND (t.character_id IS NULL
               OR (t.state = 'revoked' AND t.revoked_reason IS DISTINCT FROM 'owner hash changed')
               OR (t.state = 'valid' AND NOT t.scopes @> p.user_scopes))
        RETURNING c.account_id AS "account_id!", c.id AS "character_id!", c.name AS "name!",
                  COALESCE(t.state = 'revoked' AND t.revoked_reason IS DISTINCT FROM 'deleted',
                           false) AS "refused!"
        "#,
        plugin_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| TokenError {
            account: AccountId(r.account_id),
            character_id: r.character_id,
            name: r.name,
            refused: r.refused,
        })
        .collect())
}

/// Clears the mark of every character registered with `plugin_id` whose
/// token works for it again (aa-memberaudit's
/// `reset_token_error_notified_if_status_ok`): the next breakage tells
/// its pilot again. Returns how many.
pub async fn clear_token_errors(
    conn: &mut sqlx::PgConnection,
    plugin_id: &str,
) -> Result<u64, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE core.app_characters r SET token_error_notified_at = NULL
        FROM core.character_tokens t, core.plugins p
        WHERE r.plugin_id = $1 AND p.id = $1 AND t.character_id = r.character_id
          AND r.token_error_notified_at IS NOT NULL
          AND t.state = 'valid' AND t.scopes @> p.user_scopes
        "#,
        plugin_id,
    )
    .execute(&mut *conn)
    .await?;
    Ok(done.rows_affected())
}
