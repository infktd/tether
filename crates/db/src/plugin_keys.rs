//! Pinned plugin publisher keys (`core.plugin_keys`). Callers write the
//! audit entry in the same transaction.

/// How a key came to be pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedBy {
    FirstInstall,
    /// The old key signed a statement endorsing the new one.
    Rotation,
    /// An admin replaced it by hand.
    Repin,
}

impl PinnedBy {
    fn as_str(self) -> &'static str {
        match self {
            Self::FirstInstall => "first_install",
            Self::Rotation => "rotation",
            Self::Repin => "repin",
        }
    }
}

/// A pinned key and how it got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub plugin_id: String,
    pub public_key: String,
    pub pinned_by: String,
    pub pinned_at: chrono::DateTime<chrono::Utc>,
}

pub async fn pin<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Option<Pin>, sqlx::Error> {
    sqlx::query_as!(
        Pin,
        "SELECT plugin_id, public_key, pinned_by, pinned_at FROM core.plugin_keys WHERE plugin_id = $1",
        plugin_id
    )
    .fetch_optional(executor)
    .await
}

/// Every pin, by plugin id.
pub async fn list(pool: &crate::PgPool) -> Result<Vec<Pin>, sqlx::Error> {
    sqlx::query_as!(
        Pin,
        "SELECT plugin_id, public_key, pinned_by, pinned_at FROM core.plugin_keys ORDER BY plugin_id"
    )
    .fetch_all(pool)
    .await
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT public_key FROM core.plugin_keys WHERE plugin_id = $1",
        plugin_id
    )
    .fetch_optional(executor)
    .await
}

/// The pinned key, locked until the transaction ends, so it can't change
/// between checking a package against it and installing the package.
pub async fn get_locked(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT public_key FROM core.plugin_keys WHERE plugin_id = $1 FOR UPDATE",
        plugin_id
    )
    .fetch_optional(tx)
    .await
}

/// Pins `key` if the plugin has no key yet. False if it has one.
pub async fn pin_first<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    key: &str,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        INSERT INTO core.plugin_keys (plugin_id, public_key, pinned_by)
        VALUES ($1, $2, $3)
        ON CONFLICT (plugin_id) DO NOTHING
        "#,
        plugin_id,
        key,
        PinnedBy::FirstInstall.as_str(),
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Replaces the pinned key, but only if it is still `old`. False if it
/// isn't (someone else changed it meanwhile).
pub async fn replace<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    old: &str,
    new: &str,
    how: PinnedBy,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query!(
        r#"
        UPDATE core.plugin_keys
        SET public_key = $3, pinned_by = $4, pinned_at = now()
        WHERE plugin_id = $1 AND public_key = $2
        "#,
        plugin_id,
        old,
        new,
        how.as_str(),
    )
    .execute(executor)
    .await?;
    Ok(done.rows_affected() == 1)
}
