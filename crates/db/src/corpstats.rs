//! Corporation Stats (AA's): which corporations a viewer may see, and each
//! corporation's mains, members and unregistered members, from the member
//! lists Corp Stats sources provide.

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

/// Which corporations a viewer may see, by their permissions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scope {
    /// `compliance.view`: every corporation.
    pub all: bool,
    pub corporation: bool,
    pub alliance: bool,
    pub state: bool,
}

impl Scope {
    pub fn any(&self) -> bool {
        self.all || self.corporation || self.alliance || self.state
    }
}

/// A corporation with a member list the viewer may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corporation {
    pub corporation_id: i64,
    pub name: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub members: i32,
    pub registered: i64,
    pub mains: i64,
}

/// The corporations with member lists the account may see.
pub async fn visible(
    pool: &PgPool,
    account: AccountId,
    scope: Scope,
) -> Result<Vec<Corporation>, sqlx::Error> {
    sqlx::query_as!(
        Corporation,
        r#"
        WITH me AS (
            SELECT c.corporation_id, c.alliance_id, a.state_id
            FROM core.accounts a JOIN core.characters c ON c.id = a.main_character_id
            WHERE a.id = $1
        ),
        -- A corporation's alliance, from any character we know in it.
        lists AS (
            SELECT l.*, (SELECT c.alliance_id FROM core.characters c
                         JOIN core.corp_members cm
                           ON cm.character_id = c.id AND cm.corporation_id = l.corporation_id
                         WHERE c.alliance_id IS NOT NULL
                         ORDER BY c.affiliation_checked_at DESC NULLS LAST
                         LIMIT 1) AS alliance_id
            FROM core.corp_member_lists l
        )
        SELECT l.corporation_id, n.name AS "name?", l.fetched_at, l.members,
               (SELECT count(*) FROM core.corp_members m
                JOIN core.characters c ON c.id = m.character_id
                WHERE m.corporation_id = l.corporation_id) AS "registered!",
               (SELECT count(DISTINCT c.account_id) FROM core.corp_members m
                JOIN core.characters c ON c.id = m.character_id
                WHERE m.corporation_id = l.corporation_id) AS "mains!"
        FROM lists l
        LEFT JOIN core.entity_names n ON n.id = l.corporation_id
        WHERE $2
           OR ($3 AND l.corporation_id = (SELECT corporation_id FROM me))
           OR ($4 AND l.alliance_id IS NOT NULL AND l.alliance_id = (SELECT alliance_id FROM me))
           OR ($5 AND EXISTS (
                SELECT 1 FROM core.state_entities e
                WHERE e.state_id = (SELECT state_id FROM me)
                  AND ((e.entity_kind = 'corporation' AND e.entity_id = l.corporation_id)
                    OR (e.entity_kind = 'alliance' AND e.entity_id = l.alliance_id))))
        ORDER BY n.name NULLS LAST, l.corporation_id
        "#,
        account.0,
        scope.all,
        scope.corporation,
        scope.alliance,
        scope.state,
    )
    .fetch_all(pool)
    .await
}

/// A registered account with characters in the corporation (AA's Mains).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainRow {
    pub main_id: i64,
    pub main_name: String,
    pub main_corporation: Option<String>,
    /// Its characters in this corporation.
    pub characters: Vec<String>,
}

pub async fn mains(pool: &PgPool, corporation: i64) -> Result<Vec<MainRow>, sqlx::Error> {
    sqlx::query_as!(
        MainRow,
        r#"
        SELECT COALESCE(main.id, 0) AS "main_id!", COALESCE(main.name, '(no main)') AS "main_name!",
               cn.name AS "main_corporation?",
               array_agg(c.name ORDER BY c.name) AS "characters!"
        FROM core.corp_members m
        JOIN core.characters c ON c.id = m.character_id
        JOIN core.accounts a ON a.id = c.account_id
        LEFT JOIN core.characters main ON main.id = a.main_character_id
        LEFT JOIN core.entity_names cn ON cn.id = main.corporation_id
        WHERE m.corporation_id = $1
        GROUP BY a.id, main.id, main.name, cn.name
        ORDER BY 2
        "#,
        corporation
    )
    .fetch_all(pool)
    .await
}

/// Every member of the corporation (AA's Members).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub character_id: i64,
    pub name: Option<String>,
    pub registered: bool,
    pub main_name: Option<String>,
}

pub async fn members(pool: &PgPool, corporation: i64) -> Result<Vec<MemberRow>, sqlx::Error> {
    sqlx::query_as!(
        MemberRow,
        r#"
        SELECT m.character_id, COALESCE(c.name, n.name) AS "name?",
               c.id IS NOT NULL AS "registered!", main.name AS "main_name?"
        FROM core.corp_members m
        LEFT JOIN core.characters c ON c.id = m.character_id
        LEFT JOIN core.accounts a ON a.id = c.account_id
        LEFT JOIN core.characters main ON main.id = a.main_character_id
        LEFT JOIN core.entity_names n ON n.id = m.character_id
        WHERE m.corporation_id = $1
        ORDER BY 2 NULLS LAST, 1
        "#,
        corporation
    )
    .fetch_all(pool)
    .await
}

/// A member found by search, in one of `corporations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub corporation_id: i64,
    pub corporation: Option<String>,
    pub character_id: i64,
    pub name: Option<String>,
    pub main_name: Option<String>,
}

/// Members of `corporations` whose name or main's name contains `q`.
pub async fn search(
    pool: &PgPool,
    corporations: &[i64],
    q: &str,
) -> Result<Vec<Found>, sqlx::Error> {
    let pattern = format!(
        "%{}%",
        q.trim()
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    sqlx::query_as!(
        Found,
        r#"
        SELECT m.corporation_id, cn.name AS "corporation?", m.character_id,
               COALESCE(c.name, n.name) AS "name?", main.name AS "main_name?"
        FROM core.corp_members m
        LEFT JOIN core.characters c ON c.id = m.character_id
        LEFT JOIN core.accounts a ON a.id = c.account_id
        LEFT JOIN core.characters main ON main.id = a.main_character_id
        LEFT JOIN core.entity_names n ON n.id = m.character_id
        LEFT JOIN core.entity_names cn ON cn.id = m.corporation_id
        WHERE m.corporation_id = ANY($1)
          AND (lower(COALESCE(c.name, n.name, '')) LIKE $2 OR lower(COALESCE(main.name, '')) LIKE $2)
        ORDER BY 4 NULLS LAST
        LIMIT 200
        "#,
        corporations,
        pattern,
    )
    .fetch_all(pool)
    .await
}

/// Queues a refresh of one corporation (AA's Update Now), unless one is
/// waiting, running, or finished in the last 15 minutes. Atomic under a
/// per-corporation lock.
pub async fn queue_update(
    tx: &mut sqlx::PgConnection,
    corporation: i64,
) -> Result<bool, sqlx::Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext('corpstats.update'), ($1::bigint % 2147483647)::int)",
        corporation
    )
    .execute(&mut *tx)
    .await?;
    let result = sqlx::query!(
        r#"
        INSERT INTO core.jobs (kind, payload, max_attempts)
        SELECT 'compliance.corp_stats', jsonb_build_object('corporation_id', $1::bigint), 3
        WHERE NOT EXISTS (
            SELECT 1 FROM core.jobs WHERE kind = 'compliance.corp_stats'
              AND (payload->>'corporation_id')::bigint IS NOT DISTINCT FROM $1
              AND (state IN ('queued', 'running')
                   OR (finished_at IS NOT NULL AND finished_at > now() - interval '15 minutes'))
        )
        "#,
        corporation
    )
    .execute(&mut *tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Whether the account owns one of the corporation's approved sources (AA:
/// the source's owner may Update Now).
pub async fn owns_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    corporation: i64,
) -> Result<bool, sqlx::Error> {
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "found!" FROM core.corp_sources s
        JOIN core.characters c ON c.id = s.character_id
        WHERE s.corporation_id = $2 AND s.approved_at IS NOT NULL AND c.account_id = $1
        LIMIT 1
        "#,
        account.0,
        corporation
    )
    .fetch_optional(executor)
    .await?;
    Ok(found.is_some())
}
