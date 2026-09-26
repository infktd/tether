//! Where installed apps come from (`core.plugins.source`, a GitHub
//! repository) and what the daily update check found. Callers write the
//! audit entries.

use chrono::{DateTime, Utc};

use crate::PgPool;

/// An app that names a repository.
#[derive(Debug, Clone)]
pub struct WithSource {
    pub plugin_id: String,
    pub source: String,
}

/// Every installed app that names a repository.
pub async fn with_source(pool: &PgPool) -> Result<Vec<WithSource>, sqlx::Error> {
    sqlx::query_as!(
        WithSource,
        r#"SELECT id AS plugin_id, source AS "source!" FROM core.plugins WHERE source IS NOT NULL ORDER BY id"#
    )
    .fetch_all(pool)
    .await
}

/// An app's repository and the last check.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub source: Option<String>,
    pub latest_version: Option<String>,
    pub latest_url: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
    pub check_error: Option<String>,
}

pub async fn status(pool: &PgPool, plugin_id: &str) -> Result<Status, sqlx::Error> {
    Ok(sqlx::query_as!(
        Status,
        r#"
        SELECT source, latest_version, latest_url, checked_at, check_error
        FROM core.plugins WHERE id = $1
        "#,
        plugin_id
    )
    .fetch_optional(pool)
    .await?
    .unwrap_or_default())
}

/// Each app's newest version found, for the list.
pub async fn latest(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, latest_version AS "latest_version!" FROM core.plugins
        WHERE source IS NOT NULL AND latest_version IS NOT NULL
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.id, r.latest_version)).collect())
}

/// Records a check of `source`: the newest version found (and its
/// release page), or why there's none. Only while the app still names
/// that repository: one set meanwhile starts afresh.
pub async fn record_check(
    pool: &PgPool,
    plugin_id: &str,
    source: &str,
    latest_version: Option<&str>,
    latest_url: Option<&str>,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE core.plugins SET
            latest_version = COALESCE($2, latest_version),
            latest_url = CASE WHEN $2::text IS NULL THEN latest_url ELSE $3 END,
            checked_at = now(),
            check_error = $4
        WHERE id = $1 AND source = $5
        "#,
        plugin_id,
        latest_version,
        latest_url,
        error,
        source,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Sets (or with `None`, clears) the repository an app's updates come
/// from, forgetting the last check if it changes. True if it changed.
pub async fn set_source<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    source: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE core.plugins SET
            source = $2,
            latest_version = NULL,
            latest_url = NULL,
            checked_at = NULL,
            check_error = NULL
        WHERE id = $1 AND source IS DISTINCT FROM $2
        "#,
        plugin_id,
        source,
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}
