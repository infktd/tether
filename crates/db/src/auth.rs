//! Pending SSO logins and browser sessions.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tether_core::Secret;

use crate::PgPool;
use crate::accounts::AccountId;

pub struct NewLoginAttempt<'a> {
    pub state: &'a str,
    pub browser_hash: &'a [u8],
    pub pkce_verifier: &'a Secret<String>,
    pub return_to: &'a str,
    pub ttl: Duration,
}

#[derive(Debug)]
pub struct LoginAttempt {
    pub pkce_verifier: Secret<String>,
    pub return_to: String,
}

pub async fn insert_login_attempt(
    pool: &PgPool,
    attempt: NewLoginAttempt<'_>,
) -> Result<(), sqlx::Error> {
    // Opportunistic cleanup; abandoned attempts are otherwise never deleted.
    sqlx::query!("DELETE FROM core.login_attempts WHERE expires_at < now()")
        .execute(pool)
        .await?;
    sqlx::query!(
        r#"
        INSERT INTO core.login_attempts (state, browser_hash, pkce_verifier, return_to, expires_at)
        VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))
        "#,
        attempt.state,
        attempt.browser_hash,
        attempt.pkce_verifier.expose(),
        attempt.return_to,
        attempt.ttl.as_secs_f64(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Consumes a login attempt: returns it only if the state and browser match
/// and it hasn't expired, deleting it so it can't be replayed.
pub async fn take_login_attempt(
    pool: &PgPool,
    state: &str,
    browser_hash: &[u8],
) -> Result<Option<LoginAttempt>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        DELETE FROM core.login_attempts
        WHERE state = $1 AND browser_hash = $2 AND expires_at > now()
        RETURNING pkce_verifier, return_to
        "#,
        state,
        browser_hash,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| LoginAttempt {
        pkce_verifier: Secret::new(r.pkce_verifier),
        return_to: r.return_to,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub account: AccountId,
    pub expires_at: DateTime<Utc>,
}

pub async fn create_session(
    pool: &PgPool,
    token_hash: &[u8],
    account: AccountId,
    ttl: Duration,
) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM core.sessions WHERE expires_at < now()")
        .execute(pool)
        .await?;
    sqlx::query!(
        r#"
        INSERT INTO core.sessions (token_hash, account_id, expires_at)
        VALUES ($1, $2, now() + make_interval(secs => $3))
        "#,
        token_hash,
        account.0,
        ttl.as_secs_f64(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Looks up a live session. Sessions in use are extended to `ttl` from now,
/// at most once per `touch_every` to avoid a write on every request.
pub async fn find_session(
    pool: &PgPool,
    token_hash: &[u8],
    ttl: Duration,
    touch_every: Duration,
) -> Result<Option<SessionRecord>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT account_id, expires_at,
               last_seen_at < now() - make_interval(secs => $2) AS "stale!"
        FROM core.sessions
        WHERE token_hash = $1 AND expires_at > now()
        "#,
        token_hash,
        touch_every.as_secs_f64(),
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut expires_at = row.expires_at;
    if row.stale {
        expires_at = sqlx::query_scalar!(
            r#"
            UPDATE core.sessions
            SET last_seen_at = now(), expires_at = now() + make_interval(secs => $2)
            WHERE token_hash = $1
            RETURNING expires_at
            "#,
            token_hash,
            ttl.as_secs_f64(),
        )
        .fetch_one(pool)
        .await?;
    }
    Ok(Some(SessionRecord {
        account: AccountId(row.account_id),
        expires_at,
    }))
}

pub async fn delete_session(pool: &PgPool, token_hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM core.sessions WHERE token_hash = $1",
        token_hash
    )
    .execute(pool)
    .await?;
    Ok(())
}
