//! Encrypted SSO refresh tokens. Callers encrypt and decrypt; this module
//! only moves ciphertext.

use std::collections::HashMap;

use crate::PgPool;
use crate::accounts::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenState {
    Valid,
    Revoked,
}

#[derive(Debug, Clone)]
pub struct StoredToken {
    pub sealed: Vec<u8>,
    pub scopes: Vec<String>,
    pub state: TokenState,
}

fn state(value: &str) -> TokenState {
    if value == "revoked" {
        TokenState::Revoked
    } else {
        TokenState::Valid
    }
}

/// Stores a fresh token from a login, reviving a revoked one.
pub async fn upsert<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
    sealed: &[u8],
    scopes: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.character_tokens (character_id, refresh_token, scopes)
        VALUES ($1, $2, $3)
        ON CONFLICT (character_id) DO UPDATE
        SET refresh_token = EXCLUDED.refresh_token, scopes = EXCLUDED.scopes,
            state = 'valid', revoked_at = NULL, revoked_reason = NULL, updated_at = now()
        "#,
        character_id,
        sealed,
        scopes,
    )
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn get(pool: &PgPool, character_id: i64) -> Result<Option<StoredToken>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT refresh_token, scopes, state FROM core.character_tokens WHERE character_id = $1",
        character_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| StoredToken {
        sealed: r.refresh_token,
        scopes: r.scopes,
        state: state(&r.state),
    }))
}

/// The owner hash recorded for a character.
pub async fn owner_hash(pool: &PgPool, character_id: i64) -> Result<Option<String>, sqlx::Error> {
    let hash = sqlx::query_scalar!(
        "SELECT owner_hash FROM core.characters WHERE id = $1",
        character_id
    )
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(hash)
}

/// Saves a refresh token rotated by SSO during a refresh.
pub async fn record_refresh(
    pool: &PgPool,
    character_id: i64,
    sealed: Option<&[u8]>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE core.character_tokens
        SET refresh_token = COALESCE($2, refresh_token), last_refreshed_at = now(), updated_at = now()
        WHERE character_id = $1 AND state = 'valid'
        "#,
        character_id,
        sealed,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns false if it was already revoked. With `sealed`, only that exact
/// refresh token: a refresh that failed after a newer token was stored
/// (a new sign-in, a sale) mustn't revoke the new one.
pub async fn mark_revoked<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    character_id: i64,
    reason: &str,
    sealed: Option<&[u8]>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE core.character_tokens
        SET state = 'revoked', revoked_at = now(), revoked_reason = $2, updated_at = now()
        WHERE character_id = $1 AND state = 'valid'
          AND ($3::bytea IS NULL OR refresh_token = $3)
        "#,
        character_id,
        reason,
        sealed,
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Token state per character on the account (characters without a stored
/// token are absent).
pub async fn states_for_account(
    pool: &PgPool,
    account: AccountId,
) -> Result<HashMap<i64, TokenState>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT t.character_id, t.state
        FROM core.character_tokens t
        JOIN core.characters c ON c.id = t.character_id
        WHERE c.account_id = $1
        "#,
        account.0,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.character_id, state(&r.state)))
        .collect())
}
