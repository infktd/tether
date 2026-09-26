//! Installed plugins (`core.plugins`) and uploads waiting for approval
//! (`core.plugin_uploads`). Callers write the audit entries.

use chrono::{DateTime, Utc};

use crate::PgPool;
use crate::accounts::AccountId;

/// An upload's package and signature.
pub struct Upload {
    pub id: i64,
    pub plugin_id: String,
    pub version: String,
    pub package: Vec<u8>,
    pub signature: String,
    pub uploaded_at: DateTime<Utc>,
    /// The GitHub repository it was fetched from, `owner/name`.
    pub source: Option<String>,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("id", &self.id)
            .field("plugin_id", &self.plugin_id)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// An upload, for listing (no package bytes).
#[derive(Debug, Clone)]
pub struct UploadSummary {
    pub id: i64,
    pub plugin_id: String,
    pub version: String,
    pub uploaded_by: Option<String>,
    pub uploaded_at: DateTime<Utc>,
}

pub async fn count_uploads<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM core.plugin_uploads"#)
        .fetch_one(executor)
        .await
}

pub async fn insert_upload<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    version: &str,
    package: &[u8],
    signature: &str,
    uploaded_by: AccountId,
    source: Option<&str>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        INSERT INTO core.plugin_uploads (plugin_id, version, package, signature, uploaded_by, source)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
        plugin_id,
        version,
        package,
        signature,
        uploaded_by.0,
        source,
    )
    .fetch_one(executor)
    .await
}

pub async fn get_upload<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<Upload>, sqlx::Error> {
    sqlx::query_as!(
        Upload,
        r#"
        SELECT id, plugin_id, version, package, signature, uploaded_at, source
        FROM core.plugin_uploads WHERE id = $1
        "#,
        id
    )
    .fetch_optional(executor)
    .await
}

/// Removes an upload and returns it: whoever gets it is the one caller
/// acting on it.
pub async fn take_upload<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: i64,
) -> Result<Option<Upload>, sqlx::Error> {
    sqlx::query_as!(
        Upload,
        r#"
        DELETE FROM core.plugin_uploads WHERE id = $1
        RETURNING id, plugin_id, version, package, signature, uploaded_at, source
        "#,
        id
    )
    .fetch_optional(executor)
    .await
}

/// Newest first.
pub async fn list_uploads(pool: &PgPool) -> Result<Vec<UploadSummary>, sqlx::Error> {
    sqlx::query_as!(
        UploadSummary,
        r#"
        SELECT u.id, u.plugin_id, u.version, c.name AS "uploaded_by?", u.uploaded_at
        FROM core.plugin_uploads u
        LEFT JOIN core.accounts a ON a.id = u.uploaded_by
        LEFT JOIN core.characters c ON c.id = a.main_character_id
        ORDER BY u.id DESC
        "#
    )
    .fetch_all(pool)
    .await
}

/// Drops uploads nobody approved or discarded in `hours`.
pub async fn prune_uploads(pool: &PgPool, hours: i32) -> Result<u64, sqlx::Error> {
    let done = sqlx::query!(
        "DELETE FROM core.plugin_uploads WHERE uploaded_at < now() - make_interval(hours => $1)",
        hours
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// An installed plugin, with its package.
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub package: Vec<u8>,
    pub signature: String,
    /// SHA-256 of the package the admin approved.
    pub package_sha256: Vec<u8>,
    pub enabled: bool,
    pub installed_at: DateTime<Utc>,
    /// The version the last upgrade replaced, while it can be rolled back
    /// to.
    pub previous_version: Option<String>,
    pub upgraded_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for Installed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installed")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// An installed plugin, for listing.
#[derive(Debug, Clone)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub installed_at: DateTime<Utc>,
}

pub struct NewPlugin<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub package: &'a [u8],
    pub signature: &'a str,
    pub package_sha256: &'a [u8],
    pub installed_by: AccountId,
}

/// Installs a plugin, enabled. False if one with that id is installed.
pub async fn install<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin: &NewPlugin<'_>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        INSERT INTO core.plugins
            (id, name, version, package, signature, package_sha256, installed_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (id) DO NOTHING
        "#,
        plugin.id,
        plugin.name,
        plugin.version,
        plugin.package,
        plugin.signature,
        plugin.package_sha256,
        plugin.installed_by.0,
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: &str,
) -> Result<Option<Installed>, sqlx::Error> {
    sqlx::query_as!(
        Installed,
        r#"
        SELECT id, name, version, package, signature, package_sha256, enabled, installed_at,
               previous_version, upgraded_at
        FROM core.plugins WHERE id = $1
        "#,
        id
    )
    .fetch_optional(executor)
    .await
}

/// [`get`], locking the row for the rest of the transaction.
pub async fn get_locked(
    tx: &mut sqlx::PgConnection,
    id: &str,
) -> Result<Option<Installed>, sqlx::Error> {
    sqlx::query_as!(
        Installed,
        r#"
        SELECT id, name, version, package, signature, package_sha256, enabled, installed_at,
               previous_version, upgraded_at
        FROM core.plugins WHERE id = $1 FOR UPDATE
        "#,
        id
    )
    .fetch_optional(tx)
    .await
}

/// The package an upgrade replaced.
pub struct Previous {
    pub version: String,
    pub package: Vec<u8>,
    pub signature: String,
    pub package_sha256: Vec<u8>,
    pub upgraded_at: DateTime<Utc>,
}

impl std::fmt::Debug for Previous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Previous")
            .field("version", &self.version)
            .field("upgraded_at", &self.upgraded_at)
            .finish_non_exhaustive()
    }
}

pub async fn previous<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: &str,
) -> Result<Option<Previous>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT previous_version, previous_package, previous_signature,
               previous_package_sha256, upgraded_at
        FROM core.plugins WHERE id = $1
        "#,
        id
    )
    .fetch_optional(executor)
    .await?;
    // The table's CHECK keeps the five together.
    Ok(row.and_then(|r| {
        Some(Previous {
            version: r.previous_version?,
            package: r.previous_package?,
            signature: r.previous_signature?,
            package_sha256: r.previous_package_sha256?,
            upgraded_at: r.upgraded_at?,
        })
    }))
}

/// Replaces an installed plugin's package with a newer one, keeping the
/// one it replaces as the previous package. Call with the row locked
/// ([`get_locked`]).
pub async fn upgrade(
    tx: &mut sqlx::PgConnection,
    plugin: &NewPlugin<'_>,
) -> Result<bool, sqlx::Error> {
    // Every right-hand side reads the row as it was.
    let done = sqlx::query!(
        r#"
        UPDATE core.plugins SET
            previous_version = version,
            previous_package = package,
            previous_signature = signature,
            previous_package_sha256 = package_sha256,
            upgraded_at = now(),
            upgraded_by = $7,
            name = $2,
            version = $3,
            package = $4,
            signature = $5,
            package_sha256 = $6,
            updated_at = now()
        WHERE id = $1
        "#,
        plugin.id,
        plugin.name,
        plugin.version,
        plugin.package,
        plugin.signature,
        plugin.package_sha256,
        plugin.installed_by.0,
    )
    .execute(tx)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Puts the previous package back and forgets it, if it's still the one
/// with `previous_sha256`. `name` is from its manifest. Call with the row
/// locked ([`get_locked`]).
pub async fn roll_back(
    tx: &mut sqlx::PgConnection,
    id: &str,
    name: &str,
    previous_sha256: &[u8],
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE core.plugins SET
            name = $2,
            version = previous_version,
            package = previous_package,
            signature = previous_signature,
            package_sha256 = previous_package_sha256,
            previous_version = NULL,
            previous_package = NULL,
            previous_signature = NULL,
            previous_package_sha256 = NULL,
            upgraded_at = NULL,
            upgraded_by = NULL,
            updated_at = now()
        WHERE id = $1 AND previous_package_sha256 = $3
        "#,
        id,
        name,
        previous_sha256,
    )
    .execute(tx)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn exists<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM core.plugins WHERE id = $1) AS "exists!""#,
        id
    )
    .fetch_one(executor)
    .await
}

/// By name.
pub async fn list(pool: &PgPool) -> Result<Vec<Summary>, sqlx::Error> {
    sqlx::query_as!(
        Summary,
        "SELECT id, name, version, enabled, installed_at FROM core.plugins ORDER BY name, id"
    )
    .fetch_all(pool)
    .await
}

/// Ids of enabled plugins, for loading at startup.
pub async fn enabled_ids(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT id FROM core.plugins WHERE enabled ORDER BY id")
        .fetch_all(pool)
        .await
}

/// Switches a plugin on or off. False if it isn't installed or already was.
pub async fn set_enabled<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: &str,
    enabled: bool,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE core.plugins SET enabled = $2, updated_at = now()
        WHERE id = $1 AND enabled <> $2
        "#,
        id,
        enabled,
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Removes an installed plugin; returns its version. Its pinned key stays.
pub async fn uninstall<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM core.plugins WHERE id = $1 RETURNING version",
        id
    )
    .fetch_optional(executor)
    .await
}
