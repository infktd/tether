//! Postgres pool, migrations and repositories.

pub mod accounts;
pub mod audit;
pub mod auth;
pub mod autogroups;
pub mod blacklist;
pub mod compliance;
pub mod corpstats;
pub mod discord;
pub mod doctrines;
pub mod downloads;
pub mod esi_cache;
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
pub mod plugin_sources;
pub mod plugin_storage;
pub mod plugins;
pub mod secrets;
pub mod settings;
pub mod setup;
pub mod smart_groups;
pub mod states;
pub mod structure_names;
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

    /// Migrates `pool` to just before `version`, with one account (an
    /// instance someone has signed in to).
    async fn running_before(pool: &PgPool, version: i64) {
        Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|m| m.version < version)
                .cloned()
                .collect(),
        )
        .run(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO core.accounts (is_owner) VALUES (false)")
            .execute(pool)
            .await
            .unwrap();
    }

    async fn discord_access(pool: &PgPool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT s.builtin FROM core.permission_grants g JOIN core.states s ON s.id = g.state_id \
             WHERE g.permission = 'discord.access_discord' ORDER BY 1",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Migration 0059: Discord access starts as AA's, granted to nobody, on
    /// a new instance only; one with accounts keeps Member's and Blue's.
    #[sqlx::test(migrations = false)]
    async fn discord_access_is_granted_to_nobody_on_a_new_instance_only(pool: PgPool) {
        running_before(&pool, 59).await;
        migrate(&pool).await.unwrap();
        assert_eq!(discord_access(&pool).await, ["blue", "member"]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_new_instance_starts_with_nobody_on_discord(pool: PgPool) {
        migrate(&pool).await.unwrap();
        assert!(discord_access(&pool).await.is_empty());
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE kind = 'discord.sync_all'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 0);
    }

    /// Migration 0049 (AA's rules for the Blacklist, Secure Groups and
    /// Fleet Pings) carries what was there over: renamed grants and token
    /// scopes, accounts blacklisted through an alt, and grace periods.
    #[sqlx::test(migrations = false)]
    async fn aa_rules_migration_carries_everything_over(pool: PgPool) {
        let before = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|m| m.version < 49)
                .cloned()
                .collect(),
        );
        before.run(&pool).await.unwrap();
        let exec = |sql: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query(sql).execute(&pool).await.unwrap() }
        };
        // A spy whose alt is in a blacklisted corporation, and a clean pilot.
        exec("INSERT INTO core.accounts (id, is_owner) OVERRIDING SYSTEM VALUE VALUES (1, false), (2, false)").await;
        exec(
            "INSERT INTO core.characters (id, account_id, name, corporation_id) VALUES \
             (11, 1, 'Spy', 500), (12, 1, 'Spy Alt', 600), (21, 2, 'Clean', 500)",
        )
        .await;
        exec("UPDATE core.accounts SET main_character_id = id * 10 + 1").await;
        exec(
            "INSERT INTO core.blacklist (entity_id, entity_kind, name, reason, added_by_name) \
             VALUES (600, 'corporation', 'Hostiles', 'Awoxers', 'Admin')",
        )
        .await;
        exec("INSERT INTO core.groups (id, name) OVERRIDING SYSTEM VALUE VALUES (7, 'Officers')")
            .await;
        exec(
            "INSERT INTO core.permission_grants (permission, group_id) VALUES \
             ('fleet.ping', 7), ('blacklist.view_blacklist', 7), ('blacklist.manage_blacklist', 7)",
        )
        .await;
        exec(
            "INSERT INTO core.personal_tokens (account_id, name, token_hash, prefix, scopes, expires_at) \
             VALUES (2, 'bot', '\\x00', 'tp', ARRAY['fleet.ping', 'blacklist.add_notes'], now() + interval '1 day')",
        )
        .await;
        // A smart group with a 3 day grace period, and a member in it.
        exec("INSERT INTO core.smart_groups (group_id, grace_days, notify) VALUES (7, 3, false)")
            .await;
        exec(
            "INSERT INTO core.smart_filters (group_id, kind, config) VALUES (7, 'compliant', '{}')",
        )
        .await;
        exec("INSERT INTO core.smart_grace (group_id, account_id, since) VALUES (7, 2, now() - interval '1 day')").await;

        migrate(&pool).await.unwrap();

        let blacklisted: Vec<bool> =
            sqlx::query_scalar("SELECT core.blacklisted(id) FROM core.accounts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(blacklisted, vec![true, false], "the spy stays blacklisted");
        let carried: String = sqlx::query_scalar(
            "SELECT note FROM core.pilot_notes WHERE entity_id = 11 AND blacklisted",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            carried.contains("Hostiles") && carried.contains("Awoxers"),
            "{carried}"
        );
        let grants: Vec<String> = sqlx::query_scalar(
            "SELECT permission FROM core.permission_grants WHERE group_id = 7 ORDER BY permission",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            grants,
            vec![
                "blacklist.add_new_eve_notes",
                "blacklist.add_to_blacklist",
                "blacklist.view_eve_blacklist",
                "blacklist.view_eve_note_comments",
                "blacklist.view_eve_notes",
                "fleetpings.basic_access",
            ]
        );
        let mut scopes: Vec<String> =
            sqlx::query_scalar("SELECT unnest(scopes) FROM core.personal_tokens")
                .fetch_all(&pool)
                .await
                .unwrap();
        scopes.sort();
        assert_eq!(
            scopes,
            vec![
                "blacklist.add_new_eve_note_comments",
                "blacklist.add_new_eve_notes",
                "fleetpings.basic_access",
            ]
        );
        let (can_grace, notify_on_remove, grace_days): (bool, bool, i32) = sqlx::query_as(
            "SELECT s.can_grace, s.notify_on_remove, f.grace_days FROM core.smart_groups s \
             JOIN core.smart_filters f ON f.group_id = s.group_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(can_grace && !notify_on_remove);
        assert_eq!(grace_days, 3);
        let days_left: f64 = sqlx::query_scalar(
            "SELECT EXTRACT(epoch FROM expires_at - now())::float8 / 86400 FROM core.smart_grace",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!((1.9..2.1).contains(&days_left), "{days_left}");
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
