//! Cached ESI responses (`core.esi_cache`), behind tether-esi's
//! `PgCache`. Keyed by URL and principal (the bearer token's `sub`, `''`
//! for an unauthenticated request).

use chrono::{DateTime, Utc};

use crate::PgPool;

/// One stored response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub status: i16,
    /// Header names and values, pairwise.
    pub header_names: Vec<String>,
    pub header_values: Vec<Vec<u8>>,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn get(pool: &PgPool, url: &str, principal: &str) -> Result<Option<Entry>, sqlx::Error> {
    sqlx::query_as!(
        Entry,
        r#"
        SELECT status, header_names, header_values, body, etag, expires_at
        FROM core.esi_cache WHERE url = $1 AND principal = $2
        "#,
        url,
        principal
    )
    .fetch_optional(pool)
    .await
}

pub async fn put(
    pool: &PgPool,
    url: &str,
    principal: &str,
    entry: &Entry,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.esi_cache
            (url, principal, status, header_names, header_values, body, etag, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (url, principal) DO UPDATE SET
            status = excluded.status,
            header_names = excluded.header_names,
            header_values = excluded.header_values,
            body = excluded.body,
            etag = excluded.etag,
            expires_at = excluded.expires_at,
            stored_at = now()
        "#,
        url,
        principal,
        entry.status,
        &entry.header_names,
        &entry.header_values,
        &entry.body,
        entry.etag,
        entry.expires_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove(pool: &PgPool, url: &str, principal: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM core.esi_cache WHERE url = $1 AND principal = $2",
        url,
        principal
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Drops entries that expired more than `grace_secs` ago (or, never fresh,
/// weren't stored or revalidated for that long), then all but the `keep`
/// most recently stored.
pub async fn prune(pool: &PgPool, grace_secs: f64, keep: i64) -> Result<u64, sqlx::Error> {
    let expired = sqlx::query!(
        r#"
        DELETE FROM core.esi_cache
        WHERE coalesce(expires_at, stored_at) < now() - make_interval(secs => $1)
        "#,
        grace_secs
    )
    .execute(pool)
    .await?;
    let over = sqlx::query!(
        r#"
        DELETE FROM core.esi_cache c
        USING (
            SELECT url, principal FROM core.esi_cache
            ORDER BY stored_at DESC
            OFFSET $1
        ) extra
        WHERE c.url = extra.url AND c.principal = extra.principal
        "#,
        keep
    )
    .execute(pool)
    .await?;
    Ok(expired.rows_affected() + over.rows_affected())
}
