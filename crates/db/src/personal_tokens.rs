//! Personal access tokens (F19): an account's tokens for the JSON API,
//! stored hashed.

use chrono::{DateTime, Utc};

use crate::accounts::AccountId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalToken {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub async fn for_account<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<Vec<PersonalToken>, sqlx::Error> {
    sqlx::query_as!(
        PersonalToken,
        r#"
        SELECT id, name, prefix, scopes, created_at, expires_at, last_used_at
        FROM core.personal_tokens WHERE account_id = $1 ORDER BY created_at DESC
        "#,
        account.0
    )
    .fetch_all(executor)
    .await
}

pub async fn count<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.personal_tokens WHERE account_id = $1"#,
        account.0
    )
    .fetch_one(executor)
    .await
}

pub struct NewToken<'a> {
    pub account: AccountId,
    pub name: &'a str,
    pub token_hash: &'a [u8],
    pub prefix: &'a str,
    pub scopes: &'a [String],
    pub expires_at: DateTime<Utc>,
}

pub async fn insert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    token: NewToken<'_>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.personal_tokens (account_id, name, token_hash, prefix, scopes, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6) RETURNING id
        "#,
        token.account.0,
        token.name,
        token.token_hash,
        token.prefix,
        token.scopes,
        token.expires_at,
    )
    .fetch_one(executor)
    .await
}

/// Deletes one of the account's tokens; returns its name.
pub async fn revoke<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account: AccountId,
    id: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM core.personal_tokens WHERE id = $1 AND account_id = $2 RETURNING name",
        id,
        account.0
    )
    .fetch_optional(executor)
    .await
}

/// A live token: its account (active only) and scopes. Marks it used, at
/// most once a minute.
pub struct Found {
    pub id: i64,
    pub account: AccountId,
    pub scopes: Vec<String>,
}

pub async fn find(pool: &crate::PgPool, token_hash: &[u8]) -> Result<Option<Found>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT t.id, t.account_id, t.scopes,
               (t.last_used_at IS NULL OR t.last_used_at < now() - interval '1 minute') AS "stale!"
        FROM core.personal_tokens t JOIN core.accounts a ON a.id = t.account_id
        WHERE t.token_hash = $1 AND t.expires_at > now() AND a.active
        "#,
        token_hash
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    // Written at most once a minute, not on every request.
    if row.stale {
        sqlx::query!(
            "UPDATE core.personal_tokens SET last_used_at = now() WHERE id = $1",
            row.id
        )
        .execute(pool)
        .await?;
    }
    Ok(Some(Found {
        id: row.id,
        account: AccountId(row.account_id),
        scopes: row.scopes,
    }))
}

/// Drops tokens that expired a while ago (maintenance).
pub async fn prune<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM core.personal_tokens WHERE expires_at < now() - interval '30 days'"
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}
