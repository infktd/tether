//! Plugins (F15): trusting package signers, and the lifecycle from upload
//! to uninstall.
//!
//! A package's publisher key is pinned on first install. After that, only
//! a rotation the pinned key signed moves the pin, or an admin re-pinning
//! it by hand after typing the plugin's id to confirm (for a publisher who
//! lost their key). Every change is audited in the same transaction.
//!
//! Lifecycle: an admin uploads a package; once its signature, the pinned
//! key and its component all check out it waits for approval, showing
//! everything it asks for. The apps bundled into Tether's image
//! ([`crate::bundled`]) skip the upload: they're unsigned, as trusted as
//! the binary, and approved from the Apps page after the same review. Approving checks it all again inside the
//! install's transaction, records the key, installs and activates it, with
//! no restart. Enabling, disabling and uninstalling load and unload it.
//! [`Plugins`] holds what is running; lifecycle changes take its lock, so
//! the database and what runs can't disagree.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::json;
use sqlx::Connection;
use sqlx::PgConnection;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tether_core::crypto::EncryptionKey;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::plugin_keys::{self, PinnedBy};
use tether_db::plugin_storage::password_secret;
use tether_db::plugins as db;
use tether_db::{plugin_jobs, plugin_storage, secrets};
use tether_plugins::host::{Host, LoadedPlugin};
use tether_plugins::manifest;
use tether_plugins::package::{self, Package, PackageError, Trust, Verified};
use tether_plugins::storage::Storage;

use crate::error::AppError;

const CHANGED_MEANWHILE: &str =
    "This app's publisher key changed while the package was being checked. Upload it again.";

/// Uploads waiting for approval, across all plugins.
pub const MAX_PENDING_UPLOADS: i64 = 10;
const TOO_MANY_UPLOADS: &str =
    "Too many uploads are waiting for approval. Approve or discard some first.";
/// Uploads nobody approves or discards are dropped after this.
pub const UPLOAD_HOURS: i32 = 24;

/// The audit target for a plugin.
fn target(plugin_id: &str) -> String {
    format!("plugin:{plugin_id}")
}

/// Records the key a verified package was trusted with, in the install's
/// transaction. Check the package against the pin read by
/// [`plugin_keys::get_locked`] for [`Unverified::id`] in that same
/// transaction; each case here also re-checks the pin atomically, so a
/// stale check fails rather than overwrites.
///
/// [`Unverified::id`]: tether_plugins::package::Unverified::id
pub async fn record_trust(
    tx: &mut PgConnection,
    actor: Actor,
    verified: &Verified,
) -> Result<(), AppError> {
    let plugin = verified.package().manifest.plugin.id.as_str();
    let key = verified.key();
    match verified.trust() {
        Trust::Pinned => {
            if plugin_keys::get_locked(tx, plugin).await?.as_deref() != Some(key) {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
        }
        Trust::FirstInstall => {
            if !plugin_keys::pin_first(&mut *tx, plugin, key).await? {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
            audit::record(
                &mut *tx,
                actor,
                "plugin.key_pinned",
                Some(&target(plugin)),
                json!({ "key": key }),
            )
            .await?;
        }
        Trust::Rotated { from } => {
            if !plugin_keys::replace(&mut *tx, plugin, from, key, PinnedBy::Rotation).await? {
                return Err(AppError::bad_request(CHANGED_MEANWHILE));
            }
            audit::record(
                &mut *tx,
                actor,
                "plugin.key_rotated",
                Some(&target(plugin)),
                json!({ "old": from, "new": key }),
            )
            .await?;
        }
    }
    Ok(())
}

/// Replaces a plugin's pinned key by hand, for when a publisher lost the
/// old key and can't sign a rotation. `confirmation` must be the plugin's
/// id, typed by the admin, and `expected_old` the key the admin was shown,
/// so a pin that changed in the meantime isn't overwritten unseen.
pub async fn repin_key(
    db: &PgPool,
    actor: Actor,
    plugin_id: &str,
    expected_old: &str,
    new_key: &str,
    confirmation: &str,
) -> Result<(), AppError> {
    if confirmation.trim() != plugin_id {
        return Err(AppError::bad_request(
            "Type the app's id exactly to confirm replacing its key.",
        ));
    }
    let new_key = new_key.trim();
    manifest::check_key(new_key)
        .map_err(|_| AppError::bad_request("That isn't a minisign public key."))?;
    if new_key == expected_old {
        return Err(AppError::bad_request("That key is already pinned."));
    }
    let mut tx = db.begin().await?;
    match plugin_keys::get_locked(&mut tx, plugin_id).await? {
        None => return Err(AppError::not_found("No key is pinned for that app.")),
        Some(current) if current != expected_old => {
            return Err(AppError::bad_request(
                "The pinned key changed since you looked. Check it again.",
            ));
        }
        Some(_) => {}
    }
    if !plugin_keys::replace(&mut *tx, plugin_id, expected_old, new_key, PinnedBy::Repin).await? {
        return Err(AppError::bad_request(
            "The pinned key changed since you looked. Check it again.",
        ));
    }
    audit::record(
        &mut *tx,
        actor,
        "plugin.key_repinned",
        Some(&target(plugin_id)),
        json!({ "old": expected_old, "new": new_key }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Whether a plugin is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    /// Enabled, but it couldn't be loaded; why, for admins.
    Failed(String),
    Stopped,
}

enum Slot {
    Running(Running),
    Failed(String),
}

/// A running plugin and the manifest it was approved with.
#[derive(Clone)]
pub struct Running {
    pub plugin: LoadedPlugin,
    pub manifest: Arc<tether_plugins::manifest::Manifest>,
}

/// A Dashboard widget of a running plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetItem {
    pub plugin_id: String,
    /// Its place in the manifest's `[[widgets]]`.
    pub index: usize,
    pub title: String,
    /// What opening its page needs: a plugin permission, or `None` for
    /// admins.
    pub permission: Option<String>,
}

/// A sidebar link to a running plugin's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
    pub plugin_id: String,
    pub label: String,
    /// `/plugins/<id>/<path>`.
    pub href: String,
    /// What opening it needs: a plugin permission, or `None` for admins.
    pub permission: Option<String>,
    /// Its default sidebar section (`[[navigation]] section`).
    pub section: &'static str,
}

/// The plugins running in this process.
pub struct Plugins {
    host: Host,
    /// Opens plugins' database passwords.
    key: EncryptionKey,
    snapshots: Option<Arc<tether_snapshots::Snapshots>>,
    slots: RwLock<BTreeMap<String, Slot>>,
    /// Held across every lifecycle change (database, then load or unload),
    /// so concurrent changes can't leave a plugin running that the
    /// database says is off, or the reverse.
    lifecycle: tokio::sync::Mutex<()>,
    /// One upload is read and checked at a time: each can hold a 40 MiB
    /// package, its unpacked contents and a compile.
    uploads: tokio::sync::Semaphore,
    /// Plugins' HTTP clients and rate limit (held here for tests to route).
    #[cfg_attr(not(feature = "plugin-http-test"), allow(dead_code))]
    http: Arc<crate::plugin_http::Http>,
    /// Installs and update checks from GitHub; `None` where it's not set
    /// up (most tests).
    github: Option<Arc<crate::plugin_github::GitHub>>,
    bundled: Arc<crate::bundled::Bundled>,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugins").finish_non_exhaustive()
    }
}

impl Plugins {
    /// `deps` are what plugins reach through the host: the job queue (in
    /// `deps.db`), ESI and Discord.
    pub fn new(host: Host, deps: crate::plugin_services::Deps) -> Arc<Self> {
        // The instance's URL stays out of the User-Agent: approved hosts
        // don't learn the domain, though zKillboard asks for contact
        // details (Jay's call). An empty URL leaves it out.
        let http = Arc::new(crate::plugin_http::Http::new(""));
        Arc::new_cyclic(|plugins| {
            let services = crate::plugin_services::PluginServices::new(
                deps.clone(),
                plugins.clone(),
                http.clone(),
            );
            Self {
                host: host
                    .with_jobs(crate::plugin_jobs::PluginQueue::new(deps.db.clone()))
                    .with_services(services),
                key: deps.key.clone(),
                snapshots: deps.snapshots.clone(),
                slots: RwLock::default(),
                lifecycle: tokio::sync::Mutex::new(()),
                uploads: tokio::sync::Semaphore::new(1),
                http,
                github: deps.github.clone(),
                bundled: deps.bundled.clone(),
            }
        })
    }

    /// Tests only: serves every plugin's approved hosts from a plain-HTTP
    /// stand-in at `host_port`. Compiled out of release builds.
    #[cfg(feature = "plugin-http-test")]
    pub fn route_http_to(&self, host_port: &str) {
        self.http.route_to(host_port);
    }

    pub fn github(&self) -> Option<&crate::plugin_github::GitHub> {
        self.github.as_deref()
    }

    /// The apps bundled into this Tether's image.
    pub fn bundled(&self) -> &crate::bundled::Bundled {
        &self.bundled
    }

    /// Whether snapshots are taken before plugin migrations here.
    pub fn snapshots_on(&self) -> bool {
        self.snapshots.is_some()
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The right to read and check an upload now; `None` while another is
    /// being checked.
    pub fn upload_permit(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.uploads.try_acquire().ok()
    }

    fn slots(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Slot>> {
        // A panic while holding the lock can only leave the map mid-insert
        // of one entry; the map itself is still valid.
        self.slots.write().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self, id: &str) -> Status {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        match slots.get(id) {
            Some(Slot::Running(_)) => Status::Running,
            Some(Slot::Failed(why)) => Status::Failed(why.clone()),
            None => Status::Stopped,
        }
    }

    /// The running plugin, to call.
    pub fn get(&self, id: &str) -> Option<LoadedPlugin> {
        self.running(id).map(|r| r.plugin)
    }

    /// The running plugin with its manifest.
    pub fn running(&self, id: &str) -> Option<Running> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        match slots.get(id) {
            Some(Slot::Running(running)) => Some(running.clone()),
            _ => None,
        }
    }

    /// Every running plugin, by name.
    pub fn all_running(&self) -> Vec<Running> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let mut running: Vec<Running> = slots
            .values()
            .filter_map(|slot| match slot {
                Slot::Running(running) => Some(running.clone()),
                Slot::Failed(_) => None,
            })
            .collect();
        running.sort_by(|a, b| a.manifest.plugin.name.cmp(&b.manifest.plugin.name));
        running
    }

    /// Every running plugin's sidebar entries, by plugin name.
    pub fn navigation(&self) -> Vec<NavItem> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        let mut running: Vec<&Running> = slots
            .values()
            .filter_map(|slot| match slot {
                Slot::Running(running) => Some(running),
                Slot::Failed(_) => None,
            })
            .collect();
        running.sort_by(|a, b| a.manifest.plugin.name.cmp(&b.manifest.plugin.name));
        running
            .into_iter()
            .flat_map(|r| {
                let id = r.manifest.plugin.id.clone();
                r.manifest.navigation.iter().map(move |entry| NavItem {
                    href: page_href(&id, &entry.path),
                    permission: r.manifest.page_permission(&entry.path),
                    section: entry.section(),
                    label: entry.label.clone(),
                    plugin_id: id.clone(),
                })
            })
            .collect()
    }

    /// Every running plugin's Dashboard widgets, by plugin name.
    pub fn widgets(&self) -> Vec<WidgetItem> {
        self.all_running()
            .into_iter()
            .flat_map(|r| {
                let id = r.manifest.plugin.id.clone();
                r.manifest
                    .widgets
                    .iter()
                    .enumerate()
                    .map(|(index, widget)| WidgetItem {
                        plugin_id: id.clone(),
                        index,
                        title: widget.title.clone(),
                        permission: r.manifest.page_permission(&widget.path),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Loads an installed plugin from its stored package, but only the
    /// package that was approved (its hash, also in the audit log) signed
    /// with the key pinned now, or bundled into Tether's image. After a
    /// re-pin, a package signed with the old key stops loading.
    async fn activate(&self, db: &PgPool, installed: &db::Installed) {
        let slot = match self.load(db, installed).await {
            Ok(running) => {
                tracing::info!(
                    plugin = installed.id,
                    version = installed.version,
                    "plugin loaded"
                );
                if let Some(snapshots) = &self.snapshots {
                    let kind = tether_snapshots::Kind::Plugin(installed.id.clone());
                    snapshots.mark_running(&kind).await;
                }
                Slot::Running(running)
            }
            Err(why) => {
                tracing::error!(plugin = installed.id, error = %why, "plugin failed to load");
                Slot::Failed(why)
            }
        };
        self.slots().insert(installed.id.clone(), slot);
    }

    async fn load(&self, db: &PgPool, installed: &db::Installed) -> Result<Running, String> {
        if sha256(&installed.package) != installed.package_sha256 {
            return Err("the stored package isn't the one that was approved".to_owned());
        }
        // A rollback cut short left its data set aside; running against
        // the half-restored schema would split its data in two.
        match tether_snapshots::set_aside_exists(db, &installed.id).await {
            Ok(false) => {}
            Ok(true) => {
                return Err(format!(
                    "a rollback of its data was cut short: run `tether rollback --plugin {}` \
                     again (with Tether stopped) to finish it",
                    installed.id
                ));
            }
            Err(e) => {
                tracing::error!(plugin = installed.id, error = %e, "checking for a cut-short rollback");
                return Err("its storage couldn't be checked".to_owned());
            }
        }
        if let Err(e) = tether_snapshots::reset_restore_limits(db, &installed.id).await {
            tracing::error!(plugin = installed.id, error = %e, "resetting a rollback's temp file cap");
            return Err("its database limits couldn't be checked".to_owned());
        }
        let package = match installed.origin {
            // Shipped in Tether's image, so as trusted as the binary: the
            // package approved (its hash, checked above), with no
            // signature or key.
            db::Origin::Bundled => package::read(&installed.package)
                .map_err(|e| format!("the stored package doesn't check out: {e}"))?
                .into_bundled(),
            db::Origin::Signed => {
                let signature = installed
                    .signature
                    .as_deref()
                    .ok_or("the stored package has no signature")?;
                let pinned = plugin_keys::get(db, &installed.id)
                    .await
                    .map_err(|e| {
                        tracing::error!(plugin = installed.id, error = %e, "reading a pinned key");
                        "its pinned key couldn't be read".to_owned()
                    })?
                    .ok_or("no publisher key is pinned for it")?;
                let verified = package::read(&installed.package)
                    .and_then(|p| p.verify(signature, Some(&pinned)))
                    .map_err(|e| format!("the stored package doesn't check out: {e}"))?;
                if *verified.trust() != Trust::Pinned {
                    return Err(
                        "the stored package isn't signed with the pinned key; install a version \
                         signed with it"
                            .to_owned(),
                    );
                }
                verified.into_package()
            }
        };
        if package.manifest.plugin.id != installed.id {
            return Err("the stored package is for another plugin".to_owned());
        }
        let storage = match plugin_storage::get(db, &installed.id).await {
            Ok(Some(names)) => Some(self.storage(db, &installed.id, &names, &package).await?),
            Ok(None) if package.manifest.capabilities.storage => {
                return Err("its database storage is missing".to_owned());
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!(plugin = installed.id, error = %e, "reading plugin storage");
                return Err("its database storage couldn't be read".to_owned());
            }
        };
        let schedules = crate::plugin_jobs::declared(&package.manifest);
        let manifest = Arc::new(package.manifest);
        let loaded = self
            .host
            .load(&installed.id, package.component, storage)
            .await
            .map_err(|e| e.to_string())?;
        // Installs from before scope compliance didn't record the user
        // scopes Member requires; catch them up.
        crate::compliance::sync_plugin_scopes(db, &installed.id, &manifest.capabilities.esi.user)
            .await
            .map_err(|e| {
                tracing::error!(plugin = installed.id, error = %e, "recording plugin scopes");
                "its scopes couldn't be recorded".to_owned()
            })?;
        // Its schedules run only while it does.
        plugin_jobs::sync_schedules(db, &installed.id, &schedules)
            .await
            .map_err(|e| {
                tracing::error!(plugin = installed.id, error = %e, "syncing plugin schedules");
                "its schedules couldn't be set up".to_owned()
            })?;
        Ok(Running {
            plugin: loaded,
            manifest,
        })
    }

    /// The plugin's pool, connected as its role, once its pending
    /// migrations have run.
    async fn storage(
        &self,
        db: &PgPool,
        id: &str,
        names: &plugin_storage::Names,
        package: &Package,
    ) -> Result<Storage, String> {
        let sealed = secrets::get(db, &password_secret(id))
            .await
            .map_err(|e| {
                tracing::error!(plugin = id, error = %e, "reading a plugin's database password");
                "its database password couldn't be read".to_owned()
            })?
            .ok_or("its database password is missing")?;
        let password = self
            .key
            .open(&sealed, &secrets::context(&password_secret(id)))
            .map_err(|_| "its database password can't be opened with this instance's key")?;
        let options = db
            .connect_options()
            .as_ref()
            .clone()
            .username(&names.role_name)
            .password(password.expose())
            .application_name(&format!("tether plugin {id}"));
        migrate(
            db,
            &options,
            id,
            &package.migrations,
            self.snapshots.as_deref(),
        )
        .await?;
        let pool = PgPoolOptions::new()
            .max_connections(POOL_CONNECTIONS)
            .min_connections(0)
            .acquire_timeout(tether_plugins::storage::ACQUIRE_TIMEOUT)
            .idle_timeout(Some(std::time::Duration::from_secs(60)))
            // Session state a statement can leave behind: open cursors,
            // listens and any other settings. (Limits are put back before
            // every statement too.)
            .after_release(|conn, _| {
                Box::pin(async move {
                    sqlx::raw_sql("CLOSE ALL; UNLISTEN *; RESET ALL")
                        .execute(conn)
                        .await?;
                    Ok(true)
                })
            })
            .connect_lazy_with(options);
        Ok(Storage::new(id, &names.schema_name, pool))
    }

    /// Stops a plugin; closes its database pool.
    async fn deactivate(&self, id: &str) {
        let removed = self.slots().remove(id);
        if let Some(slot) = removed {
            if let Slot::Running(running) = slot
                && let Some(storage) = running.plugin.storage()
            {
                storage.pool().close().await;
            }
            tracing::info!(plugin = id, "plugin unloaded");
        }
    }

    /// Loads every enabled plugin. Run once at startup; one plugin failing
    /// doesn't stop the others.
    pub async fn start(&self, db: &PgPool) -> Result<(), sqlx::Error> {
        let _lifecycle = self.lifecycle.lock().await;
        for id in db::enabled_ids(db).await? {
            if let Some(installed) = db::get(db, &id).await? {
                self.activate(db, &installed).await;
            }
        }
        Ok(())
    }
}

/// Where a plugin page lives: `/plugins/<id>` or `/plugins/<id>/<path>`.
/// `path` is a checked link path, `id` a checked plugin id.
pub fn page_href(id: &str, path: &str) -> String {
    if path.is_empty() {
        format!("/plugins/{id}")
    } else {
        format!("/plugins/{id}/{path}")
    }
}

/// Connections in a plugin's pool: as many as it may have calls running.
const POOL_CONNECTIONS: u32 = tether_plugins::CALLS_PER_PLUGIN as u32;
/// Plugins with storage, at most: their pools together stay well inside
/// Postgres's connection limit, leaving core its own.
pub const MAX_STORAGE_PLUGINS: i64 = 20;
/// Its role's connection limit: the pool, a migration, and one to spare.
const ROLE_CONNECTIONS: u32 = POOL_CONNECTIONS + 2;
/// How long one plugin migration may run before its session is ended.
pub const MIGRATION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs a plugin's pending migrations over its own role's connection, one
/// transaction each, recording each with its checksum. A migration already
/// applied must be unchanged. The host ends the session of one that runs
/// past [`MIGRATION_DEADLINE`] (a plugin's SQL can lift its own statement
/// timeout). When the plugin already has data (an upgrade), a snapshot of
/// its schema comes first, and nothing runs without one (N14).
async fn migrate(
    db: &PgPool,
    options: &PgConnectOptions,
    plugin: &str,
    migrations: &[package::Migration],
    snapshots: Option<&tether_snapshots::Snapshots>,
) -> Result<(), String> {
    let failed = |e: sqlx::Error| {
        tracing::error!(plugin, error = %e, "plugin migrations");
        "its migrations couldn't be checked".to_owned()
    };
    let applied = plugin_storage::applied(db, plugin).await.map_err(failed)?;
    check_applied(&applied, migrations)?;
    let pending = &migrations[applied.len().min(migrations.len())..];
    if pending.is_empty() {
        return Ok(());
    }
    // A fresh install has nothing to keep.
    if !applied.is_empty() {
        match snapshots {
            Some(snapshots) => {
                snapshots
                    .before_plugin_migrations(db, plugin)
                    .await
                    .map_err(|e| {
                        tracing::error!(plugin, error = %e, "snapshot before plugin migrations");
                        format!(
                            "its new migrations didn't run, because a snapshot of its data \
                             couldn't be taken first ({})",
                            e.brief()
                        )
                    })?;
            }
            None => tracing::warn!(plugin, "snapshots are off; migrating without one"),
        }
    }
    let mut conn = PgConnection::connect_with(options).await.map_err(|e| {
        tracing::error!(plugin, error = %e, "connecting as a plugin role");
        "it couldn't connect to its database".to_owned()
    })?;
    // Qualified: on the plugin's own connection, an unqualified name could
    // resolve to a function of its own, and this pid is ended as Tether.
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .map_err(failed)?;
    for m in pending {
        let label = format!("{:04}_{}", m.version, m.name);
        let run = async {
            let mut tx = conn.begin().await?;
            sqlx::raw_sql("SET LOCAL statement_timeout = '60s'")
                .execute(&mut *tx)
                .await?;
            // The plugin's own SQL, as its role: Postgres confines it.
            sqlx::raw_sql(sqlx::AssertSqlSafe(m.sql.clone()))
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        };
        match tokio::time::timeout(MIGRATION_DEADLINE, run).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let why = match &e {
                    sqlx::Error::Database(db) => tether_plugins::host::printable(db.message(), 500),
                    other => other.to_string(),
                };
                return Err(format!("migration {label} failed: {why}"));
            }
            Err(_) => {
                if let Err(e) = plugin_storage::terminate(db, pid).await {
                    tracing::error!(plugin, error = %e, "ending a migration that ran too long");
                }
                return Err(format!(
                    "migration {label} ran longer than {} seconds",
                    MIGRATION_DEADLINE.as_secs()
                ));
            }
        }
        let version = i32::try_from(m.version).map_err(|_| "a migration number is too big")?;
        plugin_storage::record_migration(db, plugin, version, &m.name, &sha256(m.sql.as_bytes()))
            .await
            .map_err(failed)?;
        tracing::info!(plugin, migration = label, "plugin migration applied");
    }
    let _ = conn.close().await;
    Ok(())
}

/// Every applied migration (`(version, sha256)`) must be in the package,
/// unchanged.
fn check_applied(
    applied: &[(i32, Vec<u8>)],
    migrations: &[package::Migration],
) -> Result<(), String> {
    for (version, sha) in applied {
        let found = migrations
            .iter()
            .find(|m| i64::from(m.version) == i64::from(*version));
        match found {
            None => {
                return Err(format!(
                    "migration {version:04} was applied, but this package doesn't have it"
                ));
            }
            Some(m) if sha256(m.sql.as_bytes()) != *sha => {
                return Err(format!(
                    "migration {version:04}_{} changed since it was applied",
                    m.name
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

pub(crate) fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Runs a lifecycle change to the end even if the request that started it
/// goes away, so the database and what runs can't be left disagreeing.
async fn detached<T: Send + 'static>(
    work: impl std::future::Future<Output = Result<T, AppError>> + Send + 'static,
) -> Result<T, AppError> {
    tokio::spawn(work).await.map_err(AppError::internal)?
}

/// Plain words for a package problem. Text quoted from the package is
/// bounded and escaped by the package reader; templates escape the rest.
fn package_error(err: PackageError) -> AppError {
    match err {
        PackageError::KeyChanged { pinned, new } => AppError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "This package is signed with a different key ({new}) than the one pinned for this \
                 plugin ({pinned}), and has no rotation statement from the pinned key. If the \
                 publisher confirms they lost their old key, re-pin it under Pinned keys on the \
                 Plugins page, then upload again."
            ),
        ),
        other => AppError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            format!("This package can't be installed: {other}"),
        ),
    }
}

/// What a package can't ask for.
fn unsupported(package: &Package) -> Option<&'static str> {
    // Member requires a plugin's user scopes of every character: only ones
    // a catalogue character endpoint can use.
    if package
        .manifest
        .capabilities
        .esi
        .user
        .iter()
        .any(|s| !crate::compliance::is_catalogue_character_scope(s))
    {
        return Some(
            "Its user scopes (capabilities.esi.user) must be ones a character endpoint Tether \
             offers plugins uses; see the SDK's AGENTS.md.",
        );
    }
    if crate::plugin_http::declares_core_host(&package.manifest) {
        return Some(
            "It asks to call one of Tether's own destinations (ESI, EVE SSO, Discord or \
             GitHub) over HTTP. Apps reach ESI and Discord through Tether instead.",
        );
    }
    if !package.manifest.capabilities.storage && !package.migrations.is_empty() {
        return Some(
            "This package has database migrations but doesn't ask for storage \
             (capabilities.storage).",
        );
    }
    None
}

/// A bundled app's id, in a package from anywhere else.
fn reserved(id: &str) -> AppError {
    AppError::bad_request(format!(
        "{id} comes with Tether: it's installed and updated from \"Included with Tether\" on \
         the Apps page, never from a file or GitHub."
    ))
}

fn trust_label(trust: &Trust) -> &'static str {
    match trust {
        Trust::FirstInstall => "first_install",
        Trust::Pinned => "pinned",
        Trust::Rotated { .. } => "rotated",
    }
}

/// Where a package was fetched from.
#[derive(Debug, Clone)]
pub struct Source {
    pub repo: crate::plugin_github::Repo,
    /// The app and version its release asset is named for: the package
    /// must be that.
    pub plugin_id: String,
    pub version: String,
}

/// An uploaded package, checked (signature, pinned key, component) and
/// waiting for approval. Rejections are audited too, with the reason.
/// Development builds only (`dev-upload`); GitHub installs go through
/// [`upload_from`].
#[cfg(feature = "dev-upload")]
pub async fn upload(
    state: &crate::AppState,
    actor: AccountId,
    bytes: Vec<u8>,
    signature: String,
) -> Result<i64, AppError> {
    upload_from(state, actor, bytes, signature, None).await
}

/// [`upload`], of a package fetched from `source`.
pub async fn upload_from(
    state: &crate::AppState,
    actor: AccountId,
    bytes: Vec<u8>,
    signature: String,
    source: Option<Source>,
) -> Result<i64, AppError> {
    let expected = source
        .as_ref()
        .map(|s| (s.plugin_id.as_str(), s.version.as_str()));
    let result = check_upload(state, &bytes, &signature, expected).await;
    let (plugin_id, version, trust) = match result {
        Ok(checked) => checked,
        Err((plugin, err)) => {
            return Err(reject(state, actor, plugin.as_deref(), bytes.len(), err).await);
        }
    };
    let mut tx = state.db.begin().await?;
    // Checked again here: uploads are one at a time in this process, so
    // nothing else inserts between this count and the insert.
    if db::count_uploads(&mut *tx).await? >= MAX_PENDING_UPLOADS {
        drop(tx);
        let err = AppError::bad_request(TOO_MANY_UPLOADS);
        return Err(reject(state, actor, Some(&plugin_id), bytes.len(), err).await);
    }
    let repo = source.as_ref().map(|s| s.repo.as_str());
    let id = db::insert_upload(
        &mut *tx, &plugin_id, &version, &bytes, &signature, actor, repo,
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.uploaded",
        Some(&target(&plugin_id)),
        json!({
            "upload": id,
            "version": version,
            "bytes": bytes.len(),
            "sha256": hex(&sha256(&bytes)),
            "trust": trust,
            "source": repo,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Audits a refused upload (with the plugin id, once the package got far
/// enough to have one) and hands back the error.
pub async fn reject(
    state: &crate::AppState,
    actor: AccountId,
    plugin: Option<&str>,
    bytes: usize,
    err: AppError,
) -> AppError {
    let recorded = audit::record(
        &state.db,
        Actor::Account(actor),
        "plugin.upload_rejected",
        plugin.map(target).as_deref(),
        json!({ "bytes": bytes, "reason": err.message() }),
    )
    .await;
    match recorded {
        Ok(()) => err,
        Err(db) => db.into(),
    }
}

/// `(plugin id, version, trust)`, or the error with the plugin id when the
/// package got far enough to have one.
async fn check_upload(
    state: &crate::AppState,
    bytes: &[u8],
    signature: &str,
    expected: Option<(&str, &str)>,
) -> Result<(String, String, &'static str), (Option<String>, AppError)> {
    let unverified = package::read(bytes).map_err(|e| (None, package_error(e)))?;
    let id = unverified.id().to_owned();
    if state.plugins.bundled.reserves(&id) {
        return Err((Some(id.clone()), reserved(&id)));
    }
    if let Some((expected_id, expected_version)) = expected {
        let version = &unverified.package().manifest.plugin.version;
        if expected_id != id || expected_version != version {
            return Err((
                Some(id.clone()),
                AppError::bad_request(format!(
                    "The release's {expected_id}-{expected_version} package holds app {id} \
                     version {version} instead, so it isn't installed."
                )),
            ));
        }
    }
    let fail = |err: AppError| (Some(id.clone()), err);
    let db_err = |err: sqlx::Error| (Some(id.clone()), AppError::from(err));
    if db::count_uploads(&state.db).await.map_err(db_err)? >= MAX_PENDING_UPLOADS {
        return Err(fail(AppError::bad_request(TOO_MANY_UPLOADS)));
    }
    if let Some(installed) = db::get(&state.db, &id).await.map_err(db_err)? {
        // Installed from Tether's image, even if this Tether doesn't
        // bundle it any more: it pins no key, so nothing signed replaces it.
        if installed.origin == db::Origin::Bundled {
            return Err(fail(reserved(&id)));
        }
        check_upgrade(&state.db, &installed.version, unverified.package())
            .await
            .map_err(fail)?;
    }
    if let Some(why) = unsupported(unverified.package()) {
        return Err(fail(AppError::bad_request(why)));
    }
    let pinned = plugin_keys::get(&state.db, &id).await.map_err(db_err)?;
    let verified = unverified
        .verify(signature, pinned.as_deref())
        .map_err(|e| fail(package_error(e)))?;
    let version = verified.package().manifest.plugin.version.clone();
    let trust = trust_label(verified.trust());
    // Compile and link now, so a component that can't run is refused before
    // anyone is asked to approve it.
    state
        .plugins
        .host
        .load(&id, verified.into_package().component, None)
        .await
        .map_err(|e| {
            fail(AppError::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                format!("The app's component can't be loaded: {e}"),
            ))
        })?;
    Ok((id, version, trust))
}

/// An upload, read again for the approval screen, with how its key would
/// be trusted now.
pub struct Pending {
    pub upload: db::Upload,
    pub package: Package,
    pub trust: Trust,
    /// For an upgrade: the installed version.
    pub installed_version: Option<String>,
    /// For an upgrade: the installed version's package, unless it can't be
    /// read any more (a newer Tether may check manifests more strictly).
    pub installed: Option<Package>,
    /// What is installed now ([`base`]): approving is refused if it
    /// changes meanwhile, since the review compared against it.
    pub base: String,
    /// For an upgrade: the repository its updates come from now.
    pub installed_source: Option<String>,
    /// For an upgrade: how many of its migrations are new.
    pub new_migrations: usize,
}

pub async fn pending(state: &crate::AppState, upload_id: i64) -> Result<Pending, AppError> {
    let upload = db::get_upload(&state.db, upload_id)
        .await?
        .ok_or_else(|| AppError::not_found("No upload with that id is waiting."))?;
    let unverified = package::read(&upload.package).map_err(package_error)?;
    let pinned = plugin_keys::get(&state.db, unverified.id()).await?;
    let verified = unverified
        .verify(&upload.signature, pinned.as_deref())
        .map_err(package_error)?;
    let trust = verified.trust().clone();
    let package = verified.into_package();
    let now = compare(state, &package).await?;
    let installed_source =
        tether_db::plugin_sources::status(&state.db, &package.manifest.plugin.id)
            .await?
            .source;
    Ok(Pending {
        package,
        trust,
        upload,
        installed_version: now.version,
        installed: now.package,
        base: now.base,
        installed_source,
        new_migrations: now.new_migrations,
    })
}

/// What is installed under a package's id, for its review.
struct Current {
    version: Option<String>,
    /// Unless it can't be read any more.
    package: Option<Package>,
    base: String,
    /// How many of the package's migrations would be new.
    new_migrations: usize,
}

async fn compare(state: &crate::AppState, package: &Package) -> Result<Current, AppError> {
    let id = &package.manifest.plugin.id;
    // Stored packages were checked when approved; read for what it declares.
    let current = db::get(&state.db, id).await?;
    let installed = current.as_ref().and_then(|installed| {
        package::read(&installed.package)
            .inspect_err(|e| {
                tracing::warn!(plugin = installed.id, error = %e, "reading the installed package");
            })
            .ok()
            .map(|p| p.package().clone())
    });
    let new_migrations = if current.is_some() {
        let applied = plugin_storage::applied(&state.db, id).await?;
        package.migrations.len().saturating_sub(applied.len())
    } else {
        0
    };
    Ok(Current {
        version: current.as_ref().map(|c| c.version.clone()),
        package: installed,
        base: base(current.as_ref()),
        new_migrations,
    })
}

/// A bundled app, for its review before installing or upgrading to it.
pub struct BundledReview {
    pub package: Package,
    /// The bundled package's SHA-256 in hex, sent back on approval.
    pub sha256: String,
    /// For an upgrade: the installed version, and its package unless it
    /// can't be read any more.
    pub installed_version: Option<String>,
    pub installed: Option<Package>,
    /// What is installed now ([`base`]).
    pub base: String,
    pub new_migrations: usize,
}

pub async fn bundled_review(state: &crate::AppState, id: &str) -> Result<BundledReview, AppError> {
    let app = state
        .plugins
        .bundled
        .get(id)
        .ok_or_else(|| AppError::not_found("No app with that id comes with Tether."))?;
    let now = compare(state, &app.package).await?;
    Ok(BundledReview {
        package: app.package.clone(),
        sha256: hex(&app.sha256),
        installed_version: now.version,
        installed: now.package,
        base: now.base,
        new_migrations: now.new_migrations,
    })
}

/// What is installed under an id, as the review saw it: the package's
/// SHA-256 in hex, or `none`.
pub fn base(installed: Option<&db::Installed>) -> String {
    installed.map_or_else(|| "none".to_owned(), |i| hex(&i.package_sha256))
}

/// Installs (or upgrades to) an upload and starts it. Everything is
/// checked again in the transaction: the pin may have moved, or another
/// install finished, since the upload was checked. `reviewed` is the
/// [`base`] the review compared against, when the form sent it. Returns
/// the plugin id.
pub async fn approve(
    state: &crate::AppState,
    actor: AccountId,
    upload_id: i64,
    reviewed: Option<String>,
) -> Result<String, AppError> {
    let state = state.clone();
    detached(async move { approve_now(&state, actor, upload_id, reviewed).await }).await
}

async fn approve_now(
    state: &crate::AppState,
    actor: AccountId,
    upload_id: i64,
    reviewed: Option<String>,
) -> Result<String, AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    let mut tx = state.db.begin().await?;
    let upload = db::take_upload(&mut *tx, upload_id)
        .await?
        .ok_or_else(|| AppError::not_found("No upload with that id is waiting."))?;
    let unverified = package::read(&upload.package).map_err(package_error)?;
    let id = unverified.id().to_owned();
    // Uploaded before this Tether bundled the app, maybe.
    if plugins.bundled.reserves(&id) {
        return Err(reserved(&id));
    }
    let pinned = plugin_keys::get_locked(&mut tx, &id).await?;
    let verified = unverified
        .verify(&upload.signature, pinned.as_deref())
        .map_err(package_error)?;
    let candidate = Candidate::Signed {
        upload: &upload,
        verified: &verified,
    };
    install_or_upgrade(state, actor, tx, &id, candidate, reviewed).await
}

/// Installs (or upgrades to) the bundled app `id` and starts it, if the
/// bundled package is still the one reviewed (`sha256`, in hex: a newer
/// image may have replaced it since) and what is installed is still
/// `reviewed` ([`base`]). Returns the plugin id.
pub async fn approve_bundled(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    sha256: &str,
    reviewed: String,
) -> Result<String, AppError> {
    let (state, id, sha256) = (state.clone(), id.to_owned(), sha256.to_owned());
    detached(async move { approve_bundled_now(&state, actor, &id, &sha256, reviewed).await }).await
}

async fn approve_bundled_now(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    sha256: &str,
    reviewed: String,
) -> Result<String, AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    let app = plugins
        .bundled
        .get(id)
        .ok_or_else(|| AppError::not_found("No app with that id comes with Tether."))?;
    if hex(&app.sha256) != sha256 {
        return Err(AppError::bad_request(
            "Tether was updated since this page was shown, with another version of this app. \
             Look at it again before approving.",
        ));
    }
    let tx = state.db.begin().await?;
    install_or_upgrade(
        state,
        actor,
        tx,
        id,
        Candidate::Bundled(app),
        Some(reviewed),
    )
    .await
}

/// A package being installed, or upgraded to, and why it's trusted.
#[derive(Clone, Copy)]
enum Candidate<'a> {
    /// An upload (a file, or from GitHub), signed and checked against the
    /// key pinned for its id.
    Signed {
        upload: &'a db::Upload,
        verified: &'a Verified,
    },
    /// Built into this Tether's image: as trusted as the binary.
    Bundled(&'a crate::bundled::BundledApp),
}

impl Candidate<'_> {
    fn package(&self) -> &Package {
        match self {
            Self::Signed { verified, .. } => verified.package(),
            Self::Bundled(app) => &app.package,
        }
    }

    /// The package as stored.
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Signed { upload, .. } => &upload.package,
            Self::Bundled(app) => &app.bytes,
        }
    }

    fn origin(&self) -> db::Origin {
        match self {
            Self::Signed { .. } => db::Origin::Signed,
            Self::Bundled(_) => db::Origin::Bundled,
        }
    }

    fn signature(&self) -> Option<&str> {
        match self {
            Self::Signed { upload, .. } => Some(&upload.signature),
            Self::Bundled(_) => None,
        }
    }

    /// The publisher key it's signed with.
    fn key(&self) -> Option<&str> {
        match self {
            Self::Signed { verified, .. } => Some(verified.key()),
            Self::Bundled(_) => None,
        }
    }

    fn upload_id(&self) -> Option<i64> {
        match self {
            Self::Signed { upload, .. } => Some(upload.id),
            Self::Bundled(_) => None,
        }
    }

    /// The GitHub repository it was fetched from.
    fn source(&self) -> Option<&str> {
        match self {
            Self::Signed { upload, .. } => upload.source.as_deref(),
            Self::Bundled(_) => None,
        }
    }

    /// Records the key it was trusted with; a bundled package pins none.
    async fn record_trust(&self, tx: &mut PgConnection, actor: AccountId) -> Result<(), AppError> {
        match self {
            Self::Signed { verified, .. } => {
                record_trust(tx, Actor::Account(actor), verified).await
            }
            Self::Bundled(_) => Ok(()),
        }
    }

    fn new_plugin<'a>(
        &'a self,
        id: &'a str,
        package_sha256: &'a [u8],
        actor: AccountId,
    ) -> db::NewPlugin<'a> {
        let manifest = &self.package().manifest;
        db::NewPlugin {
            id,
            name: &manifest.plugin.name,
            version: &manifest.plugin.version,
            package: self.bytes(),
            signature: self.signature(),
            origin: self.origin(),
            package_sha256,
            installed_by: actor,
        }
    }
}

/// The rest of an approval, in its transaction: installs the candidate,
/// or upgrades the installed version to it.
async fn install_or_upgrade(
    state: &crate::AppState,
    actor: AccountId,
    mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
    id: &str,
    candidate: Candidate<'_>,
    reviewed: Option<String>,
) -> Result<String, AppError> {
    if let Some(why) = unsupported(candidate.package()) {
        return Err(AppError::bad_request(why));
    }
    let installed = db::get_locked(&mut tx, id).await?;
    if reviewed.is_some_and(|r| r != base(installed.as_ref())) {
        return Err(AppError::bad_request(
            "This app was installed, upgraded, rolled back or uninstalled since this page was \
             shown. Look at what changes again before approving.",
        ));
    }
    if let Some(installed) = installed {
        // A bundled install pins no key: a signed package never replaces
        // it, even once this Tether doesn't bundle its id any more.
        if installed.origin == db::Origin::Bundled && candidate.origin() == db::Origin::Signed {
            return Err(reserved(id));
        }
        return upgrade_now(state, actor, tx, candidate, installed).await;
    }
    candidate.record_trust(&mut tx, actor).await?;
    let manifest = &candidate.package().manifest;
    let package_sha256 = sha256(candidate.bytes());
    let installed =
        db::install(&mut *tx, &candidate.new_plugin(id, &package_sha256, actor)).await?;
    if !installed {
        return Err(AppError::bad_request(
            "An app with this id is already installed.",
        ));
    }
    // Where its updates are looked for.
    if let Some(source) = candidate.source() {
        tether_db::plugin_sources::set_source(&mut *tx, id, Some(source)).await?;
    }
    let declared = declared_permissions(id, manifest);
    tether_db::permissions::add_plugin_permissions(&mut tx, id, &declared).await?;
    // Exactly the hosts and secrets shown on the review page; nothing else
    // is reachable at runtime.
    crate::plugin_http::approve(&mut tx, id, manifest, actor).await?;
    // Member now requires its user scopes (F11, F16).
    if tether_db::compliance::set_plugin_scopes(&mut *tx, id, &manifest.capabilities.esi.user)
        .await?
    {
        crate::states::enqueue_evaluate_all(&mut *tx).await?;
    }
    let storage = if manifest.capabilities.storage {
        if plugin_storage::count(&mut *tx).await? >= MAX_STORAGE_PLUGINS {
            return Err(AppError::bad_request(
                "Too many apps have database storage already. Uninstall one first.",
            ));
        }
        Some(create_storage(state, &mut tx, id).await?)
    } else {
        None
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.installed",
        Some(&target(id)),
        json!({
            "origin": candidate.origin().as_str(),
            "upload": candidate.upload_id(),
            "source": candidate.source(),
            "version": manifest.plugin.version,
            "key": candidate.key(),
            "sha256": hex(&package_sha256),
            "capabilities": manifest.capabilities,
            "permissions": manifest.permissions,
            "storage": storage
                .as_ref()
                .map(|n| json!({ "schema": n.schema_name, "role": n.role_name })),
        }),
    )
    .await?;
    tx.commit().await?;

    if let Some(installed) = db::get(&state.db, id).await? {
        state.plugins.activate(&state.db, &installed).await;
    }
    Ok(id.to_owned())
}

/// A plugin's permissions by their full names, with descriptions.
fn declared_permissions(id: &str, manifest: &manifest::Manifest) -> Vec<(String, String)> {
    manifest
        .permissions
        .iter()
        .map(|(name, description)| (format!("plugin.{id}.{name}"), description.clone()))
        .collect()
}

/// Grants removed with permissions, for the audit log.
fn grants_json(grants: &[tether_db::permissions::Grant]) -> Vec<serde_json::Value> {
    grants
        .iter()
        .map(|g| match g.grantee {
            tether_db::permissions::Grantee::State(state) => {
                json!({ "permission": g.permission, "state_id": state.0 })
            }
            tether_db::permissions::Grantee::Group(group) => {
                json!({ "permission": g.permission, "group_id": group.0 })
            }
        })
        .collect()
}

/// Whether `package` can replace the installed version of its plugin: it
/// must be newer, keep its storage if it had some, and carry every
/// migration already applied, unchanged.
async fn check_upgrade(db: &PgPool, installed: &str, package: &Package) -> Result<(), AppError> {
    let id = package.manifest.plugin.id.as_str();
    let new = &package.manifest.plugin.version;
    let newer = match (
        manifest::parse_version(new),
        manifest::parse_version(installed),
    ) {
        (Some(new), Some(old)) => new > old,
        _ => false,
    };
    if !newer {
        return Err(AppError::bad_request(format!(
            "Version {installed} of this app is installed and this package is version {new}. \
             Only a newer version can be installed over it; to go back to the version before \
             an upgrade, roll back on the app's page."
        )));
    }
    if plugin_storage::get(db, id).await?.is_some() && !package.manifest.capabilities.storage {
        return Err(AppError::bad_request(
            "This version doesn't ask for database storage, but the installed one keeps data. \
             Uninstall the app first if its data should go.",
        ));
    }
    let applied = plugin_storage::applied(db, id).await?;
    check_applied(&applied, &package.migrations).map_err(|why| {
        AppError::bad_request(format!(
            "This version can't upgrade the installed one: {why}."
        ))
    })
}

/// Replaces an installed plugin with a newer version, keeping the one it
/// replaces for a rollback. Its permissions, HTTP hosts, secrets and scopes
/// become the new version's (the review showed what changed); storage is
/// created if it asks for it now. The plugin restarts; its new migrations
/// run after a snapshot of its data (see [`migrate`]).
async fn upgrade_now(
    state: &crate::AppState,
    actor: AccountId,
    mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
    candidate: Candidate<'_>,
    installed: db::Installed,
) -> Result<String, AppError> {
    let id = installed.id.clone();
    let package = candidate.package();
    let manifest = &package.manifest;
    // Checked at upload; again now the row is locked.
    check_upgrade(&state.db, &installed.version, package).await?;
    candidate.record_trust(&mut tx, actor).await?;
    let package_sha256 = sha256(candidate.bytes());
    let upgraded = db::upgrade(&mut tx, &candidate.new_plugin(&id, &package_sha256, actor)).await?;
    // The row is locked (get_locked), so it's there.
    if !upgraded {
        return Err(AppError::not_found("No app with that id is installed."));
    }
    match (candidate.source(), candidate.origin()) {
        // Fetched from a repository: its updates are looked for there now.
        (Some(source), _) => {
            tether_db::plugin_sources::set_source(&mut *tx, &id, Some(source)).await?;
        }
        // Its updates come with Tether's now.
        (None, db::Origin::Bundled) => {
            if tether_db::plugin_sources::set_source(&mut *tx, &id, None).await? {
                audit::record(
                    &mut *tx,
                    Actor::Account(actor),
                    "plugin.source_set",
                    Some(&target(&id)),
                    json!({ "source": null, "why": "bundled" }),
                )
                .await?;
            }
        }
        // An upload by hand keeps the repository it had.
        (None, db::Origin::Signed) => {}
    }
    let changed = apply_manifest(state, &mut tx, &id, manifest, actor).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.upgraded",
        Some(&target(&id)),
        json!({
            "origin": candidate.origin().as_str(),
            "upload": candidate.upload_id(),
            "source": candidate.source(),
            "from": installed.version,
            "to": manifest.plugin.version,
            "key": candidate.key(),
            "sha256": hex(&package_sha256),
            "capabilities": manifest.capabilities,
            "permissions": manifest.permissions,
            "grants_removed": grants_json(&changed.grants_removed),
            "secrets_deleted": changed.secrets_deleted,
            "storage": changed
                .storage_created
                .as_ref()
                .map(|n| json!({ "schema": n.schema_name, "role": n.role_name })),
        }),
    )
    .await?;
    tx.commit().await?;
    let plugins = &state.plugins;
    plugins.deactivate(&id).await;
    if installed.enabled
        && let Some(upgraded) = db::get(&state.db, &id).await?
    {
        plugins.activate(&state.db, &upgraded).await;
    }
    Ok(id)
}

/// What [`apply_manifest`] changed.
struct Applied {
    grants_removed: Vec<tether_db::permissions::Grant>,
    /// Secrets whose values went: gone, or now for another host, header
    /// or prefix.
    secrets_deleted: Vec<String>,
    storage_created: Option<plugin_storage::Names>,
}

/// Makes what an installed plugin may do exactly what `manifest` asks for,
/// on an upgrade or a rollback: its permissions (grants of dropped ones go),
/// HTTP hosts and secrets, and the user scopes Member requires. Creates its
/// storage if it asks for storage and has none.
async fn apply_manifest(
    state: &crate::AppState,
    tx: &mut PgConnection,
    id: &str,
    manifest: &manifest::Manifest,
    actor: AccountId,
) -> Result<Applied, AppError> {
    let declared = declared_permissions(id, manifest);
    let grants_removed = tether_db::permissions::sync_plugin_permissions(tx, id, &declared).await?;
    let secrets_deleted = crate::plugin_http::approve(tx, id, manifest, actor).await?;
    if tether_db::compliance::set_plugin_scopes(&mut *tx, id, &manifest.capabilities.esi.user)
        .await?
    {
        crate::states::enqueue_evaluate_all(&mut *tx).await?;
    }
    let storage_created =
        if manifest.capabilities.storage && plugin_storage::get(&mut *tx, id).await?.is_none() {
            if plugin_storage::count(&mut *tx).await? >= MAX_STORAGE_PLUGINS {
                return Err(AppError::bad_request(
                    "Too many apps have database storage already. Uninstall one first.",
                ));
            }
            Some(create_storage(state, tx, id).await?)
        } else {
            None
        };
    Ok(Applied {
        grants_removed,
        secrets_deleted,
        storage_created,
    })
}

/// Going back to the version an upgrade replaced: what would happen.
pub struct RollbackPlan {
    pub from: String,
    pub to: String,
    pub upgraded_at: chrono::DateTime<chrono::Utc>,
    /// The installed package, and the earlier one when it can be read: what
    /// it asks for changes from one to the other.
    pub current: Option<Package>,
    pub earlier: Option<Package>,
    /// The snapshot its data goes back to, when the upgrade changed its
    /// data (ran migrations the earlier version doesn't have).
    pub restore: Option<tether_snapshots::Snapshot>,
    /// It asked for no storage before: its data is deleted.
    pub deletes_data: bool,
    /// Why it can't be done, in plain words.
    pub blocked: Option<String>,
}

/// What rolling a plugin back would do, or `None` if there's no earlier
/// version to go back to.
pub async fn rollback_plan(
    state: &crate::AppState,
    id: &str,
) -> Result<Option<RollbackPlan>, AppError> {
    Ok(plan_rollback(state, id).await?.map(|(plan, _)| plan))
}

/// The plan, and the earlier version when it can be put back: its package
/// as stored, and the key it checked out against (none when bundled).
async fn plan_rollback(
    state: &crate::AppState,
    id: &str,
) -> Result<Option<(RollbackPlan, Option<(db::Previous, Option<String>)>)>, AppError> {
    let Some(installed) = db::get(&state.db, id).await? else {
        return Ok(None);
    };
    let Some(previous) = db::previous(&state.db, id).await? else {
        return Ok(None);
    };
    let mut plan = RollbackPlan {
        from: installed.version.clone(),
        to: previous.version.clone(),
        upgraded_at: previous.upgraded_at,
        current: package::read(&installed.package)
            .ok()
            .map(|p| p.package().clone()),
        earlier: package::read(&previous.package)
            .ok()
            .map(|p| p.package().clone()),
        restore: None,
        deletes_data: false,
        blocked: None,
    };
    let block = |mut plan: RollbackPlan, why: String| {
        plan.blocked = Some(why);
        Ok(Some((plan, None)))
    };
    // As loading does: only the package approved, signed with the key
    // pinned now (or bundled into Tether's image).
    let pinned = plugin_keys::get(&state.db, id).await?;
    let old = match earlier_checked(id, &previous, pinned.as_deref()) {
        Ok(old) => old,
        Err(why) => return block(plan, why),
    };
    if let Some(why) = unsupported(&old) {
        return block(
            plan,
            format!("Version {} can't run now: {why}", previous.version),
        );
    }
    if let Some(names) = plugin_storage::get(&state.db, id).await? {
        if !old.manifest.capabilities.storage {
            plan.deletes_data = true;
        } else {
            let applied = plugin_storage::applied(&state.db, id).await?;
            if check_applied(&applied, &old.migrations).is_err() {
                let Some(snapshots) = state.plugins.snapshots.as_deref() else {
                    return block(
                        plan,
                        "The upgrade changed its data and snapshots are off here, so its data \
                         can't be put back."
                            .to_owned(),
                    );
                };
                let found = snapshot_for(snapshots, id, &names.role_name, &old.migrations).await;
                let snapshot = match found {
                    Ok(Some(snapshot)) => snapshot,
                    Ok(None) => {
                        return block(
                            plan,
                            format!(
                                "The upgrade changed its data, and no snapshot of its data from \
                                 version {} is left to put back.",
                                previous.version
                            ),
                        );
                    }
                    Err(e) => {
                        tracing::error!(plugin = id, error = %e, "listing snapshots");
                        return block(
                            plan,
                            format!("Its snapshots couldn't be read ({}).", e.brief()),
                        );
                    }
                };
                // What would stop the restore: the tools, TimescaleDB's
                // version, the plugin's role.
                if let Err(e) = snapshots.check(&state.db, &snapshot).await {
                    return block(
                        plan,
                        format!(
                            "The upgrade changed its data, and the snapshot of it from {} UTC \
                             can't be restored: {e}.",
                            snapshot.header.taken_at.format("%Y-%m-%d %H:%M")
                        ),
                    );
                }
                plan.restore = Some(snapshot);
            }
        }
    }
    Ok(Some((plan, Some((previous, pinned)))))
}

/// The earlier package, if it's the one approved, for this plugin, and
/// signed with `pinned` (the key pinned now) or bundled into Tether's
/// image; otherwise why not.
fn earlier_checked(
    id: &str,
    previous: &db::Previous,
    pinned: Option<&str>,
) -> Result<Package, String> {
    let version = &previous.version;
    if sha256(&previous.package) != previous.package_sha256 {
        return Err(format!(
            "Version {version} isn't the package that was approved, so it can't be put back."
        ));
    }
    let package = match previous.origin {
        db::Origin::Bundled => package::read(&previous.package)
            .map_err(|e| format!("Version {version} doesn't check out: {e}."))?
            .into_bundled(),
        db::Origin::Signed => {
            let (Some(pinned), Some(signature)) = (pinned, previous.signature.as_deref()) else {
                return Err("No publisher key is pinned for this app.".to_owned());
            };
            let verified = match package::read(&previous.package)
                .and_then(|p| p.verify(signature, Some(pinned)))
            {
                Ok(verified) => verified,
                Err(PackageError::KeyChanged { .. }) => {
                    return Err(format!(
                        "Version {version} is signed with a publisher key this app has moved on \
                         from, so it can't be put back."
                    ));
                }
                Err(e) => return Err(format!("Version {version} doesn't check out: {e}.")),
            };
            if *verified.trust() != Trust::Pinned {
                return Err(format!(
                    "Version {version} isn't signed with the key pinned for this app, so it \
                     can't be put back."
                ));
            }
            verified.into_package()
        }
    };
    if package.manifest.plugin.id != id {
        return Err(format!(
            "Version {version} is another app's package, so it can't be put back."
        ));
    }
    Ok(package)
}

/// The newest snapshot of a plugin's data taken before migrations, with
/// its current role (not from before a reinstall), whose migrations are
/// all in `migrations`, unchanged: data the earlier version can run on.
async fn snapshot_for(
    snapshots: &tether_snapshots::Snapshots,
    id: &str,
    role: &str,
    migrations: &[package::Migration],
) -> Result<Option<tether_snapshots::Snapshot>, tether_snapshots::SnapshotError> {
    let kind = tether_snapshots::Kind::Plugin(id.to_owned());
    // Newest first.
    let all = tether_snapshots::list(snapshots.dir()).await?;
    Ok(all.into_iter().find(|s| {
        s.header.kind == kind
            && s.header.reason == tether_snapshots::Reason::BeforeMigrations
            && s.header.plugin_role.as_deref() == Some(role)
            && s.header.plugin_migrations.iter().all(|m| {
                migrations.iter().any(|p| {
                    i64::from(p.version) == i64::from(m.version)
                        && hex(&sha256(p.sql.as_bytes())) == m.sha256
                })
            })
    }))
}

/// Goes back to the version the last upgrade replaced, after the admin
/// typed the plugin's id: the earlier package, its permissions, hosts,
/// secrets and scopes, and, if the upgrade changed its data, its data as
/// the snapshot taken before that. One step back only.
pub async fn roll_back(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    confirmation: &str,
) -> Result<(), AppError> {
    if confirmation.trim() != id {
        return Err(AppError::bad_request(
            "Type the app's id exactly to confirm rolling it back.",
        ));
    }
    let (state, id) = (state.clone(), id.to_owned());
    detached(async move { roll_back_now(&state, actor, &id).await }).await
}

async fn roll_back_now(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
) -> Result<(), AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    let Some((plan, earlier)) = plan_rollback(state, id).await? else {
        return Err(AppError::not_found(
            "This app has no earlier version to roll back to.",
        ));
    };
    let Some((previous, key)) = earlier else {
        return Err(AppError::bad_request(
            plan.blocked
                .unwrap_or_else(|| "It can't be rolled back.".to_owned()),
        ));
    };
    let installed = db::get(&state.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("No app with that id is installed."))?;
    plugins.deactivate(id).await;
    // Runs it again as the database says if anything below fails.
    let restart = || async {
        if installed.enabled {
            match db::get(&state.db, id).await {
                Ok(Some(current)) => plugins.activate(&state.db, &current).await,
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(plugin = id, error = %e, "restarting after a rollback");
                }
            }
        }
    };
    if let (Some(snapshot), Some(snapshots)) = (&plan.restore, plugins.snapshots.as_deref())
        && let Err(e) = snapshots
            .restore(&state.db, snapshot, Actor::Account(actor))
            .await
    {
        tracing::error!(plugin = id, error = %e, "restoring a plugin for a rollback");
        restart().await;
        return Err(AppError::bad_request(format!(
            "Its data couldn't be put back ({}), so nothing was rolled back.",
            e.brief()
        )));
    }
    let finished = async {
        let mut tx = state.db.begin().await?;
        db::get_locked(&mut tx, id)
            .await?
            .ok_or_else(|| AppError::not_found("No app with that id is installed."))?;
        // The pin may have moved since the plan (a re-pin doesn't take the
        // lifecycle lock): check the earlier package against it again.
        let pinned = plugin_keys::get_locked(&mut tx, id).await?;
        if previous.origin == db::Origin::Signed && pinned != key {
            return Err(AppError::bad_request(
                "This app's pinned key changed while it was being rolled back. Look again.",
            ));
        }
        let old = earlier_checked(id, &previous, key.as_deref()).map_err(AppError::bad_request)?;
        if !db::roll_back(
            &mut tx,
            id,
            &old.manifest.plugin.name,
            &previous.package_sha256,
        )
        .await?
        {
            return Err(AppError::bad_request(
                "This app changed while it was being rolled back. Look again.",
            ));
        }
        let data_deleted = if plan.deletes_data
            && let Some(names) = plugin_storage::get(&mut *tx, id).await?
        {
            plugin_storage::drop(&mut tx, &names)
                .await
                .map_err(AppError::internal)?;
            secrets::delete(&mut *tx, &password_secret(id)).await?;
            true
        } else {
            false
        };
        let changed = apply_manifest(state, &mut tx, id, &old.manifest, actor).await?;
        audit::record(
            &mut *tx,
            Actor::Account(actor),
            "plugin.rolled_back",
            Some(&target(id)),
            json!({
                "from": plan.from,
                "to": plan.to,
                "origin": previous.origin.as_str(),
                "key": key,
                "sha256": hex(&previous.package_sha256),
                "capabilities": old.manifest.capabilities,
                "permissions": old.manifest.permissions,
                "snapshot": plan.restore.as_ref().map(|s| json!({
                    "name": s.name,
                    "taken_at": s.header.taken_at,
                })),
                "data_deleted": data_deleted,
                "grants_removed": grants_json(&changed.grants_removed),
                "secrets_deleted": changed.secrets_deleted,
            }),
        )
        .await?;
        tx.commit().await?;
        Ok::<(), AppError>(())
    }
    .await;
    // Rolled back or not, it runs as the database now says.
    restart().await;
    match (finished, &plan.restore) {
        (Err(err), Some(snapshot)) => {
            tracing::error!(
                plugin = id,
                snapshot = snapshot.name,
                error = %err.message(),
                "a rollback restored the data but didn't put the earlier version back"
            );
            Err(AppError::new(
                err.status(),
                format!(
                    "Its data was put back as it was at {} UTC, but version {} wasn't: {} \
                     Version {} runs again and migrates the restored data, so what it stored \
                     since then is lost.",
                    snapshot.header.taken_at.format("%Y-%m-%d %H:%M"),
                    plan.to,
                    err.message(),
                    plan.from
                ),
            ))
        }
        (finished, _) => finished,
    }
}

/// Creates a plugin's role (with a random password, kept sealed) and
/// schema in the install's transaction. Returns the names, for the audit
/// log.
async fn create_storage(
    state: &crate::AppState,
    tx: &mut PgConnection,
    id: &str,
) -> Result<plugin_storage::Names, AppError> {
    let mut random = [0u8; 4];
    getrandom::fill(&mut random).map_err(AppError::internal)?;
    let names = plugin_storage::Names {
        schema_name: format!("plugin_{id}"),
        role_name: format!("tp_{}_{id}", hex(&random)),
    };
    let password = tether_core::new_token().map_err(AppError::internal)?;
    let verifier = tether_core::scram::verifier(&password).map_err(AppError::internal)?;
    plugin_storage::create(
        tx,
        id,
        &names,
        &verifier,
        ROLE_CONNECTIONS,
        tether_plugins::storage::ROLE_SETTINGS,
    )
    .await
    .map_err(AppError::internal)?;
    let name = password_secret(id);
    let sealed = state
        .key
        .seal(&password, &secrets::context(&name))
        .map_err(AppError::internal)?;
    secrets::put(&mut *tx, &name, &sealed).await?;
    Ok(names)
}

/// Throws an upload away.
pub async fn discard(
    state: &crate::AppState,
    actor: AccountId,
    upload_id: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let upload = db::take_upload(&mut *tx, upload_id)
        .await?
        .ok_or_else(|| AppError::not_found("No upload with that id is waiting."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.upload_discarded",
        Some(&target(&upload.plugin_id)),
        json!({ "upload": upload_id, "version": upload.version }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Switches an installed plugin on or off, loading or unloading it.
pub async fn set_enabled(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    let (state, id) = (state.clone(), id.to_owned());
    detached(async move { set_enabled_now(&state, actor, &id, enabled).await }).await
}

async fn set_enabled_now(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    enabled: bool,
) -> Result<(), AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    let mut tx = state.db.begin().await?;
    if !db::exists(&mut *tx, id).await? {
        return Err(AppError::not_found("No app with that id is installed."));
    }
    if !enabled {
        // Switched back on when it next loads.
        plugin_jobs::set_schedules_enabled(&mut *tx, id, false).await?;
    }
    if db::set_enabled(&mut *tx, id, enabled).await? {
        audit::record(
            &mut *tx,
            Actor::Account(actor),
            if enabled {
                "plugin.enabled"
            } else {
                "plugin.disabled"
            },
            Some(&target(id)),
            json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    if enabled {
        // Also retries a plugin that failed to load.
        if let Some(installed) = db::get(&state.db, id).await? {
            plugins.activate(&state.db, &installed).await;
        }
    } else {
        plugins.deactivate(id).await;
    }
    Ok(())
}

/// Uninstalls a plugin after the admin typed its id. Its pinned key stays,
/// so a later package under the same id from someone else is still caught.
pub async fn uninstall(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
    confirmation: &str,
) -> Result<(), AppError> {
    if confirmation.trim() != id {
        return Err(AppError::bad_request(
            "Type the app's id exactly to confirm uninstalling it.",
        ));
    }
    let (state, id) = (state.clone(), id.to_owned());
    detached(async move { uninstall_now(&state, actor, &id).await }).await
}

async fn uninstall_now(
    state: &crate::AppState,
    actor: AccountId,
    id: &str,
) -> Result<(), AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    if !db::exists(&state.db, id).await? {
        return Err(AppError::not_found("No app with that id is installed."));
    }
    // Stopped first: its pool must be closed before its role goes.
    plugins.deactivate(id).await;
    let mut tx = state.db.begin().await?;
    // The plugin's row first, as enqueue locks it: the same lock order on
    // both sides, so they can't deadlock.
    sqlx::query_scalar!(
        r#"SELECT true AS "locked!" FROM core.plugins WHERE id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let storage = plugin_storage::get(&mut *tx, id).await?;
    if let Some(names) = &storage {
        plugin_storage::drop(&mut tx, names)
            .await
            .map_err(AppError::internal)?;
        secrets::delete(&mut *tx, &password_secret(id)).await?;
    }
    plugin_jobs::remove(&mut tx, id).await?;
    // Its secrets' values; its approvals go with its row.
    let secrets_deleted = tether_db::plugin_http::delete_secrets(&mut *tx, id).await?;
    // What it reported for Secure Groups and the timers it published.
    tether_db::smart_groups::forget_plugin(&mut tx, id).await?;
    let grants = tether_db::permissions::remove_plugin_grants(&mut tx, id).await?;
    // Member stops requiring its user scopes.
    crate::states::enqueue_evaluate_all(&mut *tx).await?;
    let version = db::uninstall(&mut *tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("No app with that id is installed."))?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.uninstalled",
        Some(&target(id)),
        json!({
            "version": version,
            "data_deleted": storage.is_some(),
            "secrets_deleted": secrets_deleted,
            "grants_removed": grants_json(&grants),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
