//! Users (AA's admin site Users): find any account by any of its
//! characters, and see everything about it.

use crate::PgPool;
use crate::accounts::AccountId;

/// How many accounts a search shows at most.
pub const PAGE: i64 = 200;

/// Which accounts a search covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    All,
    Active,
    Inactive,
}

/// An account in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub corporation: Option<String>,
    pub alliance: Option<String>,
    pub state: String,
    pub state_style: String,
    pub owner: bool,
    pub active: bool,
    pub characters: i64,
    /// The character the search found, when it isn't the main.
    pub matched: Option<String>,
}

/// `LIKE` pattern for a substring, with its wildcards escaped.
fn contains(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// Accounts with a character whose name contains `query` (or whose id or
/// account id it is), owner first, then by main name; at most [`PAGE`],
/// with the total.
pub async fn search(
    pool: &PgPool,
    query: &str,
    state: Option<i64>,
    status: Status,
) -> Result<(Vec<Row>, i64), sqlx::Error> {
    let query = query.trim();
    let id: Option<i64> = query.parse().ok();
    let pattern = (!query.is_empty()).then(|| contains(query));
    let active = match status {
        Status::All => None,
        Status::Active => Some(true),
        Status::Inactive => Some(false),
    };
    let rows = sqlx::query!(
        r#"
        WITH found AS (
            SELECT a.id, a.is_owner, a.active, a.state_id, a.main_character_id,
                   (SELECT c.name FROM core.characters c
                    WHERE c.account_id = a.id AND c.id IS DISTINCT FROM a.main_character_id
                      AND ($2::text IS NOT NULL AND c.name ILIKE $2 OR c.id = $3)
                      AND NOT EXISTS (
                          SELECT 1 FROM core.characters m
                          WHERE m.id = a.main_character_id AND (m.name ILIKE $2 OR m.id = $3))
                    ORDER BY c.name LIMIT 1) AS matched
            FROM core.accounts a
            WHERE ($1::bigint IS NULL OR a.state_id = $1)
              AND ($4::boolean IS NULL OR a.active = $4)
              AND ($2::text IS NULL OR a.id = $3 OR EXISTS (
                  SELECT 1 FROM core.characters c
                  WHERE c.account_id = a.id AND (c.name ILIKE $2 OR c.id = $3)))
        )
        SELECT f.id, COALESCE(m.id, 0) AS "main_id!",
               COALESCE(m.name, '(no main)') AS "main_name!",
               corp.name AS "corporation?", ally.name AS "alliance?",
               s.name AS state, COALESCE(s.builtin, 'custom') AS "state_style!",
               f.is_owner, f.active,
               (SELECT count(*) FROM core.characters c WHERE c.account_id = f.id) AS "characters!",
               f.matched,
               count(*) OVER () AS "total!"
        FROM found f
        JOIN core.states s ON s.id = f.state_id
        LEFT JOIN core.characters m ON m.id = f.main_character_id
        LEFT JOIN core.entity_names corp ON corp.id = m.corporation_id
        LEFT JOIN core.entity_names ally ON ally.id = m.alliance_id
        ORDER BY f.is_owner DESC, m.name NULLS LAST, f.id
        LIMIT $5
        "#,
        state,
        pattern,
        id,
        active,
        PAGE,
    )
    .fetch_all(pool)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    Ok((
        rows.into_iter()
            .map(|r| Row {
                account_id: r.id,
                main_id: r.main_id,
                main_name: r.main_name,
                corporation: r.corporation,
                alliance: r.alliance,
                state: r.state,
                state_style: r.state_style,
                owner: r.is_owner,
                active: r.active,
                characters: r.characters,
                matched: r.matched,
            })
            .collect(),
        total,
    ))
}

/// One of an account's characters, for its page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterRow {
    pub id: i64,
    pub name: String,
    pub main: bool,
    pub corporation: Option<String>,
    pub alliance: Option<String>,
    /// `valid` or `revoked`; `None` without a stored token.
    pub token: Option<String>,
    pub added_at: chrono::DateTime<chrono::Utc>,
    pub last_login_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn characters(
    pool: &PgPool,
    account: AccountId,
) -> Result<Vec<CharacterRow>, sqlx::Error> {
    sqlx::query_as!(
        CharacterRow,
        r#"
        SELECT c.id, c.name, (c.id = a.main_character_id) IS TRUE AS "main!",
               corp.name AS "corporation?", ally.name AS "alliance?",
               t.state AS "token?", c.added_at, c.last_login_at
        FROM core.characters c
        JOIN core.accounts a ON a.id = c.account_id
        LEFT JOIN core.character_tokens t ON t.character_id = c.id
        LEFT JOIN core.entity_names corp ON corp.id = c.corporation_id
        LEFT JOIN core.entity_names ally ON ally.id = c.alliance_id
        WHERE c.account_id = $1
        ORDER BY c.id = a.main_character_id DESC NULLS LAST, c.name
        "#,
        account.0
    )
    .fetch_all(pool)
    .await
}

/// The groups an account is in: `(id, name)`.
pub async fn groups(pool: &PgPool, account: AccountId) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name FROM core.group_members m JOIN core.groups g ON g.id = m.group_id
        WHERE m.account_id = $1 ORDER BY g.name
        "#,
        account.0
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.name)).collect())
}

/// When the account was made.
pub async fn created_at(
    pool: &PgPool,
    account: AccountId,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT created_at FROM core.accounts WHERE id = $1",
        account.0
    )
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_are_escaped() {
        assert_eq!(contains("a_b%c\\"), "%a\\_b\\%c\\\\%");
        assert_eq!(contains("Chribba"), "%Chribba%");
    }
}
