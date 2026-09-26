//! Postgres pool, migrations and repositories.

pub mod accounts;
pub mod audit;
pub mod auth;
pub mod autogroups;
pub mod compliance;
pub mod corpstats;
pub mod discord;
pub mod groups;
pub mod notifications;
pub mod permissions;
pub mod permissions_audit;
pub mod pings;
pub mod plugin_esi;
pub mod plugin_jobs;
pub mod plugin_keys;
pub mod plugin_storage;
pub mod plugins;
pub mod secrets;
pub mod settings;
pub mod setup;
pub mod states;
pub mod tokens;

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use tether_core::Secret;

pub use sqlx::PgPool;

/// Core migrations, embedded in the binary and applied at startup.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

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
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            max_connections: 10,
            attempt_timeout: Duration::from_secs(5),
            attempts: 30,
            retry_delay: Duration::from_secs(2),
        }
    }
}

pub async fn connect(url: &Secret<String>, options: &ConnectOptions) -> Result<PgPool, DbError> {
    let attempts = options.attempts.max(1);
    let mut attempt = 1;
    loop {
        let result = PgPoolOptions::new()
            .max_connections(options.max_connections)
            .acquire_timeout(options.attempt_timeout)
            .connect(url.expose())
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
