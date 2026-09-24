//! First-run wizard sessions.

use std::time::Duration;

use crate::PgPool;

/// Starts a setup session, replacing any other.
pub async fn start_session(
    pool: &PgPool,
    token_hash: &[u8],
    ttl: Duration,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!("DELETE FROM core.setup_sessions")
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "INSERT INTO core.setup_sessions (token_hash, expires_at) VALUES ($1, now() + make_interval(secs => $2))",
        token_hash,
        ttl.as_secs_f64(),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

pub async fn session_valid(pool: &PgPool, token_hash: &[u8]) -> Result<bool, sqlx::Error> {
    let valid = sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM core.setup_sessions WHERE token_hash = $1 AND expires_at > now()
        ) AS "valid!""#,
        token_hash,
    )
    .fetch_one(pool)
    .await?;
    Ok(valid)
}

/// Ends setup for good (called once an owner exists).
pub async fn end_sessions<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM core.setup_sessions")
        .execute(executor)
        .await?;
    Ok(())
}
