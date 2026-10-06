//! Plugin ESI access (F16): the characters plugins may use, data-source
//! characters, the access log, and the Discord channels a plugin may post
//! to. Callers audit data-source changes.

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

/// A character as plugins see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterRow {
    pub id: i64,
    pub name: String,
    pub corporation_id: Option<i64>,
    pub alliance_id: Option<i64>,
}

/// An account's characters, main first, with their corporation and
/// alliance.
pub async fn account_characters(
    pool: &PgPool,
    account: AccountId,
) -> Result<Vec<CharacterRow>, sqlx::Error> {
    sqlx::query_as!(
        CharacterRow,
        r#"
        SELECT c.id, c.name, c.corporation_id, c.alliance_id
        FROM core.characters c JOIN core.accounts a ON a.id = c.account_id
        WHERE c.account_id = $1
        ORDER BY c.id = a.main_character_id DESC NULLS LAST, c.name
        "#,
        account.0
    )
    .fetch_all(pool)
    .await
}

/// Every scope the account's characters' tokens hold, so a new grant can
/// ask for them again and not drop any.
pub async fn account_token_scopes(
    pool: &PgPool,
    account: AccountId,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT DISTINCT s AS "s!" FROM core.character_tokens t
        JOIN core.characters c ON c.id = t.character_id, unnest(t.scopes) AS s
        WHERE c.account_id = $1 AND t.state <> 'revoked'
        ORDER BY 1
        "#,
        account.0
    )
    .fetch_all(pool)
    .await
}

// ---- data sources -------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DataSource {
    pub character: CharacterRow,
    /// The main of the account that added it, and that account.
    pub offered_by: Option<String>,
    pub offered_by_account: Option<i64>,
    pub offered_at: DateTime<Utc>,
    /// Added (or, before owners needed no approval, approved).
    pub approved: bool,
    /// The corporation it was added for; a source whose character has
    /// moved since isn't used until it's added again.
    pub approved_corporation: Option<i64>,
    /// The character is still on the account that added it, and that
    /// account is active and not blacklisted.
    pub account_ok: bool,
}

impl DataSource {
    /// Added, still in the corporation it was added for, and on the
    /// account that added it, in good standing.
    pub fn in_use(&self) -> bool {
        self.approved
            && self.account_ok
            && self.approved_corporation.is_some()
            && self.approved_corporation == self.character.corporation_id
    }
}

/// Adds a character as a data source (AA's Add Owner), in use at once for
/// the corporation it's in now; adding it again brings it up to date.
/// Returns that corporation, or `None` if the character isn't known.
pub async fn add_data_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
    added_by: AccountId,
) -> Result<Option<Option<i64>>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.plugin_data_sources
            (plugin_id, character_id, offered_by, approved_by, approved_at, corporation_id)
        SELECT $1, c.id, $3, $3, now(), c.corporation_id FROM core.characters c WHERE c.id = $2
        ON CONFLICT (plugin_id, character_id) DO UPDATE
        SET offered_by = EXCLUDED.offered_by, offered_at = now(),
            approved_by = EXCLUDED.approved_by, approved_at = now(),
            corporation_id = EXCLUDED.corporation_id
        RETURNING corporation_id
        "#,
        plugin_id,
        character_id,
        added_by.0,
    )
    .fetch_optional(executor)
    .await
}

pub async fn remove_data_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        "DELETE FROM core.plugin_data_sources WHERE plugin_id = $1 AND character_id = $2",
        plugin_id,
        character_id
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Offered and approved data sources of a plugin.
pub async fn data_sources(pool: &PgPool, plugin_id: &str) -> Result<Vec<DataSource>, sqlx::Error> {
    Ok(sources_of(pool, Some(plugin_id))
        .await?
        .into_iter()
        .map(|(_, source)| source)
        .collect())
}

/// Every app's data sources, with the app's id: the sidebar's foot and the
/// System page count them.
pub async fn all_data_sources(pool: &PgPool) -> Result<Vec<(String, DataSource)>, sqlx::Error> {
    sources_of(pool, None).await
}

async fn sources_of(
    pool: &PgPool,
    plugin_id: Option<&str>,
) -> Result<Vec<(String, DataSource)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT d.plugin_id, c.id, c.name, c.corporation_id, c.alliance_id, d.offered_at,
               d.approved_at IS NOT NULL AS "approved!", o.name AS "offered_by?",
               d.offered_by AS offered_by_account,
               d.corporation_id AS approved_corporation,
               COALESCE(ca.id = d.offered_by AND ca.active AND NOT core.blacklisted(ca.id), false)
                   AS "account_ok!"
        FROM core.plugin_data_sources d
        JOIN core.characters c ON c.id = d.character_id
        LEFT JOIN core.accounts ca ON ca.id = c.account_id
        LEFT JOIN core.accounts a ON a.id = d.offered_by
        LEFT JOIN core.characters o ON o.id = a.main_character_id
        WHERE $1::text IS NULL OR d.plugin_id = $1 ORDER BY c.name
        "#,
        plugin_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.plugin_id,
                DataSource {
                    character: CharacterRow {
                        id: r.id,
                        name: r.name,
                        corporation_id: r.corporation_id,
                        alliance_id: r.alliance_id,
                    },
                    offered_by: r.offered_by,
                    offered_by_account: r.offered_by_account,
                    offered_at: r.offered_at,
                    approved: r.approved,
                    approved_corporation: r.approved_corporation,
                    account_ok: r.account_ok,
                },
            )
        })
        .collect())
}

/// How a data source's calls to one endpoint went since it was added:
/// when one last went through, and the latest with what came of it.
#[derive(Debug, Clone)]
pub struct SourceReads {
    pub plugin_id: String,
    pub character_id: i64,
    /// The catalogue's name of the endpoint called.
    pub endpoint: String,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_at: DateTime<Utc>,
    /// One of [`SOURCE_OUTCOMES`].
    pub last_outcome: String,
}

/// The access log's outcomes that say how a source is doing: through, or
/// refused for its login or its roles. The rest are the app's or ESI's
/// (a bad request, a 5xx, Tether's own limits).
pub const SOURCE_OUTCOMES: [&str; 4] = ["ok", "ESI 401", "ESI 403", "no usable token"];

/// These apps' data sources' calls since `since` (and since each was last
/// added), by app, character and endpoint, from the access log: the Data
/// sources page, its notice, and the count of those working.
pub async fn source_reads(
    pool: &PgPool,
    plugin_ids: &[String],
    since: DateTime<Utc>,
) -> Result<Vec<SourceReads>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT l.plugin_id, l.character_id AS "character_id!", l.endpoint,
               max(l.at) FILTER (WHERE l.outcome = 'ok') AS last_ok,
               max(l.at) AS "last_at!",
               (array_agg(l.outcome ORDER BY l.id DESC))[1] AS "last_outcome!"
        FROM core.plugin_access_log l
        JOIN core.plugin_data_sources d
          ON d.plugin_id = l.plugin_id AND d.character_id = l.character_id
        WHERE l.plugin_id = ANY($1) AND l.at > $2 AND l.at >= d.offered_at
          AND l.outcome = ANY($3)
        GROUP BY l.plugin_id, l.character_id, l.endpoint
        "#,
        plugin_ids,
        since,
        &SOURCE_OUTCOMES.map(str::to_owned) as &[String],
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SourceReads {
            plugin_id: r.plugin_id,
            character_id: r.character_id,
            endpoint: r.endpoint,
            last_ok: r.last_ok,
            last_at: r.last_at,
            last_outcome: r.last_outcome,
        })
        .collect())
}

/// The corporations of the mains in the Member state (active accounts),
/// with their names where known: what an app's data sources should cover.
pub async fn member_corporations(pool: &PgPool) -> Result<Vec<(i64, Option<String>)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT DISTINCT c.corporation_id AS "corporation_id!", n.name AS "name?"
        FROM core.accounts a
        JOIN core.states s ON s.id = a.state_id
        JOIN core.characters c ON c.id = a.main_character_id
        LEFT JOIN core.entity_names n ON n.id = c.corporation_id
        WHERE s.builtin = 'member' AND a.active AND c.corporation_id IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.corporation_id, r.name))
        .collect())
}

/// A data source's alliance (the character's own), for alliance endpoints;
/// none when it's in none or isn't an approved source.
pub async fn approved_source_alliance<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    let alliance = sqlx::query_scalar!(
        r#"
        SELECT c.alliance_id FROM core.plugin_data_sources d
        JOIN core.characters c ON c.id = d.character_id
        JOIN core.accounts a ON a.id = c.account_id
        WHERE d.plugin_id = $1 AND d.character_id = $2 AND d.approved_at IS NOT NULL
          AND d.corporation_id IS NOT NULL AND c.corporation_id = d.corporation_id
          AND a.id = d.offered_by AND a.active AND NOT core.blacklisted(a.id)
        "#,
        plugin_id,
        character_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(alliance.flatten())
}

/// The corporation a data source reads, if the character is still in the
/// corporation it was added for, on the account that added it.
pub async fn approved_source_corporation<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    character_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT d.corporation_id AS "corporation_id!" FROM core.plugin_data_sources d
        JOIN core.characters c ON c.id = d.character_id
        JOIN core.accounts a ON a.id = c.account_id
        WHERE d.plugin_id = $1 AND d.character_id = $2 AND d.approved_at IS NOT NULL
          AND d.corporation_id IS NOT NULL AND c.corporation_id = d.corporation_id
          AND a.id = d.offered_by AND a.active AND NOT core.blacklisted(a.id)
        "#,
        plugin_id,
        character_id
    )
    .fetch_optional(executor)
    .await
}

/// The account a character belongs to.
pub async fn character_account<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
) -> Result<Option<AccountId>, sqlx::Error> {
    let id = sqlx::query_scalar!(
        "SELECT account_id FROM core.characters WHERE id = $1",
        character_id
    )
    .fetch_optional(executor)
    .await?;
    Ok(id.map(AccountId))
}

/// A data source that went: withdrawn by its owner or removed by an
/// admin, from the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoneSource {
    pub at: DateTime<Utc>,
    /// The audit action (`...withdrawn` or `...removed`).
    pub action: String,
    /// Who did it (their main at the time).
    pub actor_name: Option<String>,
    pub character_id: i64,
    pub character_name: Option<String>,
}

/// A plugin's data sources withdrawn or removed in the last 30 days,
/// newest first (at most 20).
pub async fn gone_data_sources(
    pool: &PgPool,
    plugin_id: &str,
) -> Result<Vec<GoneSource>, sqlx::Error> {
    sqlx::query_as!(
        GoneSource,
        r#"
        WITH gone AS MATERIALIZED (
            SELECT l.id, l.at, l.action, l.actor_name, l.details->>'character_id' AS character
            FROM core.audit_log l
            WHERE l.at > now() - interval '30 days'
              AND l.target = 'plugin:' || $1
              AND l.action IN ('plugin.data_source_withdrawn', 'plugin.data_source_removed')
              AND l.details->>'character_id' ~ '^[0-9]{1,18}$'
            ORDER BY l.at DESC, l.id DESC
            LIMIT 20
        )
        SELECT g.at AS "at!", g.action AS "action!", g.actor_name AS "actor_name?",
               g.character::bigint AS "character_id!",
               c.name AS "character_name?"
        FROM gone g
        LEFT JOIN core.characters c ON c.id = g.character::bigint
        ORDER BY g.at DESC, g.id DESC
        "#,
        plugin_id
    )
    .fetch_all(pool)
    .await
}

// ---- access log ---------------------------------------------------------------

pub async fn log_access(
    pool: &PgPool,
    plugin_id: &str,
    character_id: Option<i64>,
    endpoint: &str,
    outcome: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.plugin_access_log (plugin_id, character_id, endpoint, outcome)
        VALUES ($1, $2, $3, $4)
        "#,
        plugin_id,
        character_id,
        endpoint,
        outcome,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AccessRow {
    pub at: DateTime<Utc>,
    pub character: Option<String>,
    pub endpoint: String,
    pub outcome: String,
}

/// Newest first.
pub async fn access_log(
    pool: &PgPool,
    plugin_id: &str,
    limit: i64,
) -> Result<Vec<AccessRow>, sqlx::Error> {
    sqlx::query_as!(
        AccessRow,
        r#"
        SELECT l.at, c.name AS "character?", l.endpoint, l.outcome
        FROM core.plugin_access_log l LEFT JOIN core.characters c ON c.id = l.character_id
        WHERE l.plugin_id = $1 ORDER BY l.id DESC LIMIT $2
        "#,
        plugin_id,
        limit
    )
    .fetch_all(pool)
    .await
}

/// Drops access-log rows older than `days`, and beyond the newest `keep`
/// of each plugin.
pub async fn prune_access_log(pool: &PgPool, days: i32, keep: i64) -> Result<u64, sqlx::Error> {
    let old = sqlx::query!(
        "DELETE FROM core.plugin_access_log WHERE at < now() - make_interval(days => $1)",
        days
    )
    .execute(pool)
    .await?;
    let over = sqlx::query!(
        r#"
        DELETE FROM core.plugin_access_log l
        USING (
            SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY plugin_id ORDER BY id DESC) AS n
                FROM core.plugin_access_log
            ) ranked WHERE n > $1
        ) extra
        WHERE l.id = extra.id
        "#,
        keep
    )
    .execute(pool)
    .await?;
    Ok(old.rows_affected() + over.rows_affected())
}

// ---- Discord channels ---------------------------------------------------------

/// A plugin's assigned channels that are still ping channels of `guild`:
/// `(channel id, name)`.
pub async fn channels<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    guild_id: i64,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT c.channel_id, d.name FROM core.plugin_channels c
        JOIN core.discord_ping_channels d ON d.channel_id = c.channel_id
        WHERE c.plugin_id = $1 AND d.guild_id = $2 ORDER BY d.name
        "#,
        plugin_id,
        guild_id
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().map(|r| (r.channel_id, r.name)).collect())
}

/// Assigns a ping channel to a plugin; false if it already was.
pub async fn assign_channel<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    channel_id: i64,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        INSERT INTO core.plugin_channels (plugin_id, channel_id) VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        plugin_id,
        channel_id
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn unassign_channel<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    channel_id: i64,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        "DELETE FROM core.plugin_channels WHERE plugin_id = $1 AND channel_id = $2",
        plugin_id,
        channel_id
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}
