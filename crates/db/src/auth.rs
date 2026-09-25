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
    /// What it's for, and the scopes it asked SSO for.
    pub purpose: Purpose,
    pub scopes: &'a [String],
    /// The signed-in account that started it (registering, offers).
    pub started_by: Option<AccountId>,
}

/// Why a login was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    Login,
    /// Registering a character with its state's required scopes.
    Register,
    /// Offering a character as a plugin's data source.
    DataSource(String),
    /// Offering a character's corporation member list (Corp Stats).
    CorpSource,
}

impl Purpose {
    fn columns(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Login => ("login", None),
            Self::Register => ("register", None),
            Self::DataSource(plugin) => ("data_source", Some(plugin)),
            Self::CorpSource => ("corp_source", None),
        }
    }

    fn from_columns(purpose: &str, plugin: Option<String>) -> Self {
        match (purpose, plugin) {
            ("register", _) => Self::Register,
            ("data_source", Some(plugin)) => Self::DataSource(plugin),
            ("corp_source", _) => Self::CorpSource,
            _ => Self::Login,
        }
    }
}

#[derive(Debug)]
pub struct LoginAttempt {
    pub pkce_verifier: Secret<String>,
    pub return_to: String,
    pub purpose: Purpose,
    pub scopes: Vec<String>,
    pub started_by: Option<AccountId>,
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
        INSERT INTO core.login_attempts
            (state, browser_hash, pkce_verifier, return_to, expires_at, purpose, plugin_id, scopes,
             started_by)
        VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6, $7, $8, $9)
        "#,
        attempt.state,
        attempt.browser_hash,
        attempt.pkce_verifier.expose(),
        attempt.return_to,
        attempt.ttl.as_secs_f64(),
        attempt.purpose.columns().0,
        attempt.purpose.columns().1,
        attempt.scopes,
        attempt.started_by.map(|a| a.0),
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
        RETURNING pkce_verifier, return_to, purpose, plugin_id, scopes, started_by
        "#,
        state,
        browser_hash,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| LoginAttempt {
        pkce_verifier: Secret::new(r.pkce_verifier),
        return_to: r.return_to,
        purpose: Purpose::from_columns(&r.purpose, r.plugin_id),
        scopes: r.scopes,
        started_by: r.started_by.map(AccountId),
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

/// Looks up a live session of an active account, noting when it was last
/// used (at most once per `touch_every`, to avoid a write per request).
/// Sessions aren't extended: they end `ttl` after sign-in.
pub async fn find_session(
    pool: &PgPool,
    token_hash: &[u8],
    ttl: Duration,
    touch_every: Duration,
) -> Result<Option<SessionRecord>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT s.account_id, s.expires_at,
               s.last_seen_at < now() - make_interval(secs => $2) AS "stale!"
        FROM core.sessions s JOIN core.accounts a ON a.id = s.account_id
        WHERE s.token_hash = $1 AND s.expires_at > now() AND a.active
        "#,
        token_hash,
        touch_every.as_secs_f64(),
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    // A fixed lifetime from sign-in, as Django's sessions (AA): only the
    // last-seen time moves.
    let _ = ttl;
    if row.stale {
        sqlx::query!(
            "UPDATE core.sessions SET last_seen_at = now() WHERE token_hash = $1",
            token_hash,
        )
        .execute(pool)
        .await?;
    }
    let expires_at = row.expires_at;
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

/// Rows removed by [`prune_expired`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pruned {
    pub sessions: u64,
    pub login_attempts: u64,
    pub setup_sessions: u64,
}

/// Deletes expired sessions, login attempts and setup sessions.
pub async fn prune_expired(pool: &PgPool) -> Result<Pruned, sqlx::Error> {
    let sessions = sqlx::query!("DELETE FROM core.sessions WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();
    let login_attempts = sqlx::query!("DELETE FROM core.login_attempts WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();
    let setup_sessions = sqlx::query!("DELETE FROM core.setup_sessions WHERE expires_at < now()")
        .execute(pool)
        .await?
        .rows_affected();
    Ok(Pruned {
        sessions,
        login_attempts,
        setup_sessions,
    })
}
