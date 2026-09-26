//! Plugin HTTP: the hosts and secrets an admin approved for each plugin,
//! and the log of every request. Callers audit approvals; secret values
//! live sealed in `core.secrets` (see [`secret_name`]).

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

/// Where a plugin secret's value is kept (sealed) in `core.secrets`. `:`
/// can't appear in a plugin id or a secret name, so no two plugins' names
/// (nor anything else in `core.secrets`) can collide.
pub fn secret_name(plugin_id: &str, name: &str) -> String {
    format!("plugin-secret:{plugin_id}:{name}")
}

/// Every secret of a plugin in `core.secrets` starts with this.
pub fn secret_prefix(plugin_id: &str) -> String {
    format!("plugin-secret:{plugin_id}:")
}

/// An approved secret: where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSpec {
    pub name: String,
    pub host: String,
    pub header: String,
    pub prefix: Option<String>,
}

/// What an admin approved for a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Approved {
    pub hosts: Vec<String>,
    pub secrets: Vec<SecretSpec>,
}

/// Replaces a plugin's approved hosts and secrets with exactly these, in
/// the install's (or an upgrade's) transaction. A secret that's gone, or
/// now goes to another host, header or prefix, loses its value: the admin
/// enters it again for where it goes now. Returns the names of the
/// secrets whose values were deleted, for the audit log.
pub async fn approve(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
    hosts: &[String],
    secrets: &[SecretSpec],
    by: AccountId,
) -> Result<Vec<String>, sqlx::Error> {
    let before = sqlx::query_as!(
        SecretSpec,
        r#"
        SELECT name, host, header, prefix FROM core.plugin_http_secrets
        WHERE plugin_id = $1
        "#,
        plugin_id
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut deleted = Vec::new();
    for old in &before {
        if !secrets.contains(old)
            && crate::secrets::delete(&mut *tx, &secret_name(plugin_id, &old.name)).await?
        {
            deleted.push(old.name.clone());
        }
    }
    sqlx::query!(
        "DELETE FROM core.plugin_http_secrets WHERE plugin_id = $1",
        plugin_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM core.plugin_http_hosts WHERE plugin_id = $1",
        plugin_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO core.plugin_http_hosts (plugin_id, host, approved_by)
        SELECT $1, h, $3 FROM unnest($2::text[]) AS h
        "#,
        plugin_id,
        hosts,
        by.0,
    )
    .execute(&mut *tx)
    .await?;
    for s in secrets {
        sqlx::query!(
            r#"
            INSERT INTO core.plugin_http_secrets (plugin_id, name, host, header, prefix, approved_by)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            plugin_id,
            s.name,
            s.host,
            s.header,
            s.prefix,
            by.0,
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(deleted)
}

/// What was approved for a plugin.
pub async fn approved(pool: &PgPool, plugin_id: &str) -> Result<Approved, sqlx::Error> {
    let hosts = sqlx::query_scalar!(
        "SELECT host FROM core.plugin_http_hosts WHERE plugin_id = $1 ORDER BY host",
        plugin_id
    )
    .fetch_all(pool)
    .await?;
    let secrets = sqlx::query_as!(
        SecretSpec,
        r#"
        SELECT name, host, header, prefix FROM core.plugin_http_secrets
        WHERE plugin_id = $1 ORDER BY name
        "#,
        plugin_id
    )
    .fetch_all(pool)
    .await?;
    Ok(Approved { hosts, secrets })
}

/// Whether `name` is an approved secret of the plugin, with the plugin's
/// row locked (shared) for the rest of the transaction, so an uninstall or
/// approval can't change that meanwhile.
pub async fn lock_approved_secret(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let locked = sqlx::query_scalar!(
        r#"SELECT true AS "locked!" FROM core.plugins WHERE id = $1 FOR SHARE"#,
        plugin_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if locked.is_none() {
        return Ok(false);
    }
    let found = sqlx::query_scalar!(
        r#"
        SELECT true AS "found!" FROM core.plugin_http_secrets
        WHERE plugin_id = $1 AND name = $2
        "#,
        plugin_id,
        name
    )
    .fetch_optional(&mut *tx)
    .await?;
    Ok(found.is_some())
}

/// Every enabled plugin's approved hosts, `(plugin id, host)`, for
/// `doctor`.
pub async fn enabled_hosts(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT h.plugin_id, h.host FROM core.plugin_http_hosts h
        JOIN core.plugins p ON p.id = h.plugin_id
        WHERE p.enabled ORDER BY h.plugin_id, h.host
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.plugin_id, r.host)).collect())
}

/// When each of a plugin's secrets was last entered, `(name, when)`, for
/// those that have a value.
pub async fn secrets_set(
    pool: &PgPool,
    plugin_id: &str,
) -> Result<Vec<(String, DateTime<Utc>)>, sqlx::Error> {
    let prefix = secret_prefix(plugin_id);
    let rows = sqlx::query!(
        r#"
        SELECT substring(name FROM length($1) + 1) AS "name!", updated_at
        FROM core.secrets WHERE left(name, length($1)) = $1
        "#,
        prefix
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.name, r.updated_at)).collect())
}

/// Deletes every secret value of a plugin (on uninstall).
pub async fn delete_secrets<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<u64, sqlx::Error> {
    let prefix = secret_prefix(plugin_id);
    let done = sqlx::query!(
        "DELETE FROM core.secrets WHERE left(name, length($1)) = $1",
        prefix
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected())
}

// ---- the log ------------------------------------------------------------------

/// One request, as logged. `path` has no query string.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    pub plugin_id: &'a str,
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub status: Option<u16>,
    pub outcome: &'a str,
    pub secret: Option<&'a str>,
    pub bytes: u64,
    pub duration_ms: u64,
}

pub async fn log(pool: &PgPool, entry: &Entry<'_>) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.plugin_http_log
            (plugin_id, method, host, path, status, outcome, secret, bytes, duration_ms)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
        entry.plugin_id,
        entry.method,
        entry.host,
        entry.path,
        entry.status.map(i32::from),
        entry.outcome,
        entry.secret,
        i64::try_from(entry.bytes).unwrap_or(i64::MAX),
        i32::try_from(entry.duration_ms).unwrap_or(i32::MAX),
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LogRow {
    pub at: DateTime<Utc>,
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: Option<i32>,
    pub outcome: String,
    pub secret: Option<String>,
    pub bytes: i64,
    pub duration_ms: i32,
}

/// Newest first.
pub async fn recent(
    pool: &PgPool,
    plugin_id: &str,
    limit: i64,
) -> Result<Vec<LogRow>, sqlx::Error> {
    sqlx::query_as!(
        LogRow,
        r#"
        SELECT at, method, host, path, status, outcome, secret, bytes, duration_ms
        FROM core.plugin_http_log WHERE plugin_id = $1 ORDER BY id DESC LIMIT $2
        "#,
        plugin_id,
        limit
    )
    .fetch_all(pool)
    .await
}

/// Drops log rows older than `days`, and beyond the newest `keep` of each
/// plugin.
pub async fn prune_log(pool: &PgPool, days: i32, keep: i64) -> Result<u64, sqlx::Error> {
    let old = sqlx::query!(
        "DELETE FROM core.plugin_http_log WHERE at < now() - make_interval(days => $1)",
        days
    )
    .execute(pool)
    .await?;
    let over = sqlx::query!(
        r#"
        DELETE FROM core.plugin_http_log l
        USING (
            SELECT id FROM (
                SELECT id, row_number() OVER (PARTITION BY plugin_id ORDER BY id DESC) AS n
                FROM core.plugin_http_log
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
