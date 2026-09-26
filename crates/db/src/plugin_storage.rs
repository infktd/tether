//! Plugin schemas and roles (`core.plugin_storage`), and the migrations
//! applied to them (`core.plugin_migrations`). DDL takes identifiers that
//! can't be bound as parameters, so every name is checked to be made only
//! of `[a-z0-9._-]` and then quoted.

/// A plugin's storage names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub schema_name: String,
    pub role_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0:?} isn't a name Tether would give a plugin's schema or role")]
    BadName(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// `"name"`, for names that need no escaping.
fn ident(name: &str) -> Result<String, StorageError> {
    let fine = (1..=63).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        });
    if fine {
        Ok(format!("\"{name}\""))
    } else {
        Err(StorageError::BadName(name.to_owned()))
    }
}

/// A SCRAM verifier as a SQL string literal: base64, `$`, `:` and digits.
fn verifier_literal(verifier: &str) -> Result<String, StorageError> {
    let fine = verifier.starts_with("SCRAM-SHA-256$")
        && verifier.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'$' | b':' | b'+' | b'/' | b'=' | b'-')
        });
    if fine {
        Ok(format!("'{verifier}'"))
    } else {
        Err(StorageError::BadName("the password verifier".to_owned()))
    }
}

async fn ddl(tx: &mut sqlx::PgConnection, sql: String) -> Result<(), StorageError> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(tx).await?;
    Ok(())
}

/// Creates a plugin's role and schema, in the install's transaction (DDL
/// is transactional in Postgres, so a failed install leaves neither).
/// `settings` are the role's defaults (timeouts, memory); the search path
/// is set to the schema.
pub async fn create(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
    names: &Names,
    verifier: &str,
    connection_limit: u32,
    settings: &[(&str, &str)],
) -> Result<(), StorageError> {
    let role = ident(&names.role_name)?;
    let schema = ident(&names.schema_name)?;
    let password = verifier_literal(verifier)?;
    ddl(
        tx,
        format!(
            "CREATE ROLE {role} LOGIN PASSWORD {password} NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOINHERIT NOREPLICATION NOBYPASSRLS CONNECTION LIMIT {connection_limit}"
        ),
    )
    .await?;
    for (name, value) in settings {
        // Setting names and values are Tether's own constants.
        let fine = name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            && value.bytes().all(|b| b.is_ascii_alphanumeric());
        if !fine {
            return Err(StorageError::BadName((*name).to_owned()));
        }
        ddl(tx, format!("ALTER ROLE {role} SET {name} = '{value}'")).await?;
    }
    ddl(tx, format!("ALTER ROLE {role} SET search_path = {schema}")).await?;
    // Owned by Tether's role: the plugin can use it and create in it, but
    // can't grant anyone else access.
    ddl(tx, format!("CREATE SCHEMA {schema}")).await?;
    ddl(tx, format!("REVOKE ALL ON SCHEMA {schema} FROM PUBLIC")).await?;
    ddl(
        tx,
        format!("GRANT USAGE, CREATE ON SCHEMA {schema} TO {role}"),
    )
    .await?;
    sqlx::query!(
        "INSERT INTO core.plugin_storage (plugin_id, schema_name, role_name) VALUES ($1, $2, $3)",
        plugin_id,
        names.schema_name,
        names.role_name,
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Removes a plugin's schema (with all its data) and role. Its sessions
/// are ended first; call it after closing the plugin's pool.
pub async fn drop(tx: &mut sqlx::PgConnection, names: &Names) -> Result<(), StorageError> {
    let role = ident(&names.role_name)?;
    let schema = ident(&names.schema_name)?;
    sqlx::query_scalar!(
        r#"SELECT count(pg_terminate_backend(pid)) AS "n!" FROM pg_stat_activity WHERE usename = $1"#,
        names.role_name
    )
    .fetch_one(&mut *tx)
    .await?;
    ddl(tx, format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await?;
    // Anything else it owns or was granted in this database (large
    // objects, for one), then the role.
    ddl(tx, format!("DROP OWNED BY {role}")).await?;
    ddl(tx, format!("DROP ROLE IF EXISTS {role}")).await?;
    Ok(())
}

/// Plugins with storage.
pub async fn count<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM core.plugin_storage"#)
        .fetch_one(executor)
        .await
}

pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Option<Names>, sqlx::Error> {
    sqlx::query_as!(
        Names,
        "SELECT schema_name, role_name FROM core.plugin_storage WHERE plugin_id = $1",
        plugin_id
    )
    .fetch_optional(executor)
    .await
}

/// Where a plugin's database password is kept (sealed) in `core.secrets`.
pub fn password_secret(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.db_password")
}

/// Every plugin with storage, by id.
pub async fn plugin_ids<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT plugin_id FROM core.plugin_storage ORDER BY plugin_id")
        .fetch_all(executor)
        .await
}

/// Applied migrations: `(version, sha256)`, in order.
pub async fn applied<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
) -> Result<Vec<(i32, Vec<u8>)>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT version, sha256 FROM core.plugin_migrations WHERE plugin_id = $1 ORDER BY version",
        plugin_id
    )
    .fetch_all(executor)
    .await?;
    Ok(rows.into_iter().map(|r| (r.version, r.sha256)).collect())
}

pub async fn record_migration<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    plugin_id: &str,
    version: i32,
    name: &str,
    sha256: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.plugin_migrations (plugin_id, version, name, sha256)
        VALUES ($1, $2, $3, $4)
        "#,
        plugin_id,
        version,
        name,
        sha256,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Ends a backend: for a plugin migration that ran past its deadline.
pub async fn terminate(pool: &crate::PgPool, pid: i32) -> Result<(), sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT pg_terminate_backend($1) AS "ended!""#, pid)
        .fetch_one(pool)
        .await?;
    Ok(())
}
