//! Files apps offer for download (`core.plugin_downloads`): each is built in
//! parts beside the finished one, which it replaces when done.

use sqlx::PgPool;

/// Starts building `name` for `plugin_id` (a build left unfinished is
/// dropped): its title, the permission that may download it, its column
/// count, and the header line as the first part. The build's version, or
/// none when `plugin_id` already has `max_downloads` others.
#[allow(clippy::too_many_arguments)]
pub async fn begin(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    title: &str,
    permission: &str,
    columns: i32,
    header: &str,
    max_downloads: i64,
) -> Result<Option<i32>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // One app's begins one at a time, so the count below holds.
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext('plugin_downloads'), hashtext($1))",
        plugin_id
    )
    .execute(&mut *tx)
    .await?;
    let others = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.plugin_downloads WHERE plugin_id = $1 AND name <> $2"#,
        plugin_id,
        name
    )
    .fetch_one(&mut *tx)
    .await?;
    if others >= max_downloads {
        return Ok(None);
    }
    sqlx::query!(
        "INSERT INTO core.plugin_downloads (plugin_id, name) VALUES ($1, $2) \
         ON CONFLICT (plugin_id, name) DO NOTHING",
        plugin_id,
        name
    )
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query!(
        "SELECT ready_version, building_version FROM core.plugin_downloads \
         WHERE plugin_id = $1 AND name = $2 FOR UPDATE",
        plugin_id,
        name
    )
    .fetch_one(&mut *tx)
    .await?;
    if let Some(old) = row.building_version {
        sqlx::query!(
            "DELETE FROM core.plugin_download_parts \
             WHERE plugin_id = $1 AND name = $2 AND version = $3",
            plugin_id,
            name,
            old
        )
        .execute(&mut *tx)
        .await?;
    }
    let version = row
        .ready_version
        .max(row.building_version)
        .unwrap_or(0)
        .saturating_add(1);
    let bytes = i64::try_from(header.len()).unwrap_or(i64::MAX);
    sqlx::query!(
        "UPDATE core.plugin_downloads SET building_version = $3, building_title = $4, \
         building_permission = $5, building_columns = $6, building_rows = 0, \
         building_bytes = $7 WHERE plugin_id = $1 AND name = $2",
        plugin_id,
        name,
        version,
        title,
        permission,
        columns,
        bytes
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO core.plugin_download_parts (plugin_id, name, version, seq, csv) \
         VALUES ($1, $2, $3, 0, $4)",
        plugin_id,
        name,
        version,
        header
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(version))
}

/// What became of an `append` or `finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// A newer build began.
    Superseded,
    /// No such build under way.
    NotBuilding,
    /// Past the bytes a file may have.
    TooLarge,
    /// Past the parts a build may have.
    TooManyParts,
}

/// The build under way and the name's newest version; `version` is that
/// build's, or an older one when a newer began.
fn stale(ready: Option<i32>, building: Option<i32>, version: i32) -> Outcome {
    if ready.max(building).is_some_and(|newest| newest > version) {
        Outcome::Superseded
    } else {
        Outcome::NotBuilding
    }
}

/// The column count of `version`, if it's the build under way.
pub async fn columns(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    version: i32,
) -> Result<Result<i32, Outcome>, sqlx::Error> {
    let row = sqlx::query!(
        "SELECT ready_version, building_version, building_columns FROM core.plugin_downloads \
         WHERE plugin_id = $1 AND name = $2",
        plugin_id,
        name
    )
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => Err(Outcome::NotBuilding),
        Some(r) if r.building_version == Some(version) => Ok(r.building_columns),
        Some(r) => Err(stale(r.ready_version, r.building_version, version)),
    })
}

/// Adds `csv` (`rows` lines) to the build `version`, if it's still the one
/// under way and stays within `max_bytes` and `max_parts`.
#[allow(clippy::too_many_arguments)]
pub async fn append(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    version: i32,
    csv: &str,
    rows: i64,
    max_bytes: i64,
    max_parts: i32,
) -> Result<Outcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query!(
        "SELECT ready_version, building_version, building_bytes FROM core.plugin_downloads \
         WHERE plugin_id = $1 AND name = $2 FOR UPDATE",
        plugin_id,
        name
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Outcome::NotBuilding);
    };
    if row.building_version != Some(version) {
        return Ok(stale(row.ready_version, row.building_version, version));
    }
    let bytes = i64::try_from(csv.len()).unwrap_or(i64::MAX);
    if row.building_bytes.saturating_add(bytes) > max_bytes {
        return Ok(Outcome::TooLarge);
    }
    let parts = sqlx::query_scalar!(
        r#"SELECT coalesce(max(seq), 0) AS "n!" FROM core.plugin_download_parts
           WHERE plugin_id = $1 AND name = $2 AND version = $3"#,
        plugin_id,
        name,
        version
    )
    .fetch_one(&mut *tx)
    .await?;
    if parts >= max_parts {
        return Ok(Outcome::TooManyParts);
    }
    sqlx::query!(
        "INSERT INTO core.plugin_download_parts (plugin_id, name, version, seq, csv) \
         VALUES ($1, $2, $3, $4, $5)",
        plugin_id,
        name,
        version,
        parts + 1,
        csv
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE core.plugin_downloads SET building_rows = building_rows + $3, \
         building_bytes = building_bytes + $4 WHERE plugin_id = $1 AND name = $2",
        plugin_id,
        name,
        rows,
        bytes
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Outcome::Done)
}

/// Makes the build `version` the finished file, dropping the last.
pub async fn finish(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    version: i32,
) -> Result<Outcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query!(
        "SELECT ready_version, building_version FROM core.plugin_downloads \
         WHERE plugin_id = $1 AND name = $2 FOR UPDATE",
        plugin_id,
        name
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Ok(Outcome::NotBuilding);
    };
    if row.building_version != Some(version) {
        return Ok(stale(row.ready_version, row.building_version, version));
    }
    if let Some(old) = row.ready_version {
        sqlx::query!(
            "DELETE FROM core.plugin_download_parts \
             WHERE plugin_id = $1 AND name = $2 AND version = $3",
            plugin_id,
            name,
            old
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE core.plugin_downloads SET ready_version = building_version, \
         ready_title = building_title, ready_permission = building_permission, \
         ready_rows = building_rows, ready_bytes = building_bytes, built_at = now(), \
         building_version = NULL, building_title = NULL, building_permission = NULL, \
         building_columns = 0, building_rows = 0, building_bytes = 0 \
         WHERE plugin_id = $1 AND name = $2",
        plugin_id,
        name
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Outcome::Done)
}

/// A finished download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub title: String,
    pub permission: String,
    pub version: i32,
    pub rows: i64,
    pub bytes: i64,
    pub built_at: chrono::DateTime<chrono::Utc>,
}

/// `plugin_id`'s finished downloads, by name.
pub async fn files(pool: &PgPool, plugin_id: &str) -> Result<Vec<File>, sqlx::Error> {
    sqlx::query_as!(
        File,
        r#"
        SELECT name, ready_title AS "title!", ready_permission AS "permission!",
               ready_version AS "version!", ready_rows AS rows, ready_bytes AS bytes,
               built_at AS "built_at!"
        FROM core.plugin_downloads
        WHERE plugin_id = $1 AND ready_version IS NOT NULL
        ORDER BY name
        "#,
        plugin_id
    )
    .fetch_all(pool)
    .await
}

/// How many downloads `plugin_id` has, finished or being built.
pub async fn count(pool: &PgPool, plugin_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM core.plugin_downloads WHERE plugin_id = $1"#,
        plugin_id
    )
    .fetch_one(pool)
    .await
}

/// Up to `limit` of a finished file's parts after `after` (its `seq`),
/// in order.
pub async fn parts(
    pool: &PgPool,
    plugin_id: &str,
    name: &str,
    version: i32,
    after: i32,
    limit: i64,
) -> Result<Vec<(i32, String)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT seq, csv FROM core.plugin_download_parts \
         WHERE plugin_id = $1 AND name = $2 AND version = $3 AND seq > $4 \
         ORDER BY seq LIMIT $5",
        plugin_id,
        name,
        version,
        after,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.seq, r.csv)).collect())
}
