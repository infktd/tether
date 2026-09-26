//! Postgres pool, migrations and repositories.

pub mod accounts;
pub mod audit;
pub mod auth;
pub mod autogroups;
pub mod blacklist;
pub mod compliance;
pub mod corpstats;
pub mod discord;
pub mod groups;
pub mod menu;
pub mod notifications;
pub mod permissions;
pub mod permissions_audit;
pub mod personal_tokens;
pub mod ping_options;
pub mod pings;
pub mod plugin_esi;
pub mod plugin_http;
pub mod plugin_jobs;
pub mod plugin_keys;
pub mod plugin_storage;
pub mod plugins;
pub mod secrets;
pub mod settings;
pub mod setup;
pub mod smart_groups;
pub mod states;
pub mod tokens;
pub mod users;

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tether_core::Secret;

pub use sqlx::PgPool;

/// Core migrations, embedded in the binary and applied at startup.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// The server's connections carry this `application_name` (its plugins'
/// carry `tether plugin <id>`), so `tether rollback` can tell it's running.
pub const SERVER_APPLICATION_NAME: &str = "tether";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("could not connect to Postgres after {attempts} attempts")]
    Connect {
        attempts: u32,
        #[source]
        source: sqlx::Error,
    },
    #[error("applying migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    // No source: it could quote the URL, and so the password.
    #[error("DATABASE_URL isn't a valid Postgres URL")]
    BadUrl,
}

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub max_connections: u32,
    /// How long one attempt may take (sqlx retries internally within it).
    pub attempt_timeout: Duration,
    /// Postgres may still be starting when the app boots; keep trying this
    /// many times before giving up.
    pub attempts: u32,
    pub retry_delay: Duration,
    /// Shown in `pg_stat_activity`; see [`SERVER_APPLICATION_NAME`].
    pub application_name: Option<&'static str>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            max_connections: 10,
            attempt_timeout: Duration::from_secs(5),
            attempts: 30,
            retry_delay: Duration::from_secs(2),
            application_name: None,
        }
    }
}

pub async fn connect(url: &Secret<String>, options: &ConnectOptions) -> Result<PgPool, DbError> {
    let attempts = options.attempts.max(1);
    let mut connect_options: PgConnectOptions =
        url.expose().parse().map_err(|_| DbError::BadUrl)?;
    if let Some(name) = options.application_name {
        connect_options = connect_options.application_name(name);
    }
    let mut attempt = 1;
    loop {
        let result = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.attempt_timeout)
            .connect_with(connect_options.clone())
            .await;
        match result {
            Ok(pool) => return Ok(pool),
            Err(source) if attempt >= attempts => {
                return Err(DbError::Connect { attempts, source });
            }
            Err(err) => {
                tracing::warn!(attempt, attempts, error = %err, "Postgres not reachable yet, retrying");
                tokio::time::sleep(options.retry_delay).await;
                attempt += 1;
            }
        }
    }
}

/// Applies pending core migrations. Safe to run from several processes at
/// once: sqlx holds an advisory lock while migrating.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// How far the database is behind a migrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStatus {
    /// Migrations recorded as applied (0 on a fresh database).
    pub applied: usize,
    /// The migrator's migrations not applied yet.
    pub pending: usize,
}

/// Compares `_sqlx_migrations` with `migrator`, without changing anything.
pub async fn migration_status(
    pool: &PgPool,
    migrator: &Migrator,
) -> Result<MigrationStatus, sqlx::Error> {
    let exists = sqlx::query_scalar!(
        r#"SELECT to_regclass('public._sqlx_migrations') IS NOT NULL AS "exists!""#
    )
    .fetch_one(pool)
    .await?;
    let applied: Vec<i64> = if exists {
        sqlx::query_scalar!("SELECT version FROM public._sqlx_migrations WHERE success")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    let pending = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .count();
    Ok(MigrationStatus {
        applied: applied.len(),
        pending,
    })
}

/// Cheapest possible round trip, for readiness checks.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = false)]
    async fn migrations_apply_and_are_idempotent(pool: PgPool) {
        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();

        let has_core: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'core')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(has_core);
    }

    #[sqlx::test(migrations = false)]
    async fn migration_status_counts_applied_and_pending(pool: PgPool) {
        let all = MIGRATOR.iter().count();
        let fresh = migration_status(&pool, &MIGRATOR).await.unwrap();
        assert_eq!(
            fresh,
            MigrationStatus {
                applied: 0,
                pending: all
            }
        );

        let older = Migrator::with_migrations(MIGRATOR.iter().take(3).cloned().collect());
        older.run(&pool).await.unwrap();
        let behind = migration_status(&pool, &MIGRATOR).await.unwrap();
        assert_eq!(
            behind,
            MigrationStatus {
                applied: 3,
                pending: all - 3
            }
        );

        migrate(&pool).await.unwrap();
        let current = migration_status(&pool, &MIGRATOR).await.unwrap();
        assert_eq!(
            current,
            MigrationStatus {
                applied: all,
                pending: 0
            }
        );
    }

    #[sqlx::test(migrations = false)]
    async fn ping_succeeds(pool: PgPool) {
        ping(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn connect_gives_up_after_the_configured_attempts() {
        let url = Secret::new("postgres://nobody:hunter2@127.0.0.1:1/none".to_owned());
        let options = ConnectOptions {
            max_connections: 1,
            attempt_timeout: Duration::from_millis(100),
            attempts: 2,
            retry_delay: Duration::from_millis(10),
            application_name: None,
        };

        let err = connect(&url, &options).await.unwrap_err();

        assert!(matches!(err, DbError::Connect { attempts: 2, .. }));
        // The URL, and so the password, must not leak into the error chain.
        let mut chain = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(s) = source {
            chain.push_str(&s.to_string());
            source = s.source();
        }
        assert!(!chain.contains("hunter2"), "{chain}");
    }
}
