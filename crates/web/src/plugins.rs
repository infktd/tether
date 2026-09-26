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
//! everything it asks for. Approving checks it all again inside the
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
}

/// The plugins running in this process.
pub struct Plugins {
    host: Host,
    /// Opens plugins' database passwords.
    key: EncryptionKey,
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
        let http = Arc::new(crate::plugin_http::Http::new());
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
                slots: RwLock::default(),
                lifecycle: tokio::sync::Mutex::new(()),
                uploads: tokio::sync::Semaphore::new(1),
                http,
            }
        })
    }

    /// Tests only: serves every plugin's approved hosts from a plain-HTTP
    /// stand-in at `host_port`. Compiled out of release builds.
    #[cfg(feature = "plugin-http-test")]
    pub fn route_http_to(&self, host_port: &str) {
        self.http.route_to(host_port);
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
    /// with the key pinned now. After a re-pin, a package signed with the
    /// old key stops loading.
    async fn activate(&self, db: &PgPool, installed: &db::Installed) {
        let slot = match self.load(db, installed).await {
            Ok(running) => {
                tracing::info!(
                    plugin = installed.id,
                    version = installed.version,
                    "plugin loaded"
                );
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
        let pinned = plugin_keys::get(db, &installed.id)
            .await
            .map_err(|e| {
                tracing::error!(plugin = installed.id, error = %e, "reading a pinned key");
                "its pinned key couldn't be read".to_owned()
            })?
            .ok_or("no publisher key is pinned for it")?;
        let verified = package::read(&installed.package)
            .and_then(|p| p.verify(&installed.signature, Some(&pinned)))
            .map_err(|e| format!("the stored package doesn't check out: {e}"))?;
        if *verified.trust() != Trust::Pinned {
            return Err(
                "the stored package isn't signed with the pinned key; install a version signed \
                 with it"
                    .to_owned(),
            );
        }
        let package = verified.into_package();
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
        migrate(db, &options, id, &package.migrations).await?;
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

/// Where a plugin's database password is kept (sealed) in `core.secrets`.
fn password_secret(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.db_password")
}

/// Runs a plugin's pending migrations over its own role's connection, one
/// transaction each, recording each with its checksum. A migration already
/// applied must be unchanged. The host ends the session of one that runs
/// past [`MIGRATION_DEADLINE`] (a plugin's SQL can lift its own statement
/// timeout).
async fn migrate(
    db: &PgPool,
    options: &PgConnectOptions,
    plugin: &str,
    migrations: &[package::Migration],
) -> Result<(), String> {
    let failed = |e: sqlx::Error| {
        tracing::error!(plugin, error = %e, "plugin migrations");
        "its migrations couldn't be checked".to_owned()
    };
    let applied = plugin_storage::applied(db, plugin).await.map_err(failed)?;
    for (version, sha) in &applied {
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
    let pending = &migrations[applied.len().min(migrations.len())..];
    if pending.is_empty() {
        return Ok(());
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

fn trust_label(trust: &Trust) -> &'static str {
    match trust {
        Trust::FirstInstall => "first_install",
        Trust::Pinned => "pinned",
        Trust::Rotated { .. } => "rotated",
    }
}

/// An uploaded package, checked (signature, pinned key, component) and
/// waiting for approval. Rejections are audited too, with the reason.
pub async fn upload(
    state: &crate::AppState,
    actor: AccountId,
    bytes: Vec<u8>,
    signature: String,
) -> Result<i64, AppError> {
    let result = check_upload(state, &bytes, &signature).await;
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
    let id = db::insert_upload(&mut *tx, &plugin_id, &version, &bytes, &signature, actor).await?;
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
) -> Result<(String, String, &'static str), (Option<String>, AppError)> {
    let unverified = package::read(bytes).map_err(|e| (None, package_error(e)))?;
    let id = unverified.id().to_owned();
    let fail = |err: AppError| (Some(id.clone()), err);
    let db_err = |err: sqlx::Error| (Some(id.clone()), AppError::from(err));
    if db::count_uploads(&state.db).await.map_err(db_err)? >= MAX_PENDING_UPLOADS {
        return Err(fail(AppError::bad_request(TOO_MANY_UPLOADS)));
    }
    if db::exists(&state.db, &id).await.map_err(db_err)? {
        return Err(fail(AppError::bad_request(
            "A plugin with this id is already installed. Upgrading isn't supported yet: \
             uninstall it first.",
        )));
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
    Ok(Pending {
        package: verified.into_package(),
        trust,
        upload,
    })
}

/// Installs an upload and starts it. Everything is checked again in the
/// transaction: the pin may have moved, or another install finished, since
/// the upload was checked. Returns the plugin id.
pub async fn approve(
    state: &crate::AppState,
    actor: AccountId,
    upload_id: i64,
) -> Result<String, AppError> {
    let state = state.clone();
    detached(async move { approve_now(&state, actor, upload_id).await }).await
}

async fn approve_now(
    state: &crate::AppState,
    actor: AccountId,
    upload_id: i64,
) -> Result<String, AppError> {
    let plugins = &state.plugins;
    let _lifecycle = plugins.lifecycle.lock().await;
    let mut tx = state.db.begin().await?;
    let upload = db::take_upload(&mut *tx, upload_id)
        .await?
        .ok_or_else(|| AppError::not_found("No upload with that id is waiting."))?;
    let unverified = package::read(&upload.package).map_err(package_error)?;
    let id = unverified.id().to_owned();
    let pinned = plugin_keys::get_locked(&mut tx, &id).await?;
    let verified = unverified
        .verify(&upload.signature, pinned.as_deref())
        .map_err(package_error)?;
    if let Some(why) = unsupported(verified.package()) {
        return Err(AppError::bad_request(why));
    }
    record_trust(&mut tx, Actor::Account(actor), &verified).await?;
    let manifest = &verified.package().manifest;
    let package_sha256 = sha256(&upload.package);
    let installed = db::install(
        &mut *tx,
        &db::NewPlugin {
            id: &id,
            name: &manifest.plugin.name,
            version: &manifest.plugin.version,
            package: &upload.package,
            signature: &upload.signature,
            package_sha256: &package_sha256,
            installed_by: actor,
        },
    )
    .await?;
    if !installed {
        return Err(AppError::bad_request(
            "An app with this id is already installed.",
        ));
    }
    let declared: Vec<(String, String)> = manifest
        .permissions
        .iter()
        .map(|(name, description)| (format!("plugin.{id}.{name}"), description.clone()))
        .collect();
    tether_db::permissions::add_plugin_permissions(&mut tx, &id, &declared).await?;
    // Exactly the hosts and secrets shown on the review page; nothing else
    // is reachable at runtime.
    crate::plugin_http::approve(&mut tx, &id, manifest, actor).await?;
    // Member now requires its user scopes (F11, F16).
    if tether_db::compliance::set_plugin_scopes(&mut *tx, &id, &manifest.capabilities.esi.user)
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
        Some(create_storage(state, &mut tx, &id).await?)
    } else {
        None
    };
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "plugin.installed",
        Some(&target(&id)),
        json!({
            "upload": upload_id,
            "version": manifest.plugin.version,
            "key": verified.key(),
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

    if let Some(installed) = db::get(&state.db, &id).await? {
        plugins.activate(&state.db, &installed).await;
    }
    Ok(id)
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
            "grants_removed": grants
                .iter()
                .map(|g| match g.grantee {
                    tether_db::permissions::Grantee::State(state) => {
                        json!({ "permission": g.permission, "state_id": state.0 })
                    }
                    tether_db::permissions::Grantee::Group(group) => {
                        json!({ "permission": g.permission, "group_id": group.0 })
                    }
                })
                .collect::<Vec<_>>(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
