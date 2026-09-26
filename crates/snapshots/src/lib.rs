//! Snapshots, rollback and backups (N14, N7).
//!
//! A snapshot is one kind of data, dumped with `pg_dump` (custom format)
//! and encrypted as it streams to disk: [`Kind::Core`] is the `core`
//! schema plus the core migration history (`public._sqlx_migrations`);
//! [`Kind::Plugin`] is one plugin's `plugin_<id>` schema plus its rows in
//! `core.plugin_migrations`. The migration records ride in the snapshot's
//! header, read in the same database snapshot as the dump.
//!
//! Snapshots are taken before pending core migrations and before a
//! plugin's migrations on an upgrade, into `<dir>/snapshots` (the last
//! [`KEEP_SNAPSHOTS`] per kind); nightly backups are the same thing, into
//! `<dir>/backups` (the last [`KEEP_BACKUPS`] per kind).
//!
//! Restoring replaces that kind's schema and migration records, and a
//! failure changes nothing. `pg_restore` turns the archive back into SQL,
//! and `psql` runs it in a transaction committed only once everything has
//! run and the whole file has checked out. Core's runs as Tether's role
//! between a `DROP SCHEMA` and the migration records (and the audit entry),
//! all in that one transaction. A plugin's runs as the plugin's own role,
//! never Tether's (see [`Snapshots::restore`]). Other kinds are never
//! touched.
//!
//! Files are sealed with a key derived from the instance key
//! ([`KEY_LABEL`]); see [`file`] for the format.

mod file;
mod pg;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool};
use tether_core::Secret;
use tether_core::crypto::{CryptoError, EncryptionKey};
use tether_db::audit::{self, Actor};
use tether_db::plugin_storage;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};

pub use pg::{Tools, free_bytes};

/// The label the snapshot key is derived under.
pub const KEY_LABEL: &str = "tether snapshots v1";
/// Snapshots kept per kind, taken before changes.
pub const KEEP_SNAPSHOTS: usize = 5;
/// Nightly backups kept per kind: a week.
pub const KEEP_BACKUPS: usize = 7;
/// Free space required beyond the estimate of a dump's size.
const HEADROOM: u64 = 64 * 1024 * 1024;
const EXTENSION: &str = ".tsnap";
/// A `.partial` file older than this is left over from a crash.
const STALE_PARTIAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error(
        "{0} wasn't found. The app image includes it (postgresql-client-16); outside Docker, \
         install the Postgres client tools or set PG_BIN_DIR"
    )]
    ToolMissing(&'static str),
    #[error(
        "{tool} is from Postgres {major}, but the database runs Postgres {server_major}; \
         they must match (postgresql-client-{server_major})"
    )]
    ToolVersion {
        tool: &'static str,
        major: u32,
        server_major: u32,
    },
    #[error("{tool} failed: {detail}")]
    Tool { tool: &'static str, detail: String },
    #[error(
        "not enough free disk space in {dir} for a snapshot: {free_mib} MiB free, about \
         {needed_mib} MiB needed"
    )]
    NoSpace {
        dir: String,
        free_mib: u64,
        needed_mib: u64,
    },
    #[error(
        "the snapshot can't be opened with this instance's ENCRYPTION_KEY, or it was changed \
         since it was taken"
    )]
    Decrypt,
    #[error("the snapshot is damaged: {0}")]
    Corrupt(String),
    #[error("the snapshot changed on disk since it was listed")]
    Changed,
    #[error(
        "the snapshot was taken with TimescaleDB {snapshot}, but the database has {current}; \
         restore it only with the same version"
    )]
    TimescaleMismatch { snapshot: String, current: String },
    #[error("the app {0} isn't installed with storage now, so its data can't be restored")]
    PluginMissing(String),
    #[error(
        "the app {0} was uninstalled and installed again since this snapshot, so its data \
         can't be restored into it"
    )]
    PluginReinstalled(String),
    #[error("another rollback is running")]
    Busy,
    #[error(
        "the restore failed ({restore}), and putting the app's data back failed too \
         ({put_back}); its data is kept in schema {aside}: run the rollback again"
    )]
    Stranded {
        restore: String,
        put_back: String,
        aside: String,
    },
    #[error("{0:?} isn't an app id Tether would use")]
    BadPluginId(String),
    #[error("the app {0}'s database password can't be opened with this instance's ENCRYPTION_KEY")]
    PluginPassword(String),
    #[error("DATABASE_URL isn't a valid Postgres URL")]
    BadUrl,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("snapshot header: {0}")]
    Header(#[from] serde_json::Error),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl SnapshotError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// A short reason for pages, without paths or tool output (the log
    /// has the full error).
    pub fn brief(&self) -> &'static str {
        match self {
            Self::NoSpace { .. } => "there isn't enough free disk space for it",
            Self::ToolMissing(_) | Self::ToolVersion { .. } => {
                "the Postgres 16 client tools are missing"
            }
            _ => "the server log and `tether doctor` say why",
        }
    }
}

/// What a snapshot holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Core,
    Plugin(String),
}

impl Kind {
    /// The schema it covers.
    pub fn schema(&self) -> String {
        match self {
            Self::Core => "core".to_owned(),
            Self::Plugin(id) => format!("plugin_{id}"),
        }
    }

    /// Plugin ids are checked on install; checked again here because they
    /// end up in file names, `pg_dump` patterns and SQL.
    fn check(&self) -> Result<(), SnapshotError> {
        match self {
            Self::Core => Ok(()),
            Self::Plugin(id) if safe_name(id) && safe_name(&self.schema()) => Ok(()),
            Self::Plugin(id) => Err(SnapshotError::BadPluginId(id.clone())),
        }
    }

    fn file_prefix(&self) -> String {
        match self {
            Self::Core => "core".to_owned(),
            Self::Plugin(id) => format!("plugin.{id}"),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => f.write_str("core"),
            Self::Plugin(id) => write!(f, "app {id}"),
        }
    }
}

/// `[a-z0-9._-]`, 1 to 63 bytes: the names Tether gives schemas and roles.
fn safe_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
        })
}

/// Why it was taken, which decides where it's kept and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Before migrations: core's at startup, a plugin's on an upgrade.
    BeforeMigrations,
    /// The nightly backup.
    Nightly,
}

impl Reason {
    fn subdir(self) -> &'static str {
        match self {
            Self::BeforeMigrations => "snapshots",
            Self::Nightly => "backups",
        }
    }

    fn keep(self) -> usize {
        match self {
            Self::BeforeMigrations => KEEP_SNAPSHOTS,
            Self::Nightly => KEEP_BACKUPS,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::BeforeMigrations => "before migrations",
            Self::Nightly => "nightly backup",
        }
    }
}

/// A core migration as recorded in `public._sqlx_migrations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreMigration {
    pub version: i64,
    pub description: String,
    pub installed_on: DateTime<Utc>,
    pub success: bool,
    /// Hex.
    pub checksum: String,
    pub execution_time: i64,
}

/// A plugin migration as recorded in `core.plugin_migrations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMigration {
    pub version: i32,
    pub name: String,
    /// Hex.
    pub sha256: String,
    pub applied_at: DateTime<Utc>,
}

/// A snapshot's plaintext (but authenticated) header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub kind: Kind,
    pub reason: Reason,
    pub taken_at: DateTime<Utc>,
    pub tether_version: String,
    /// `server_version_num`, e.g. 160015.
    pub postgres_version: i32,
    /// The TimescaleDB extension's version, if it was installed.
    pub timescaledb: Option<String>,
    /// A plugin snapshot's database role: the plugin's tables belong to it.
    pub plugin_role: Option<String>,
    #[serde(default)]
    pub core_migrations: Vec<CoreMigration>,
    #[serde(default)]
    pub plugin_migrations: Vec<PluginMigration>,
}

/// A snapshot or backup on disk.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub path: PathBuf,
    /// The file name, which `tether rollback --snapshot` takes.
    pub name: String,
    /// On disk.
    pub bytes: u64,
    pub header: Header,
}

/// What [`Snapshots`] needs.
pub struct Config {
    /// The snapshots volume (`SNAPSHOT_DIR`).
    pub dir: PathBuf,
    /// Where the Postgres client tools are (`PG_BIN_DIR`); `None` for PATH.
    pub pg_bin_dir: Option<PathBuf>,
    /// The database the tools connect to (`DATABASE_URL`).
    pub database_url: Secret<String>,
    /// The instance key; snapshots use a key derived from it.
    pub key: EncryptionKey,
}

/// Takes and restores snapshots. `Debug` shows no secrets.
#[derive(Clone)]
pub struct Snapshots {
    dir: PathBuf,
    tools: Tools,
    conn: pg::Conn,
    /// Derived: seals and opens snapshot files.
    key: EncryptionKey,
    /// The instance key: opens a plugin's database password (a restore
    /// logs in as the plugin).
    instance_key: EncryptionKey,
}

impl std::fmt::Debug for Snapshots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshots")
            .field("dir", &self.dir)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl Snapshots {
    pub fn new(config: Config) -> Result<Self, SnapshotError> {
        Ok(Self {
            conn: pg::Conn::from_url(&config.database_url)?,
            dir: config.dir,
            tools: Tools::new(config.pg_bin_dir),
            key: config.key.derive(KEY_LABEL)?,
            instance_key: config.key,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    /// Takes a core snapshot if `migrator` has migrations to apply to a
    /// database that already has some (a fresh one has nothing to keep).
    pub async fn before_migrations(
        &self,
        db: &PgPool,
        migrator: &Migrator,
    ) -> Result<Option<Snapshot>, SnapshotError> {
        let status = tether_db::migration_status(db, migrator).await?;
        if status.pending == 0 || status.applied == 0 {
            return Ok(None);
        }
        if let Some(unchanged) = self.unchanged_since(db, &Kind::Core).await? {
            return Ok(Some(unchanged));
        }
        tracing::info!(
            pending = status.pending,
            "taking a snapshot before core migrations"
        );
        self.take(db, &Kind::Core, Reason::BeforeMigrations)
            .await
            .map(Some)
    }

    /// Takes a plugin snapshot before its pending migrations (an upgrade),
    /// or reuses one of exactly this state (see [`Snapshots::mark_running`]).
    pub async fn before_plugin_migrations(
        &self,
        db: &PgPool,
        plugin_id: &str,
    ) -> Result<Snapshot, SnapshotError> {
        let kind = Kind::Plugin(plugin_id.to_owned());
        if let Some(unchanged) = self.unchanged_since(db, &kind).await? {
            return Ok(unchanged);
        }
        self.take(db, &kind, Reason::BeforeMigrations).await
    }

    /// Records that `kind` ran (core: the server migrated and started; a
    /// plugin: it loaded), so its data may have changed since any snapshot
    /// taken before now. Failures are logged: at worst, one more snapshot.
    pub async fn mark_running(&self, kind: &Kind) {
        let written = async {
            create_private_dir(&self.dir).await?;
            tokio::fs::write(self.ran_marker(kind), Utc::now().to_rfc3339())
                .await
                .map_err(|e| SnapshotError::io("recording a start", e))
        };
        if let Err(err) = written.await {
            tracing::warn!(kind = %kind, error = %err, "recording a start for snapshots");
        }
    }

    fn ran_marker(&self, kind: &Kind) -> PathBuf {
        self.dir.join(format!(".ran-{}", kind.file_prefix()))
    }

    /// The newest snapshot taken before migrations, if the data can't have
    /// changed since: `kind` hasn't run since it was taken (a server or
    /// plugin that fails its migrations never gets to), and the same
    /// migrations are applied. Restarting after a failed migration would
    /// otherwise take one each time and push the one from before the
    /// upgrade out of the last five.
    async fn unchanged_since(
        &self,
        db: &PgPool,
        kind: &Kind,
    ) -> Result<Option<Snapshot>, SnapshotError> {
        let Some(ran) = tokio::fs::read_to_string(self.ran_marker(kind))
            .await
            .ok()
            .and_then(|t| DateTime::parse_from_rfc3339(t.trim()).ok())
        else {
            return Ok(None);
        };
        let newest = list_in(&self.dir.join(Reason::BeforeMigrations.subdir()))
            .await?
            .into_iter()
            .find(|s| s.header.kind == *kind);
        let Some(newest) = newest.filter(|s| s.header.taken_at > ran) else {
            return Ok(None);
        };
        let same = match kind {
            Kind::Core => {
                successful(&newest.header.core_migrations) == applied_versions(db).await?
            }
            Kind::Plugin(id) => {
                let role = plugin_storage::get(db, id).await?.map(|n| n.role_name);
                let applied: Vec<i32> = plugin_storage::applied(db, id)
                    .await?
                    .into_iter()
                    .map(|(v, _)| v)
                    .collect();
                role == newest.header.plugin_role
                    && applied
                        == newest
                            .header
                            .plugin_migrations
                            .iter()
                            .map(|m| m.version)
                            .collect::<Vec<_>>()
            }
        };
        if !same {
            return Ok(None);
        }
        tracing::info!(
            snapshot = newest.name,
            kind = %kind,
            "reusing the snapshot of this same state (a restart after failed migrations?)"
        );
        Ok(Some(newest))
    }

    /// The nightly backup: core, then every plugin with storage. One kind
    /// failing doesn't stop the others; the first error is returned.
    pub async fn back_up_all(&self, db: &PgPool) -> Result<Vec<Snapshot>, SnapshotError> {
        let mut kinds = vec![Kind::Core];
        kinds.extend(
            tether_db::plugin_storage::plugin_ids(db)
                .await?
                .into_iter()
                .map(Kind::Plugin),
        );
        let mut taken = Vec::new();
        let mut first_error = None;
        for kind in kinds {
            match self.take(db, &kind, Reason::Nightly).await {
                Ok(snapshot) => taken.push(snapshot),
                Err(err) => {
                    tracing::error!(kind = %kind, error = %err, "nightly backup");
                    first_error.get_or_insert(err);
                }
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(taken),
        }
    }

    /// Dumps one kind into an encrypted snapshot, then drops the oldest of
    /// that kind beyond what's kept.
    pub async fn take(
        &self,
        db: &PgPool,
        kind: &Kind,
        reason: Reason,
    ) -> Result<Snapshot, SnapshotError> {
        kind.check()?;
        let postgres_version = server_version(db).await?;
        self.tools.check(server_major(postgres_version)).await?;
        let dir = self.dir.join(reason.subdir());
        create_private_dir(&dir).await?;
        let schema = kind.schema();

        let needed = schema_bytes(db, &schema).await?.saturating_add(HEADROOM);
        let free = free_bytes(&dir).await?;
        if free < needed {
            return Err(SnapshotError::NoSpace {
                dir: dir.display().to_string(),
                free_mib: free / (1024 * 1024),
                needed_mib: needed / (1024 * 1024),
            });
        }

        // One view of the database for the header's records and the dump:
        // pg_dump joins this transaction's snapshot.
        let mut tx = db.begin().await?;
        sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let exported = sqlx::query_scalar!(r#"SELECT pg_export_snapshot() AS "id!""#)
            .fetch_one(&mut *tx)
            .await?;
        let mut header = Header {
            kind: kind.clone(),
            reason,
            taken_at: Utc::now(),
            tether_version: env!("CARGO_PKG_VERSION").to_owned(),
            postgres_version,
            timescaledb: timescaledb_version(&mut *tx).await?,
            plugin_role: None,
            core_migrations: Vec::new(),
            plugin_migrations: Vec::new(),
        };
        match kind {
            Kind::Core => header.core_migrations = core_migrations(&mut tx).await?,
            Kind::Plugin(id) => {
                let names = tether_db::plugin_storage::get(&mut *tx, id)
                    .await?
                    .ok_or_else(|| SnapshotError::PluginMissing(id.clone()))?;
                header.plugin_role = Some(names.role_name);
                header.plugin_migrations = plugin_migrations(&mut tx, id).await?;
            }
        }
        let header_json = serde_json::to_vec(&header)?;

        let name = format!(
            "{}-{}-{}{EXTENSION}",
            kind.file_prefix(),
            header.taken_at.format("%Y%m%dT%H%M%SZ"),
            random_hex()?
        );
        let path = dir.join(&name);
        let partial = Partial(Some(dir.join(format!(".{name}.partial"))));

        let mut command = self.tools.command("pg_dump");
        self.conn.apply(&mut command);
        command
            .args([
                "--format=custom",
                "--no-password",
                "--strict-names",
                "--lock-wait-timeout=60s",
            ])
            // Quoted: an exact name, not a pattern.
            .arg(format!("--schema=\"{schema}\""))
            .arg(format!("--snapshot={exported}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| pg::spawn_error("pg_dump", e))?;
        let (Some(mut stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(SnapshotError::Tool {
                tool: "pg_dump",
                detail: "its output couldn't be read".to_owned(),
            });
        };
        let stderr = tokio::spawn(pg::read_tail(stderr));

        let file = create_private_file(partial.path()).await?;
        let mut out = BufWriter::new(file);
        let sealed = file::seal(&self.key, &header_json, &mut stdout, &mut out).await;
        // If sealing stopped early, pg_dump mustn't wait on a full pipe.
        drop(stdout);
        if sealed.is_err() {
            let _ = child.start_kill();
        }
        let status = child
            .wait()
            .await
            .map_err(|e| SnapshotError::io("waiting for pg_dump", e))?;
        let stderr = stderr.await.unwrap_or_default();
        let plain_bytes = sealed?;
        if !status.success() {
            return Err(SnapshotError::Tool {
                tool: "pg_dump",
                detail: failure(status, &stderr),
            });
        }
        tx.commit().await?;
        let file = out.into_inner();
        file.sync_all()
            .await
            .map_err(|e| SnapshotError::io("saving the snapshot", e))?;
        drop(file);
        tokio::fs::rename(partial.path(), &path)
            .await
            .map_err(|e| SnapshotError::io("saving the snapshot", e))?;
        partial.keep();
        sync_dir(&dir).await;
        let bytes = tokio::fs::metadata(&path)
            .await
            .map_err(|e| SnapshotError::io("reading the snapshot", e))?
            .len();
        tracing::info!(
            snapshot = name,
            kind = %kind,
            reason = reason.describe(),
            bytes,
            dumped_bytes = plain_bytes,
            "snapshot taken"
        );
        prune(&dir, kind, reason.keep()).await;
        Ok(Snapshot {
            path,
            name,
            bytes,
            header,
        })
    }

    /// Refuses a restore that can't work or can't be trusted, before
    /// anything changes: the tools, TimescaleDB's version, and for a
    /// plugin, that it's installed with the same role.
    pub async fn check(&self, db: &PgPool, snapshot: &Snapshot) -> Result<(), SnapshotError> {
        let header = &snapshot.header;
        header.kind.check()?;
        self.tools
            .check(server_major(server_version(db).await?))
            .await?;
        let current = timescaledb_version(db).await?;
        if current != header.timescaledb {
            let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_owned());
            return Err(SnapshotError::TimescaleMismatch {
                snapshot: show(&header.timescaledb),
                current: show(&current),
            });
        }
        if let Kind::Plugin(id) = &header.kind {
            let names = tether_db::plugin_storage::get(db, id)
                .await?
                .ok_or_else(|| SnapshotError::PluginMissing(id.clone()))?;
            if header.plugin_role.as_deref() != Some(names.role_name.as_str()) {
                return Err(SnapshotError::PluginReinstalled(id.clone()));
            }
        }
        Ok(())
    }

    /// Reads the whole snapshot and checks every chunk, changing nothing.
    pub async fn verify(&self, snapshot: &Snapshot) -> Result<(), SnapshotError> {
        let (mut input, head) = self.open(snapshot).await?;
        file::open(&self.key, &head, &mut input, &mut tokio::io::sink()).await?;
        Ok(())
    }

    /// The file, positioned after its header, which must still be the one
    /// listed.
    async fn open(
        &self,
        snapshot: &Snapshot,
    ) -> Result<(BufReader<tokio::fs::File>, Vec<u8>), SnapshotError> {
        let file = tokio::fs::File::open(&snapshot.path)
            .await
            .map_err(|e| SnapshotError::io(format!("opening {}", snapshot.name), e))?;
        let mut input = BufReader::new(file);
        let (head, json) = file::read_head(&mut input).await?;
        let header: Header = serde_json::from_slice(&json)?;
        if header != snapshot.header {
            return Err(SnapshotError::Changed);
        }
        Ok((input, head))
    }

    /// Puts a snapshot's kind back as it was. Newer data of that kind is
    /// lost; other kinds are untouched. Checks first
    /// ([`Snapshots::check`]), reads the whole file once to be sure it
    /// opens, and wraps the restore in TimescaleDB's pre- and post-restore
    /// calls when it's installed. Audited as `snapshot.restored`, in the
    /// transaction that finishes the restore. Nothing else may be using
    /// the data: stop the server first (see [`server_connections`]).
    ///
    /// A plugin's schema is restored as the plugin's own role, never as
    /// Tether's: the dump holds the plugin's functions, and CHECK
    /// constraints, domains or generated columns would run them as whoever
    /// loads the data. Only Tether's role can replace the schema, so that
    /// takes three steps: the schema is set aside and an empty one made in
    /// its place; the dump is loaded into it as the plugin; then the old one
    /// is dropped and the migration records are put back. A failure puts the
    /// old schema back, and a restore cut short is undone by the next one.
    pub async fn restore(
        &self,
        db: &PgPool,
        snapshot: &Snapshot,
        actor: Actor,
    ) -> Result<(), SnapshotError> {
        // One restore at a time, and the server won't start during one
        // (see `rollback_running`). Held by a connection of its own, which
        // closing releases even if this is cancelled.
        let mut lock = db.acquire().await?.detach();
        let got = sqlx::query_scalar!(r#"SELECT pg_try_advisory_lock($1) AS "got!""#, RESTORE_LOCK)
            .fetch_one(&mut lock)
            .await?;
        if !got {
            return Err(SnapshotError::Busy);
        }
        let restored = self.restore_locked(db, snapshot, actor).await;
        let _ = sqlx::Connection::close(lock).await;
        restored
    }

    async fn restore_locked(
        &self,
        db: &PgPool,
        snapshot: &Snapshot,
        actor: Actor,
    ) -> Result<(), SnapshotError> {
        self.check(db, snapshot).await?;
        self.verify(snapshot).await?;
        let timescale = snapshot.header.timescaledb.is_some();
        if timescale {
            sqlx::raw_sql("SELECT public.timescaledb_pre_restore()")
                .execute(db)
                .await?;
        }
        let restored = match &snapshot.header.kind {
            Kind::Core => self.restore_core(db, snapshot, actor).await,
            Kind::Plugin(id) => self.restore_plugin(db, snapshot, id, actor).await,
        };
        if timescale {
            let post = sqlx::raw_sql("SELECT public.timescaledb_post_restore()")
                .execute(db)
                .await;
            match (post, &restored) {
                (Err(err), Ok(())) => return Err(err.into()),
                (Err(err), Err(_)) => {
                    tracing::error!(error = %err, "timescaledb_post_restore after a failed restore");
                }
                (Ok(_), _) => {}
            }
        }
        if restored.is_ok() {
            tracing::info!(
                snapshot = snapshot.name,
                kind = %snapshot.header.kind,
                taken_at = %snapshot.header.taken_at,
                "snapshot restored"
            );
        }
        restored
    }

    async fn restore_core(
        &self,
        db: &PgPool,
        snapshot: &Snapshot,
        actor: Actor,
    ) -> Result<(), SnapshotError> {
        // What the rollback throws away of the audit log, recorded with it.
        let discarded = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM core.audit_log WHERE at > $1"#,
            snapshot.header.taken_at
        )
        .fetch_one(db)
        .await?;
        let (prologue, epilogue) = core_scripts(snapshot, actor, discarded)?;
        self.run_script(
            snapshot,
            &["--format=custom", "--file=-"],
            None,
            &prologue,
            &epilogue,
        )
        .await
    }

    async fn restore_plugin(
        &self,
        db: &PgPool,
        snapshot: &Snapshot,
        id: &str,
        actor: Actor,
    ) -> Result<(), SnapshotError> {
        let header = &snapshot.header;
        let schema = header.kind.schema();
        let names = plugin_storage::get(db, id)
            .await?
            .ok_or_else(|| SnapshotError::PluginMissing(id.to_owned()))?;
        let role = names.role_name;
        if names.schema_name != schema || !safe_name(&role) {
            return Err(SnapshotError::BadPluginId(id.to_owned()));
        }
        let password = self.plugin_password(db, id).await?;
        let aside = set_aside_schema(id);

        // 1. The current schema set aside, an empty one in its place.
        let mut tx = db.begin().await?;
        ddl(&mut tx, "SET LOCAL lock_timeout = '30s'".to_owned()).await?;
        if schema_exists(&mut tx, &aside).await? {
            tracing::warn!(
                plugin = id,
                "an earlier restore was cut short; putting the data from before it back first"
            );
            ddl(
                &mut tx,
                format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"),
            )
            .await?;
            ddl(
                &mut tx,
                format!("ALTER SCHEMA \"{aside}\" RENAME TO \"{schema}\""),
            )
            .await?;
        }
        ddl(
            &mut tx,
            format!("ALTER SCHEMA \"{schema}\" RENAME TO \"{aside}\""),
        )
        .await?;
        // As plugin_storage::create makes it.
        ddl(&mut tx, format!("CREATE SCHEMA \"{schema}\"")).await?;
        ddl(
            &mut tx,
            format!("REVOKE ALL ON SCHEMA \"{schema}\" FROM PUBLIC"),
        )
        .await?;
        ddl(
            &mut tx,
            format!("GRANT USAGE, CREATE ON SCHEMA \"{schema}\" TO \"{role}\""),
        )
        .await?;
        // The role's temp file cap is for its own queries; building all its
        // indexes at once may need more, and only Tether can raise it. This
        // database-level override beats the role's own setting, step 3
        // removes it, and loading the plugin removes one a crash left. It
        // stays finite: the plugin's own functions run during the load.
        let database = quoted_database(&mut tx).await?;
        let limit_kib = restore_temp_limit_kib(snapshot.bytes);
        ddl(
            &mut tx,
            format!(
                "ALTER ROLE \"{role}\" IN DATABASE {database} SET temp_file_limit = '{limit_kib}kB'"
            ),
        )
        .await?;
        tx.commit().await?;

        // 2. The dump, as the plugin: its objects (not the schema), owned
        //    by it, without the dump's grants.
        let only = format!("--schema={schema}");
        let loaded = self
            .run_script(
                snapshot,
                &[
                    "--format=custom",
                    "--file=-",
                    &only,
                    "--no-owner",
                    "--no-privileges",
                ],
                Some((&role, &password)),
                "BEGIN;\n",
                "\nCOMMIT;\n",
            )
            .await;

        // 3. Keep it and put the records back, or put the old schema back.
        if let Err(err) = loaded {
            tracing::error!(plugin = id, error = %err, "restore failed; putting the app's data back");
            let put_back = async {
                let mut tx = db.begin().await?;
                ddl(&mut tx, "SET LOCAL lock_timeout = '30s'".to_owned()).await?;
                ddl(
                    &mut tx,
                    format!("ALTER ROLE \"{role}\" IN DATABASE {database} RESET temp_file_limit"),
                )
                .await?;
                ddl(
                    &mut tx,
                    format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"),
                )
                .await?;
                ddl(
                    &mut tx,
                    format!("ALTER SCHEMA \"{aside}\" RENAME TO \"{schema}\""),
                )
                .await?;
                tx.commit().await
            };
            return match put_back.await {
                Ok(()) => Err(err),
                Err(put_back) => Err(SnapshotError::Stranded {
                    restore: err.to_string(),
                    put_back: put_back.to_string(),
                    aside,
                }),
            };
        }
        let mut tx = db.begin().await?;
        ddl(&mut tx, "SET LOCAL lock_timeout = '30s'".to_owned()).await?;
        ddl(
            &mut tx,
            format!("ALTER ROLE \"{role}\" IN DATABASE {database} RESET temp_file_limit"),
        )
        .await?;
        ddl(&mut tx, format!("DROP SCHEMA \"{aside}\" CASCADE")).await?;
        sqlx::query!(
            "DELETE FROM core.plugin_migrations WHERE plugin_id = $1",
            id
        )
        .execute(&mut *tx)
        .await?;
        for m in &header.plugin_migrations {
            sqlx::query!(
                r#"
                INSERT INTO core.plugin_migrations (plugin_id, version, name, sha256, applied_at)
                VALUES ($1, $2, $3, $4, $5)
                "#,
                id,
                m.version,
                m.name,
                unhex(&m.sha256)?,
                m.applied_at,
            )
            .execute(&mut *tx)
            .await?;
        }
        audit::record(
            &mut *tx,
            actor,
            RESTORED,
            Some(&format!("plugin:{id}")),
            audit_details(snapshot, None),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// A plugin's database password, sealed in `core.secrets`.
    async fn plugin_password(
        &self,
        db: &PgPool,
        id: &str,
    ) -> Result<Secret<String>, SnapshotError> {
        let name = plugin_storage::password_secret(id);
        let sealed = tether_db::secrets::get(db, &name)
            .await?
            .ok_or_else(|| SnapshotError::PluginPassword(id.to_owned()))?;
        self.instance_key
            .open(&sealed, &tether_db::secrets::context(&name))
            .map_err(|_| SnapshotError::PluginPassword(id.to_owned()))
    }

    /// Runs `pg_restore`'s SQL for the snapshot through `psql` (as
    /// `login`, or Tether's role), between `prologue` (which begins the
    /// transaction) and `epilogue` (which commits it). The epilogue is sent
    /// only if the whole file opened and both tools succeeded; otherwise
    /// psql's input ends inside the transaction, which Postgres rolls back.
    async fn run_script(
        &self,
        snapshot: &Snapshot,
        restore_args: &[&str],
        login: Option<(&str, &Secret<String>)>,
        prologue: &str,
        epilogue: &str,
    ) -> Result<(), SnapshotError> {
        let (mut input, head) = self.open(snapshot).await?;

        let mut restore = self.tools.command("pg_restore");
        restore
            .args(restore_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut restore = restore
            .spawn()
            .map_err(|e| pg::spawn_error("pg_restore", e))?;
        let mut psql = self.tools.command("psql");
        match login {
            Some((user, password)) => self.conn.apply_as(&mut psql, user, Some(password)),
            None => self.conn.apply(&mut psql),
        }
        psql.args([
            "--no-psqlrc",
            "--quiet",
            "--no-password",
            "--set=ON_ERROR_STOP=1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        let mut psql = psql.spawn().map_err(|e| pg::spawn_error("psql", e))?;

        let pipes = (
            restore.stdin.take(),
            restore.stdout.take(),
            restore.stderr.take(),
            psql.stdin.take(),
            psql.stderr.take(),
        );
        let (
            Some(mut restore_in),
            Some(mut restore_out),
            Some(restore_err),
            Some(mut psql_in),
            Some(psql_err),
        ) = pipes
        else {
            return Err(SnapshotError::Tool {
                tool: "pg_restore",
                detail: "its pipes couldn't be opened".to_owned(),
            });
        };
        let restore_err = tokio::spawn(pg::read_tail(restore_err));
        let psql_err = tokio::spawn(pg::read_tail(psql_err));

        let feed = async {
            let opened = file::open(&self.key, &head, &mut input, &mut restore_in).await;
            // pg_restore sees the end of its input either way.
            let _ = restore_in.shutdown().await;
            drop(restore_in);
            opened
        };
        // Owns pg_restore's output, so if psql stops taking it, pg_restore
        // stops too instead of waiting on a full pipe.
        let prologue = prologue.as_bytes();
        let pipe = async move {
            psql_in.write_all(prologue).await?;
            tokio::io::copy(&mut restore_out, &mut psql_in).await?;
            Ok::<_, std::io::Error>(psql_in)
        };
        let (fed, piped) = tokio::join!(feed, pipe);
        let restore_status = restore
            .wait()
            .await
            .map_err(|e| SnapshotError::io("waiting for pg_restore", e))?;

        let committed = match (&fed, piped, restore_status.success()) {
            (Ok(_), Ok(mut psql_in), true) => {
                let sent = psql_in.write_all(epilogue.as_bytes()).await;
                let _ = psql_in.shutdown().await;
                sent
            }
            (_, piped, _) => {
                drop(piped);
                let _ = psql.start_kill();
                Err(std::io::Error::other("not committed"))
            }
        };
        let psql_status = psql
            .wait()
            .await
            .map_err(|e| SnapshotError::io("waiting for psql", e))?;
        let restore_err = restore_err.await.unwrap_or_default();
        let psql_err = psql_err.await.unwrap_or_default();

        // The first cause: a bad file, then an SQL error (which stops
        // pg_restore and the feed in turn), then the rest.
        if let Err(err @ (SnapshotError::Decrypt | SnapshotError::Corrupt(_))) = fed {
            return Err(err);
        }
        if !psql_status.success() && !psql_err.is_empty() {
            return Err(SnapshotError::Tool {
                tool: "psql",
                detail: failure(psql_status, &psql_err),
            });
        }
        if !restore_status.success() {
            return Err(SnapshotError::Tool {
                tool: "pg_restore",
                detail: failure(restore_status, &restore_err),
            });
        }
        fed?;
        if !psql_status.success() {
            return Err(SnapshotError::Tool {
                tool: "psql",
                detail: failure(psql_status, &psql_err),
            });
        }
        committed.map_err(|e| SnapshotError::io("restoring", e))?;
        Ok(())
    }
}

/// Every snapshot and backup in `dir`, newest first. Needs no key: the
/// headers are plaintext. Files that can't be read are skipped (and
/// logged).
pub async fn list(dir: &Path) -> Result<Vec<Snapshot>, SnapshotError> {
    let mut all = Vec::new();
    for reason in [Reason::BeforeMigrations, Reason::Nightly] {
        all.extend(list_in(&dir.join(reason.subdir())).await?);
    }
    all.sort_by_key(|s| std::cmp::Reverse(s.header.taken_at));
    Ok(all)
}

async fn list_in(dir: &Path) -> Result<Vec<Snapshot>, SnapshotError> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(SnapshotError::io(format!("reading {}", dir.display()), e)),
    };
    let mut found = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| SnapshotError::io(format!("reading {}", dir.display()), e))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.ends_with(EXTENSION) {
            continue;
        }
        match read_snapshot(entry.path(), name.clone()).await {
            Ok(snapshot) => found.push(snapshot),
            Err(err) => {
                tracing::warn!(file = name, error = %err, "skipping an unreadable snapshot")
            }
        }
    }
    found.sort_by_key(|s| std::cmp::Reverse(s.header.taken_at));
    Ok(found)
}

async fn read_snapshot(path: PathBuf, name: String) -> Result<Snapshot, SnapshotError> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| SnapshotError::io(format!("opening {name}"), e))?;
    let bytes = file
        .metadata()
        .await
        .map_err(|e| SnapshotError::io(format!("reading {name}"), e))?
        .len();
    let mut input = BufReader::new(file);
    let (_, json) = file::read_head(&mut input).await?;
    let header: Header = serde_json::from_slice(&json)?;
    header.kind.check()?;
    Ok(Snapshot {
        path,
        name,
        bytes,
        header,
    })
}

/// Drops the oldest snapshots of `kind` in `dir` beyond `keep`, and
/// partial files left by a crash. Failures are logged: a snapshot was
/// just taken, which is what matters.
async fn prune(dir: &Path, kind: &Kind, keep: usize) {
    match list_in(dir).await {
        Ok(snapshots) => {
            for old in snapshots
                .iter()
                .filter(|s| s.header.kind == *kind)
                .skip(keep)
            {
                match tokio::fs::remove_file(&old.path).await {
                    Ok(()) => tracing::info!(snapshot = old.name, "old snapshot removed"),
                    Err(err) => {
                        tracing::warn!(snapshot = old.name, error = %err, "removing an old snapshot");
                    }
                }
            }
        }
        Err(err) => tracing::warn!(error = %err, "listing snapshots to prune"),
    }
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = match entry.metadata().await.and_then(|m| m.modified()) {
            Ok(modified) => modified.elapsed().is_ok_and(|age| age > STALE_PARTIAL),
            Err(_) => false,
        };
        if name.starts_with('.') && name.ends_with(".partial") && stale {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// Client connections from a running Tether server (or its plugins) to
/// this database. A restore must not run while there are any.
pub async fn server_connections(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "n!" FROM pg_stat_activity
        WHERE datname = current_database() AND pid <> pg_backend_pid()
          AND (application_name = $1 OR application_name LIKE 'tether plugin %')
        "#,
        tether_db::SERVER_APPLICATION_NAME,
    )
    .fetch_one(db)
    .await
}

async fn server_version(db: &PgPool) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT current_setting('server_version_num')::int AS "v!""#)
        .fetch_one(db)
        .await
}

fn server_major(version_num: i32) -> u32 {
    u32::try_from(version_num / 10000).unwrap_or_default()
}

async fn timescaledb_version<'e>(
    executor: impl sqlx::PgExecutor<'e>,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT extversion FROM pg_extension WHERE extname = 'timescaledb'")
        .fetch_optional(executor)
        .await
}

/// Roughly what a schema's dump holds: its tables' data (with TOAST),
/// not their indexes.
async fn schema_bytes(db: &PgPool, schema: &str) -> Result<u64, sqlx::Error> {
    let bytes = sqlx::query_scalar!(
        r#"
        SELECT coalesce(sum(pg_table_size(c.oid)), 0)::bigint AS "bytes!"
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = $1 AND c.relkind IN ('r', 'm', 'p')
        "#,
        schema
    )
    .fetch_one(db)
    .await?;
    Ok(u64::try_from(bytes).unwrap_or_default())
}

/// Successful core migrations, in order.
async fn applied_versions(db: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT version FROM public._sqlx_migrations WHERE success ORDER BY version"
    )
    .fetch_all(db)
    .await
}

fn successful(migrations: &[CoreMigration]) -> Vec<i64> {
    let mut versions: Vec<i64> = migrations
        .iter()
        .filter(|m| m.success)
        .map(|m| m.version)
        .collect();
    versions.sort_unstable();
    versions
}

/// After a restore cut short between TimescaleDB's pre- and post-restore
/// calls, the database stays in restore mode (its background jobs off).
/// Run at startup: switches it back and says whether it had to.
pub async fn finish_interrupted_restore(db: &PgPool) -> Result<bool, sqlx::Error> {
    let restoring = sqlx::query_scalar!(
        r#"SELECT current_setting('timescaledb.restoring', true) AS "restoring""#
    )
    .fetch_one(db)
    .await?;
    if restoring.as_deref() != Some("on") {
        return Ok(false);
    }
    sqlx::raw_sql("SELECT public.timescaledb_post_restore()")
        .execute(db)
        .await?;
    Ok(true)
}

async fn core_migrations(tx: &mut sqlx::PgConnection) -> Result<Vec<CoreMigration>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT version, description, installed_on, success, checksum, execution_time
        FROM public._sqlx_migrations ORDER BY version
        "#
    )
    .fetch_all(tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| CoreMigration {
            version: r.version,
            description: r.description,
            installed_on: r.installed_on,
            success: r.success,
            checksum: hex(&r.checksum),
            execution_time: r.execution_time,
        })
        .collect())
}

async fn plugin_migrations(
    tx: &mut sqlx::PgConnection,
    plugin_id: &str,
) -> Result<Vec<PluginMigration>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT version, name, sha256, applied_at FROM core.plugin_migrations
        WHERE plugin_id = $1 ORDER BY version
        "#,
        plugin_id
    )
    .fetch_all(tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| PluginMigration {
            version: r.version,
            name: r.name,
            sha256: hex(&r.sha256),
            applied_at: r.applied_at,
        })
        .collect())
}

const RESTORED: &str = "snapshot.restored";
/// The advisory lock a restore holds (`tether rollback` in ASCII).
const RESTORE_LOCK: i64 = 0x7465_7468_6572_7262;

/// Whether a restore is running (it holds [`RESTORE_LOCK`]). The server
/// checks at startup and won't start during one.
pub async fn rollback_running(db: &PgPool) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let got = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_xact_lock($1) AS "got!""#,
        RESTORE_LOCK
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(!got)
}

/// Schemas a plugin restore cut short left set aside, holding that
/// plugin's data from before it. `doctor` reports them; the plugin won't
/// load until a rollback finishes.
pub async fn set_aside_schemas(db: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT nspname::text AS "name!" FROM pg_namespace WHERE nspname LIKE 'rollback\_%' ORDER BY 1"#
    )
    .fetch_all(db)
    .await
}

/// Whether a restore of this plugin was cut short.
pub async fn set_aside_exists(db: &PgPool, plugin_id: &str) -> Result<bool, sqlx::Error> {
    let mut conn = db.acquire().await?;
    schema_exists(&mut conn, &set_aside_schema(plugin_id)).await
}

/// The temp file cap for loading a plugin's dump: eight times the
/// (compressed) snapshot, at least 1 GiB and at most 64 GiB.
fn restore_temp_limit_kib(snapshot_bytes: u64) -> u64 {
    const GIB_KIB: u64 = 1024 * 1024;
    (snapshot_bytes.saturating_mul(8) / 1024).clamp(GIB_KIB, 64 * GIB_KIB)
}

/// Removes a temp file cap a restore of this plugin raised and couldn't
/// put back (it was cut short). Run before the plugin loads.
pub async fn reset_restore_limits(db: &PgPool, plugin_id: &str) -> Result<(), SnapshotError> {
    let Some(names) = plugin_storage::get(db, plugin_id).await? else {
        return Ok(());
    };
    let raised = sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM pg_db_role_setting s JOIN pg_roles r ON r.oid = s.setrole
            JOIN pg_database d ON d.oid = s.setdatabase
            WHERE r.rolname = $1 AND d.datname = current_database()
        ) AS "raised!"
        "#,
        names.role_name
    )
    .fetch_one(db)
    .await?;
    if !raised {
        return Ok(());
    }
    if !safe_name(&names.role_name) {
        return Err(SnapshotError::BadPluginId(plugin_id.to_owned()));
    }
    let mut conn = db.acquire().await?;
    let database = quoted_database(&mut conn).await?;
    ddl(
        &mut conn,
        format!(
            "ALTER ROLE \"{}\" IN DATABASE {database} RESET temp_file_limit",
            names.role_name
        ),
    )
    .await?;
    tracing::warn!(
        plugin = plugin_id,
        "removed the temp file cap a cut-short rollback raised"
    );
    Ok(())
}

/// `current_database()` as a quoted identifier.
async fn quoted_database(tx: &mut PgConnection) -> Result<String, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT quote_ident(current_database()) AS "name!""#)
        .fetch_one(tx)
        .await
}

/// Where a plugin's schema waits while its restore runs: a name no plugin
/// schema can have (theirs start `plugin_`), the same for every attempt so
/// one cut short is found.
pub fn set_aside_schema(plugin_id: &str) -> String {
    format!(
        "rollback_{}",
        &hex(&Sha256::digest(plugin_id.as_bytes()))[..16]
    )
}

async fn ddl(tx: &mut PgConnection, sql: String) -> Result<(), sqlx::Error> {
    // Names in `sql` are Tether's own, checked by `safe_name`.
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(tx).await?;
    Ok(())
}

async fn schema_exists(tx: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1) AS "exists!""#,
        name
    )
    .fetch_one(tx)
    .await
}

fn audit_details(snapshot: &Snapshot, discarded_audit_entries: Option<i64>) -> serde_json::Value {
    let mut details = serde_json::json!({
        "snapshot": snapshot.name,
        "taken_at": snapshot.header.taken_at,
        "reason": snapshot.header.reason,
    });
    if let (Some(n), Some(object)) = (discarded_audit_entries, details.as_object_mut()) {
        object.insert("discarded_audit_entries".to_owned(), n.into());
    }
    details
}

/// The SQL around pg_restore's script for core: drop the schema first;
/// end every session (a rollback mustn't bring back sessions ended since),
/// put the migration records back, audit it, and commit, last. Values come
/// from the header, which the key authenticates, and are quoted as
/// literals all the same. Only columns core has had since the audit log
/// began are named: the snapshot may be older than this version.
fn core_scripts(
    snapshot: &Snapshot,
    actor: Actor,
    discarded_audit_entries: i64,
) -> Result<(String, String), SnapshotError> {
    let prologue = "BEGIN;\n\
                    SET LOCAL lock_timeout = '30s';\n\
                    DROP SCHEMA IF EXISTS \"core\" CASCADE;\n"
        .to_owned();
    // pg_restore's script clears search_path, so every name is qualified.
    let mut epilogue = String::from("\nSET LOCAL standard_conforming_strings = on;\n");
    epilogue.push_str("DELETE FROM core.sessions;\n");
    epilogue.push_str("DELETE FROM public._sqlx_migrations;\n");
    for m in &snapshot.header.core_migrations {
        epilogue.push_str(&format!(
            "INSERT INTO public._sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES ({}, {}, {}, {}, {}, {});\n",
            m.version,
            text_literal(&m.description)?,
            time_literal(m.installed_on),
            m.success,
            bytea_literal(&m.checksum)?,
            m.execution_time,
        ));
    }
    let (account, name) = match actor {
        Actor::Account(a) => (a.0.to_string(), "NULL".to_owned()),
        Actor::System => ("NULL".to_owned(), "NULL".to_owned()),
        Actor::Cli => ("NULL".to_owned(), text_literal("cli")?),
    };
    let details = serde_json::to_string(&audit_details(snapshot, Some(discarded_audit_entries)))?;
    epilogue.push_str(&format!(
        "INSERT INTO core.audit_log (actor_account_id, actor_name, action, target, details) \
         VALUES ({account}, COALESCE({name}, (SELECT c.name FROM core.accounts a \
         JOIN core.characters c ON c.id = a.main_character_id WHERE a.id = {account})), \
         {}, 'core', {}::jsonb);\n",
        text_literal(RESTORED)?,
        text_literal(&details)?,
    ));
    epilogue.push_str("COMMIT;\n");
    Ok((prologue, epilogue))
}

/// A standard-conforming string literal.
fn text_literal(text: &str) -> Result<String, SnapshotError> {
    if text.contains('\0') {
        return Err(SnapshotError::Corrupt("a text value has a NUL".to_owned()));
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

fn bytea_literal(hex: &str) -> Result<String, SnapshotError> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SnapshotError::Corrupt("a checksum isn't hex".to_owned()));
    }
    Ok(format!("'\\x{hex}'::bytea"))
}

fn time_literal(time: DateTime<Utc>) -> String {
    format!(
        "'{}'::timestamptz",
        time.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(hex: &str) -> Result<Vec<u8>, SnapshotError> {
    let bad = || SnapshotError::Corrupt("a checksum isn't hex".to_owned());
    if !hex.len().is_multiple_of(2) {
        return Err(bad());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(bad)
        })
        .collect()
}

fn random_hex() -> Result<String, SnapshotError> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|_| CryptoError::Random)?;
    Ok(hex(&bytes))
}

fn failure(status: std::process::ExitStatus, stderr: &str) -> String {
    if stderr.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {stderr}")
    }
}

/// A `.partial` file, removed unless kept (so a failed or cancelled
/// snapshot leaves nothing behind).
struct Partial(Option<PathBuf>);

impl Partial {
    fn path(&self) -> &Path {
        self.0.as_deref().unwrap_or(Path::new(""))
    }

    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn create_private_dir(dir: &Path) -> Result<(), SnapshotError> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder
        .create(dir)
        .await
        .map_err(|e| SnapshotError::io(format!("creating {}", dir.display()), e))
}

async fn create_private_file(path: &Path) -> Result<tokio::fs::File, SnapshotError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .await
        .map_err(|e| SnapshotError::io(format!("creating {}", path.display()), e))
}

/// So a rename survives a crash. Best effort: not every platform can.
async fn sync_dir(dir: &Path) {
    if let Ok(dir) = tokio::fs::File::open(dir).await {
        let _ = dir.sync_all().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: Kind, taken_at: DateTime<Utc>) -> Header {
        Header {
            kind,
            reason: Reason::BeforeMigrations,
            taken_at,
            tether_version: "0.1.0".to_owned(),
            postgres_version: 160015,
            timescaledb: Some("2.30.1".to_owned()),
            plugin_role: None,
            core_migrations: vec![CoreMigration {
                version: 1,
                description: "it's core".to_owned(),
                installed_on: taken_at,
                success: true,
                checksum: "00ff".to_owned(),
                execution_time: 5,
            }],
            plugin_migrations: vec![PluginMigration {
                version: 1,
                name: "o'brien".to_owned(),
                sha256: "abcd".to_owned(),
                applied_at: taken_at,
            }],
        }
    }

    #[test]
    fn plugin_ids_are_checked() {
        assert!(Kind::Plugin("moon-mining".into()).check().is_ok());
        for bad in ["", "Moon", "a\"b", "a b", "x;drop", &"a".repeat(57)] {
            assert!(Kind::Plugin(bad.into()).check().is_err(), "{bad:?}");
        }
    }

    fn snapshot(header: Header) -> Snapshot {
        Snapshot {
            path: PathBuf::from("x.tsnap"),
            name: "core-x.tsnap".to_owned(),
            bytes: 0,
            header,
        }
    }

    #[test]
    fn core_scripts_quote_values_and_end_sessions() {
        let now = Utc::now();
        let (prologue, epilogue) =
            core_scripts(&snapshot(header(Kind::Core, now)), Actor::Cli, 3).unwrap();
        assert!(prologue.starts_with("BEGIN;"));
        assert!(prologue.contains("DROP SCHEMA IF EXISTS \"core\" CASCADE;"));
        assert!(epilogue.contains("DELETE FROM core.sessions;"));
        assert!(epilogue.contains("DELETE FROM public._sqlx_migrations;"));
        assert!(epilogue.contains("'it''s core'"));
        assert!(epilogue.contains("'\\x00ff'::bytea"));
        assert!(epilogue.contains("'snapshot.restored'"));
        assert!(epilogue.contains("\"discarded_audit_entries\":3"));
        assert!(!epilogue.contains("plugin_migrations"));
        assert!(epilogue.ends_with("COMMIT;\n"));

        let mut bad = header(Kind::Core, now);
        bad.core_migrations[0].checksum = "'; DROP".to_owned();
        assert!(core_scripts(&snapshot(bad), Actor::Cli, 0).is_err());
        let mut bad = header(Kind::Core, now);
        bad.core_migrations[0].description = "a\0b".to_owned();
        assert!(core_scripts(&snapshot(bad), Actor::Cli, 0).is_err());
    }

    #[test]
    fn restore_temp_limits_stay_finite() {
        assert_eq!(restore_temp_limit_kib(0), 1024 * 1024);
        assert_eq!(restore_temp_limit_kib(1024 * 1024 * 1024), 8 * 1024 * 1024);
        assert_eq!(restore_temp_limit_kib(u64::MAX), 64 * 1024 * 1024);
    }

    #[test]
    fn set_aside_names_are_fixed_per_plugin_and_never_a_plugins() {
        let a = set_aside_schema("moon");
        assert_eq!(a, set_aside_schema("moon"));
        assert_ne!(a, set_aside_schema("moon2"));
        assert!(a.starts_with("rollback_") && safe_name(&a));
        assert_eq!(unhex("00ff").unwrap(), vec![0, 255]);
        assert!(unhex("0g").is_err() && unhex("0").is_err());
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tether-snapshots-{}", random_hex().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_key() -> EncryptionKey {
        EncryptionKey::from_hex(&Secret::new("ab".repeat(32)))
            .unwrap()
            .derive(KEY_LABEL)
            .unwrap()
    }

    /// A snapshot file without pg_dump: a header and a small body.
    async fn fake(dir: &Path, header: &Header) {
        let json = serde_json::to_vec(header).unwrap();
        let name = format!(
            "{}-{}-{}{EXTENSION}",
            header.kind.file_prefix(),
            header.taken_at.format("%Y%m%dT%H%M%S%.fZ"),
            random_hex().unwrap()
        );
        let mut out = Vec::new();
        file::seal(&test_key(), &json, &mut &b"body"[..], &mut out)
            .await
            .unwrap();
        tokio::fs::write(dir.join(name), out).await.unwrap();
    }

    #[tokio::test]
    async fn keeps_the_newest_per_kind_and_lists_newest_first() {
        let root = temp_dir();
        let dir = root.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let start = Utc::now() - chrono::Duration::days(1);
        for i in 0..7 {
            fake(
                &dir,
                &header(Kind::Core, start + chrono::Duration::minutes(i)),
            )
            .await;
        }
        let plugin = Kind::Plugin("moon".into());
        fake(&dir, &header(plugin.clone(), start)).await;
        tokio::fs::write(dir.join("notes.txt"), "not a snapshot")
            .await
            .unwrap();
        tokio::fs::write(dir.join("junk.tsnap"), "not a snapshot")
            .await
            .unwrap();

        prune(&dir, &Kind::Core, KEEP_SNAPSHOTS).await;

        let all = list(&root).await.unwrap();
        let core: Vec<_> = all.iter().filter(|s| s.header.kind == Kind::Core).collect();
        assert_eq!(core.len(), KEEP_SNAPSHOTS);
        assert_eq!(
            core[0].header.taken_at,
            start + chrono::Duration::minutes(6)
        );
        assert_eq!(
            core[4].header.taken_at,
            start + chrono::Duration::minutes(2)
        );
        assert_eq!(all.iter().filter(|s| s.header.kind == plugin).count(), 1);
        assert!(dir.join("notes.txt").exists(), "only snapshots are pruned");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_missing_directory_lists_nothing() {
        let root = std::env::temp_dir().join("tether-snapshots-does-not-exist");
        assert!(list(&root).await.unwrap().is_empty());
    }
}
