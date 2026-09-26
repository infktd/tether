#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)] // test code

//! Snapshots and `tether rollback` against a real database (N14, N7): a
//! snapshot before migrations, the migration, the rollback, and the data
//! checked afterwards.
//!
//! These need the Postgres 16 client tools (the app image's
//! postgresql-client-16): `TETHER_TEST_PG_BIN`, PATH, the usual install
//! directories, or failing those, the ones inside the dev database
//! container (`deploy/docker-compose.dev.yml`), through small wrapper
//! scripts. CI installs them; there a missing tool fails the test rather
//! than skipping it.

use std::path::{Path, PathBuf};

use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool, SqlSafeStr};
use tether_cli::rollback::{self, Args};
use tether_core::Secret;
use tether_core::crypto::EncryptionKey;
use tether_db::accounts::{self, AccountId};
use tether_db::plugin_storage::{self, Names};
use tether_db::secrets;
use tether_snapshots::{Config, Kind, Reason, Snapshots, Tools};

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn random() -> String {
    tether_core::new_token().unwrap().expose()[..8].to_owned()
}

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        let env = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"))
            .expect("DATABASE_URL or a .env file");
        env.lines()
            .find_map(|l| l.strip_prefix("DATABASE_URL="))
            .expect("DATABASE_URL in .env")
            .trim()
            .to_owned()
    })
}

/// The URL of this test's own database.
async fn test_url(db: &PgPool) -> String {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(db)
        .await
        .unwrap();
    let base = database_url();
    let base = base.split('?').next().unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    format!("{server}/{name}")
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tether-rollback-{}", random()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Where Postgres 16's client tools are (`Some(None)` for PATH), or `None`
/// with a message when this machine has none (never in CI).
async fn pg_bin(db: &PgPool) -> Option<Option<PathBuf>> {
    let major: i32 =
        sqlx::query_scalar("SELECT current_setting('server_version_num')::int / 10000")
            .fetch_one(db)
            .await
            .unwrap();
    let major = u32::try_from(major).unwrap();
    let mut candidates: Vec<Option<PathBuf>> = Vec::new();
    if let Ok(dir) = std::env::var("TETHER_TEST_PG_BIN") {
        candidates.push(Some(dir.into()));
    }
    candidates.push(None);
    for dir in [
        format!("/usr/lib/postgresql/{major}/bin"),
        format!("/opt/homebrew/opt/postgresql@{major}/bin"),
        format!("/usr/local/opt/postgresql@{major}/bin"),
    ] {
        candidates.push(Some(dir.into()));
    }
    for candidate in candidates {
        if Tools::new(candidate.clone()).check(major).await.is_ok() {
            return Some(candidate);
        }
    }
    if let Some(dir) = container_tools().await
        && Tools::new(Some(dir.clone())).check(major).await.is_ok()
    {
        return Some(Some(dir));
    }
    let why = format!(
        "the Postgres {major} client tools (pg_dump, pg_restore, psql) aren't installed; set \
         TETHER_TEST_PG_BIN or start the dev database container"
    );
    if std::env::var_os("CI").is_some() {
        panic!("{why}");
    }
    eprintln!("SKIPPED: {why}");
    None
}

/// Wrapper scripts running the tools inside the dev database container,
/// which reach the same server over its own socket (so only the user,
/// password and database are passed on).
async fn container_tools() -> Option<PathBuf> {
    let container =
        std::env::var("TETHER_TEST_PG_CONTAINER").unwrap_or_else(|_| "tether-dev-db-1".to_owned());
    let running = tokio::process::Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", &container])
        .output()
        .await
        .ok()?;
    if String::from_utf8_lossy(&running.stdout).trim() != "true" {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("tether-pg-container-{container}"));
    std::fs::create_dir_all(&dir).unwrap();
    for tool in ["pg_dump", "pg_restore", "psql"] {
        // Written aside and renamed: tests run in parallel.
        let staged = dir.join(format!(".{tool}.{}", random()));
        std::fs::write(
            &staged,
            format!(
                "#!/bin/sh\nexec docker exec -i -e PGUSER -e PGPASSWORD -e PGDATABASE -e PGAPPNAME \
                 {container} {tool} \"$@\"\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staged, dir.join(tool)).unwrap();
    }
    Some(dir)
}

async fn snapshots(db: &PgPool, bin: Option<PathBuf>, dir: &Path) -> Snapshots {
    Snapshots::new(Config {
        dir: dir.to_owned(),
        pg_bin_dir: bin,
        database_url: Secret::new(test_url(db).await),
        key: EncryptionKey::from_hex(&Secret::new(KEY.to_owned())).unwrap(),
    })
    .unwrap()
}

async fn rollback(
    db: &PgPool,
    snapshots: &Snapshots,
    args: Args,
    answer: &str,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    rollback::run(args, db, snapshots, &mut answer.as_bytes(), &mut out).await?;
    Ok(String::from_utf8(out).unwrap())
}

fn args() -> Args {
    Args {
        list: false,
        plugin: None,
        snapshot: None,
        yes: false,
    }
}

async fn character(db: &PgPool, id: i64, name: &str) -> AccountId {
    accounts::sign_in(
        db,
        accounts::Login {
            character_id: id,
            character_name: name,
            owner_hash: "h",
        },
        false,
    )
    .await
    .unwrap()
    .outcome
    .account()
    .unwrap()
}

async fn character_name(db: &PgPool, id: i64) -> Option<String> {
    sqlx::query_scalar("SELECT name FROM core.characters WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .unwrap()
}

async fn count(db: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
}

/// Today's migrations plus one more: the upgrade being rolled back. It
/// changes the schema and existing data.
fn upgrade() -> Migrator {
    let mut migrations: Vec<Migration> = tether_db::MIGRATOR.iter().cloned().collect();
    let next = migrations.iter().map(|m| m.version).max().unwrap() + 1;
    migrations.push(Migration::new(
        next,
        "test upgrade".into(),
        MigrationType::Simple,
        "ALTER TABLE core.accounts ADD COLUMN upgraded boolean NOT NULL DEFAULT true; \
         CREATE TABLE core.upgrade_only (id int PRIMARY KEY); \
         INSERT INTO core.upgrade_only VALUES (1); \
         UPDATE core.characters SET name = 'Renamed By Upgrade';"
            .into_sql_str(),
        false,
    ));
    Migrator::with_migrations(migrations)
}

#[sqlx::test(migrations = false)]
async fn snapshot_migrate_roll_back_and_check_the_data(db: PgPool) {
    let Some(bin) = pg_bin(&db).await else {
        return;
    };
    let dir = temp_dir();
    let snapshots = snapshots(&db, bin, &dir).await;

    // A fresh database has nothing to keep; an up-to-date one nothing
    // pending.
    assert!(
        snapshots
            .before_migrations(&db, &tether_db::MIGRATOR)
            .await
            .unwrap()
            .is_none()
    );
    tether_db::migrate(&db).await.unwrap();
    // As the server records after migrating.
    snapshots.mark_running(&Kind::Core).await;
    let pilot = character(&db, 1001, "Before Pilot").await;
    // Signed in before the snapshot: a rollback still signs everyone out.
    tether_db::auth::create_session(
        &db,
        &[7; 32],
        pilot,
        std::time::Duration::from_secs(3600),
        None,
    )
    .await
    .unwrap();
    assert!(
        snapshots
            .before_migrations(&db, &tether_db::MIGRATOR)
            .await
            .unwrap()
            .is_none()
    );

    // The upgrade: a snapshot first, then its migration.
    let upgrade = upgrade();
    let taken = snapshots
        .before_migrations(&db, &upgrade)
        .await
        .unwrap()
        .expect("pending migrations are snapshotted first");
    assert_eq!(taken.header.kind, Kind::Core);
    assert_eq!(taken.header.reason, Reason::BeforeMigrations);
    assert_eq!(
        taken.header.core_migrations.len(),
        tether_db::MIGRATOR.iter().count()
    );
    assert!(
        taken.header.timescaledb.is_some(),
        "the test database has TimescaleDB, so the restore is wrapped"
    );
    assert!(taken.path.starts_with(dir.join("snapshots")));
    let raw = std::fs::read(&taken.path).unwrap();
    assert!(
        !raw.windows(12).any(|w| w == b"Before Pilot"),
        "the dump is encrypted"
    );
    // A server restarting (say, after a failed migration) doesn't take
    // another of the same state, which would push this one out...
    let again = snapshots
        .before_migrations(&db, &upgrade)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.name, taken.name);
    let files = |dir: &Path| {
        std::fs::read_dir(dir.join("snapshots"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tsnap")
            })
            .count()
    };
    assert_eq!(files(&dir), 1);
    // ...but once it has run since, its data may have changed: a new one.
    snapshots.mark_running(&Kind::Core).await;
    let taken = snapshots
        .before_migrations(&db, &upgrade)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(taken.name, again.name);
    assert_eq!(files(&dir), 2);
    upgrade.run(&db).await.unwrap();
    // Newer data, which rolling back loses.
    character(&db, 1002, "After Pilot").await;
    assert_eq!(
        character_name(&db, 1001).await.as_deref(),
        Some("Renamed By Upgrade")
    );

    // Declining changes nothing.
    let out = rollback(&db, &snapshots, args(), "no\n").await.unwrap();
    assert!(out.contains(&format!("Snapshot: {}", taken.name)), "{out}");
    assert!(out.contains("Taken:"), "{out}");
    assert!(out.contains("before migrations"), "{out}");
    assert!(out.contains("is lost"), "{out}");
    assert!(out.contains("Nothing was changed."), "{out}");
    assert!(character_name(&db, 1002).await.is_some());

    let out = rollback(&db, &snapshots, args(), "yes\n").await.unwrap();
    assert!(
        out.contains(&format!("Rolled back to {}", taken.name)),
        "{out}"
    );

    // The data is as it was: the renamed pilot has its name back, the newer
    // one is gone, and the upgrade's column and table with it.
    assert_eq!(
        character_name(&db, 1001).await.as_deref(),
        Some("Before Pilot")
    );
    assert_eq!(character_name(&db, 1002).await, None);
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM information_schema.columns \
             WHERE table_schema = 'core' AND table_name = 'accounts' AND column_name = 'upgraded'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_class WHERE oid = to_regclass('core.upgrade_only')"
        )
        .await,
        0
    );
    // So is the migration history: today's version starts cleanly, and the
    // upgrade can run again.
    let status = tether_db::migration_status(&db, &tether_db::MIGRATOR)
        .await
        .unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(status.applied, tether_db::MIGRATOR.iter().count());
    tether_db::migrate(&db).await.unwrap();
    // Triggers came back too (the audit log is append-only), and the
    // rollback itself is audited.
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM pg_trigger WHERE tgname = 'audit_log_append_only'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM core.audit_log WHERE action = 'snapshot.restored' \
             AND actor_name = 'cli' AND details ? 'discarded_audit_entries'"
        )
        .await,
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM core.sessions").await, 0);
    upgrade.run(&db).await.unwrap();

    // Nightly backups reuse it, kept apart.
    let backups = snapshots.back_up_all(&db).await.unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path.starts_with(dir.join("backups")));
    let out = rollback(
        &db,
        &snapshots,
        Args {
            list: true,
            ..args()
        },
        "",
    )
    .await
    .unwrap();
    assert!(
        out.contains(&taken.name) && out.contains("nightly backup"),
        "{out}"
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[sqlx::test(migrations = false)]
async fn each_kind_rolls_back_alone(db: PgPool) {
    let Some(bin) = pg_bin(&db).await else {
        return;
    };
    let dir = temp_dir();
    let snapshots = snapshots(&db, bin, &dir).await;
    tether_db::migrate(&db).await.unwrap();

    // An app with storage, set up as installing one does.
    let id = format!("snap{}", random());
    let schema = format!("plugin_{id}");
    let names = Names {
        schema_name: schema.clone(),
        role_name: format!("tp_{}_{id}", random()),
    };
    sqlx::query(
        "INSERT INTO core.plugins (id, name, version, package, signature, package_sha256) \
         VALUES ($1, 'Snapshot test', '1.0.0', '\\x00', 'sig', '\\x00')",
    )
    .bind(&id)
    .execute(&db)
    .await
    .unwrap();
    let password = Secret::new(random());
    let verifier = tether_core::scram::verifier(&password).unwrap();
    let mut tx = db.begin().await.unwrap();
    plugin_storage::create(&mut tx, &id, &names, &verifier, 5, &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // As installing does: the password sealed with the instance key.
    let secret = plugin_storage::password_secret(&id);
    let sealed = EncryptionKey::from_hex(&Secret::new(KEY.to_owned()))
        .unwrap()
        .seal(&password, &secrets::context(&secret))
        .unwrap();
    secrets::put(&db, &secret, &sealed).await.unwrap();
    let app_sql = |sql: String| {
        let db = db.clone();
        async move {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&db)
                .await
                .unwrap();
        }
    };
    app_sql(format!(
        "CREATE TABLE \"{schema}\".notes (id int PRIMARY KEY, body text); \
         ALTER TABLE \"{schema}\".notes OWNER TO \"{role}\"; \
         INSERT INTO \"{schema}\".notes VALUES (1, 'kept');",
        role = names.role_name
    ))
    .await;
    plugin_storage::record_migration(&db, &id, 1, "notes", &[1; 32])
        .await
        .unwrap();
    // The app's own migration, as its role: a CHECK constraint whose
    // function makes the app a superuser whenever a superuser runs it. It
    // runs again whenever the table's data is loaded.
    let options: PgConnectOptions = test_url(&db).await.parse().unwrap();
    let mut as_app = PgConnection::connect_with(
        &options
            .username(&names.role_name)
            .password(password.expose()),
    )
    .await
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE FUNCTION trap(v int) RETURNS boolean LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user) THEN \
             EXECUTE 'ALTER ROLE \"{role}\" SUPERUSER'; \
           END IF; \
           RETURN true; \
         END $$; \
         CREATE TABLE trapped (v int CHECK (trap(v))); \
         INSERT INTO trapped VALUES (1);",
        role = names.role_name
    )))
    .execute(&mut as_app)
    .await
    .unwrap();
    as_app.close().await.unwrap();
    let superuser = |db: PgPool, role: String| async move {
        sqlx::query_scalar::<_, bool>("SELECT rolsuper FROM pg_roles WHERE rolname = $1")
            .bind(role)
            .fetch_one(&db)
            .await
            .unwrap()
    };
    let notes = |db: PgPool, schema: String| async move {
        sqlx::query_as::<_, (i32, String)>(sqlx::AssertSqlSafe(format!(
            "SELECT id, body FROM \"{schema}\".notes ORDER BY id"
        )))
        .fetch_all(&db)
        .await
        .unwrap()
    };

    let app = Kind::Plugin(id.clone());
    let taken = snapshots
        .take(&db, &app, Reason::BeforeMigrations)
        .await
        .unwrap();
    assert_eq!(taken.header.plugin_role.as_deref(), Some(&*names.role_name));
    assert_eq!(taken.header.plugin_migrations.len(), 1);

    // The app's upgrade, and core changing meanwhile.
    app_sql(format!(
        "ALTER TABLE \"{schema}\".notes ADD COLUMN extra int; \
         UPDATE \"{schema}\".notes SET body = 'changed'; \
         INSERT INTO \"{schema}\".notes VALUES (2, 'lost', 0);"
    ))
    .await;
    plugin_storage::record_migration(&db, &id, 2, "extra", &[2; 32])
        .await
        .unwrap();
    character(&db, 2001, "Core Pilot").await;
    // A restore cut short after setting the schema aside: the next one
    // puts it back first.
    let aside = tether_snapshots::set_aside_schema(&id);
    app_sql(format!(
        "ALTER SCHEMA \"{schema}\" RENAME TO \"{aside}\"; CREATE SCHEMA \"{schema}\";"
    ))
    .await;

    let out = rollback(
        &db,
        &snapshots,
        Args {
            plugin: Some(id.clone()),
            yes: true,
            ..args()
        },
        "",
    )
    .await
    .unwrap();
    assert!(out.contains(&format!("app {id}")), "{out}");

    // The app's data and migration records are back; core is untouched.
    assert_eq!(
        notes(db.clone(), schema.clone()).await,
        vec![(1, "kept".to_owned())]
    );
    assert_eq!(
        plugin_storage::applied(&db, &id)
            .await
            .unwrap()
            .into_iter()
            .map(|(v, _)| v)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        character_name(&db, 2001).await.as_deref(),
        Some("Core Pilot")
    );
    // Still the app's own: its role owns the table and can use the schema.
    let owner: String = sqlx::query_scalar(
        "SELECT tableowner::text FROM pg_tables WHERE schemaname = $1 AND tablename = 'notes'",
    )
    .bind(&schema)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(owner, names.role_name);
    let usable: bool = sqlx::query_scalar("SELECT has_schema_privilege($1, $2, 'USAGE, CREATE')")
        .bind(&names.role_name)
        .bind(&schema)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(usable);
    // The temp file cap raised for the load is back to the role's own.
    let raised: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_db_role_setting s JOIN pg_roles r ON r.oid = s.setrole \
         WHERE r.rolname = $1 AND s.setdatabase <> 0",
    )
    .bind(&names.role_name)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(raised, 0);
    // Loaded as the app, so its function never ran as a superuser.
    assert!(!superuser(db.clone(), names.role_name.clone()).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM \"{schema}\".trapped"
        )))
        .fetch_one(&db)
        .await
        .unwrap(),
        1
    );
    let leftover: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
            .bind(&aside)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(!leftover, "the schema set aside is dropped");

    // A core rollback leaves the app's data alone.
    let core = snapshots
        .take(&db, &Kind::Core, Reason::BeforeMigrations)
        .await
        .unwrap();
    character(&db, 2002, "Lost Pilot").await;
    app_sql(format!(
        "INSERT INTO \"{schema}\".notes VALUES (3, 'app data')"
    ))
    .await;
    rollback(
        &db,
        &snapshots,
        Args {
            snapshot: Some(core.name.clone()),
            yes: true,
            ..args()
        },
        "",
    )
    .await
    .unwrap();
    assert_eq!(character_name(&db, 2002).await, None);
    assert_eq!(
        character_name(&db, 2001).await.as_deref(),
        Some("Core Pilot")
    );
    assert_eq!(notes(db.clone(), schema.clone()).await.len(), 2);

    // Nightly backups cover every app with storage too.
    let backups = snapshots.back_up_all(&db).await.unwrap();
    assert_eq!(
        backups
            .iter()
            .map(|b| b.header.kind.clone())
            .collect::<Vec<_>>(),
        vec![Kind::Core, app.clone()]
    );

    // An app that was reinstalled (another role) can't take old data.
    let mut other = taken.clone();
    other.header.plugin_role = Some("tp_someone_else".to_owned());
    let err = snapshots.check(&db, &other).await.unwrap_err();
    assert!(err.to_string().contains("installed again"), "{err}");

    let mut tx = db.begin().await.unwrap();
    plugin_storage::drop(&mut tx, &names).await.unwrap();
    tx.commit().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[sqlx::test(migrations = false)]
async fn rollback_refuses_while_tether_runs(db: PgPool) {
    tether_db::migrate(&db).await.unwrap();
    let dir = temp_dir();
    let snapshots = snapshots(&db, None, &dir).await;
    let options: PgConnectOptions = test_url(&db).await.parse().unwrap();
    let server =
        PgConnection::connect_with(&options.application_name(tether_db::SERVER_APPLICATION_NAME))
            .await
            .unwrap();

    let err = rollback(
        &db,
        &snapshots,
        Args {
            yes: true,
            ..args()
        },
        "",
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("Tether is running"), "{err}");

    server.close().await.unwrap();
    // Stopped, it gets as far as finding no snapshot.
    let err = rollback(&db, &snapshots, args(), "").await.unwrap_err();
    assert!(err.to_string().contains("no snapshot of core"), "{err}");
    let out = rollback(
        &db,
        &snapshots,
        Args {
            list: true,
            ..args()
        },
        "",
    )
    .await
    .unwrap();
    assert!(out.contains("No snapshots or backups yet."), "{out}");

    // A restore holds its lock; the server checks it before starting.
    assert!(!tether_snapshots::rollback_running(&db).await.unwrap());
    let plain: PgConnectOptions = test_url(&db).await.parse().unwrap();
    let mut restoring = PgConnection::connect_with(&plain).await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(0x7465_7468_6572_7262_i64)
        .execute(&mut restoring)
        .await
        .unwrap();
    assert!(tether_snapshots::rollback_running(&db).await.unwrap());
    restoring.close().await.unwrap();
    assert!(!tether_snapshots::rollback_running(&db).await.unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}
