//! Plugin admin pages (F15): installed plugins, the upload form, the
//! approval screen, a plugin's page (enable, disable, uninstall) and
//! re-pinning a publisher key.

use askama::Template;
use axum::Form;
use axum::extract::multipart::{Field, Multipart, MultipartRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::ADMIN_PLUGINS;
use tether_db::audit::Actor;
use tether_db::{plugin_keys, plugins as db};
use tether_plugins::manifest::{self, Manifest};
use tether_plugins::package::{self, Package, Trust};

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugins::{self, Status};

/// Request bodies on the upload route: the largest package and signature,
/// plus room for the multipart framing.
pub const UPLOAD_BODY_LIMIT: usize =
    package::MAX_PACKAGE_BYTES + package::MAX_SIGNATURE_BYTES + 16 * 1024;
/// How long an upload's body may take to arrive: it holds the one upload
/// slot meanwhile.
pub const UPLOAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// A plugin id from the path: anything that isn't one can't exist.
fn plugin_id(id: &str) -> Result<&str, AppError> {
    manifest::check_id(id)
        .map(|()| id)
        .map_err(|_| AppError::not_found("No app with that id."))
}

/// One thing a plugin asks for, in plain words.
pub struct Capability {
    pub title: String,
    pub detail: String,
}

fn capabilities(manifest: &Manifest) -> Vec<Capability> {
    let c = &manifest.capabilities;
    let mut lines = Vec::new();
    let mut add = |title: &str, detail: String| {
        lines.push(Capability {
            title: title.to_owned(),
            detail,
        });
    };
    if c.storage {
        add(
            "Database storage",
            "Keeps its own data in a schema only it can use.".to_owned(),
        );
    }
    if !c.esi.user.is_empty() {
        add(
            "ESI access to every Member's characters",
            format!(
                "{}. Member will require these of every character: Members who haven't \
                 granted them are flagged as not compliant (and leave the Compliance Group) \
                 until they register again.",
                c.esi.user.join(", ")
            ),
        );
    }
    if !c.esi.data_source.is_empty() {
        add(
            "ESI access through characters you designate",
            c.esi.data_source.join(", "),
        );
    }
    if !c.discord.is_empty() {
        add("Discord", c.discord.join(", ").replace('_', " "));
    }
    for schedule in &c.schedules {
        add(
            "Scheduled work",
            format!("{} every {}", schedule.name, schedule.every),
        );
    }
    for host in &c.http {
        add("HTTPS requests", host.clone());
    }
    for (name, secret) in &c.secrets {
        add(
            "A secret you enter",
            format!(
                "{name}, sent only to {} in the {} header",
                secret.host, secret.header
            ),
        );
    }
    if lines.is_empty() {
        lines.push(Capability {
            title: "Nothing else".to_owned(),
            detail: "It can only show its own pages and write its own log.".to_owned(),
        });
    }
    lines
}

pub struct PermissionRow {
    pub name: String,
    pub description: String,
}

fn permissions(manifest: &Manifest) -> Vec<PermissionRow> {
    manifest
        .permissions
        .iter()
        .map(|(name, description)| PermissionRow {
            name: format!("plugin.{}.{name}", manifest.plugin.id),
            description: description.clone(),
        })
        .collect()
}

/// A package's identity, for the approval screen and the plugin's page.
pub struct About {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub key: String,
    pub storage: bool,
    pub migrations: usize,
    pub assets: usize,
    pub capabilities: Vec<Capability>,
    pub permissions: Vec<PermissionRow>,
}

impl About {
    fn new(package: &Package) -> Self {
        let m = &package.manifest;
        Self {
            id: m.plugin.id.clone(),
            name: m.plugin.name.clone(),
            version: m.plugin.version.clone(),
            description: m.plugin.description.clone(),
            repository: m.plugin.repository.clone(),
            key: m.publisher.key.clone(),
            storage: m.capabilities.storage,
            migrations: package.migrations.len(),
            assets: package.assets.len(),
            capabilities: capabilities(m),
            permissions: permissions(m),
        }
    }
}

// ---- the list ---------------------------------------------------------------

pub struct PluginRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub status: &'static str,
    pub variant: &'static str,
    pub installed: String,
}

pub struct UploadRow {
    pub id: i64,
    pub plugin_id: String,
    pub version: String,
    pub by: String,
    pub when: String,
}

pub struct PinRow {
    pub plugin_id: String,
    pub key: String,
    pub how: &'static str,
    pub when: String,
}

#[derive(Template)]
#[template(path = "admin_plugins.html")]
struct PluginsPage {
    shell: Shell,
    plugins: Vec<PluginRow>,
    uploads: Vec<UploadRow>,
    pins: Vec<PinRow>,
    upload_hours: i32,
    max_mib: usize,
    error: Option<String>,
}

fn status_label(status: &Status, enabled: bool) -> (&'static str, &'static str) {
    match (status, enabled) {
        (Status::Running, _) => ("Running", "secondary"),
        (Status::Failed(_), _) => ("Failed to load", "destructive"),
        (Status::Stopped, true) => ("Starting", "outline"),
        (Status::Stopped, false) => ("Disabled", "outline"),
    }
}

fn pinned_by(how: &str) -> &'static str {
    match how {
        "first_install" => "First install",
        "rotation" => "Rotation",
        "repin" => "Re-pinned by an admin",
        _ => "Unknown",
    }
}

async fn list_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let plugins = db::list(&state.db)
        .await?
        .into_iter()
        .map(|p| {
            let (status, variant) = status_label(&state.plugins.status(&p.id), p.enabled);
            PluginRow {
                status,
                variant,
                installed: time(p.installed_at),
                id: p.id,
                name: p.name,
                version: p.version,
            }
        })
        .collect();
    let uploads = db::list_uploads(&state.db)
        .await?
        .into_iter()
        .map(|u| UploadRow {
            id: u.id,
            plugin_id: u.plugin_id,
            version: u.version,
            by: u.uploaded_by.unwrap_or_else(|| "Someone".to_owned()),
            when: time(u.uploaded_at),
        })
        .collect();
    let pins = plugin_keys::list(&state.db)
        .await?
        .into_iter()
        .map(|p| PinRow {
            how: pinned_by(&p.pinned_by),
            when: time(p.pinned_at),
            plugin_id: p.plugin_id,
            key: p.public_key,
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &PluginsPage {
            shell,
            plugins,
            uploads,
            pins,
            upload_hours: plugins::UPLOAD_HOURS,
            max_mib: package::MAX_PACKAGE_BYTES / (1024 * 1024),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugins`
pub async fn list(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    list_page(&state, shell, None).await
}

async fn read_field(mut field: Field<'_>, cap: usize) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if bytes.len() + chunk.len() > cap {
            return Err(AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("That file is bigger than {cap} bytes."),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn multipart_error(err: axum::extract::multipart::MultipartError) -> AppError {
    let status = err.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::new(status, "The upload is too big.")
    } else {
        AppError::bad_request("The upload couldn't be read. Choose the files again.")
    }
}

/// The package and its signature, and nothing else.
async fn read_upload(mut multipart: Multipart) -> Result<(Vec<u8>, String), AppError> {
    let mut package = None;
    let mut signature = None;
    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        match field.name() {
            Some("package") if package.is_none() => {
                package = Some(read_field(field, package::MAX_PACKAGE_BYTES).await?);
            }
            Some("signature") if signature.is_none() => {
                signature = Some(read_field(field, package::MAX_SIGNATURE_BYTES).await?);
            }
            _ => {
                return Err(AppError::bad_request(
                    "An upload has exactly two files: the package and its signature.",
                ));
            }
        }
    }
    let (Some(package), Some(signature)) = (package, signature) else {
        return Err(AppError::bad_request(
            "Choose both the package (.zip) and its signature (.minisig).",
        ));
    };
    if package.is_empty() || signature.is_empty() {
        return Err(AppError::bad_request(
            "Choose both the package (.zip) and its signature (.minisig).",
        ));
    }
    let signature = String::from_utf8(signature)
        .map_err(|_| AppError::bad_request("The signature isn't a minisign signature file."))?;
    Ok((package, signature))
}

/// `POST /admin/plugins` (multipart: `package`, `signature`). The
/// permission is checked before any of the body is read.
pub async fn upload(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let Some(_permit) = state.plugins.upload_permit() else {
        let busy = AppError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Another upload is being checked. Try again in a moment.",
        );
        let err = plugins::reject(&state, session.account, None, 0, busy).await;
        return list_page(&state, shell, Some(err)).await;
    };
    let result = match multipart {
        Ok(multipart) => tokio::time::timeout(UPLOAD_READ_TIMEOUT, read_upload(multipart))
            .await
            .unwrap_or_else(|_| {
                Err(AppError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "The upload took too long to arrive. Try again.",
                ))
            }),
        Err(_) => Err(AppError::bad_request(
            "Upload the package with the form on this page.",
        )),
    };
    let result = match result {
        Ok((bytes, signature)) => plugins::upload(&state, session.account, bytes, signature).await,
        Err(err) => Err(plugins::reject(&state, session.account, None, 0, err).await),
    };
    match result {
        Ok(id) => Ok(Redirect::to(&format!("/admin/plugin-uploads/{id}")).into_response()),
        Err(err) => list_page(&state, shell, Some(err)).await,
    }
}

// ---- approval ---------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin_plugin_review.html")]
struct ReviewPage {
    shell: Shell,
    upload_id: i64,
    about: About,
    trust_title: &'static str,
    trust_detail: String,
    uploaded: String,
    error: Option<String>,
}

fn trust_text(trust: &Trust) -> (&'static str, String) {
    match trust {
        Trust::FirstInstall => (
            "New publisher key",
            "No key is pinned for this plugin id yet. Installing pins this one: every later \
             version must be signed with it."
                .to_owned(),
        ),
        Trust::Pinned => (
            "Signed with the pinned key",
            "This is the key earlier versions of this app were signed with.".to_owned(),
        ),
        Trust::Rotated { from } => (
            "Key rotation",
            format!(
                "The publisher moved to a new key, and the pinned key ({from}) signed a \
                 statement endorsing it. Installing pins the new key."
            ),
        ),
    }
}

async fn review_page(
    state: &AppState,
    shell: Shell,
    upload_id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let pending = plugins::pending(state, upload_id).await?;
    let (trust_title, trust_detail) = trust_text(&pending.trust);
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &ReviewPage {
            shell,
            upload_id,
            about: About::new(&pending.package),
            trust_title,
            trust_detail,
            uploaded: time(pending.upload.uploaded_at),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugin-uploads/{id}`: what the plugin asks for, to approve.
pub async fn review(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(upload_id): Path<i64>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    review_page(&state, shell, upload_id, None).await
}

/// `POST /admin/plugin-uploads/{id}/approve`
pub async fn approve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(upload_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    match plugins::approve(&state, session.account, upload_id).await {
        Ok(id) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        // The upload is gone (or never was): back to the list.
        Err(err) if err.status() == StatusCode::NOT_FOUND => {
            list_page(&state, shell, Some(err)).await
        }
        Err(err) => review_page(&state, shell, upload_id, Some(err)).await,
    }
}

/// `POST /admin/plugin-uploads/{id}/discard`
pub async fn discard(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(upload_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    match plugins::discard(&state, session.account, upload_id).await {
        Ok(()) => Ok(Redirect::to("/admin/plugins").into_response()),
        Err(err) => list_page(&state, shell, Some(err)).await,
    }
}

// ---- one plugin -------------------------------------------------------------

pub struct SourceView {
    pub character_id: i64,
    pub name: String,
    pub offered_by: String,
    pub when: String,
    /// approved, moved (approved for another corporation) or waiting.
    pub state: &'static str,
    pub corporation: String,
}

pub struct ChannelView {
    pub id: i64,
    pub name: String,
}

pub struct AccessView {
    pub at: String,
    pub character: String,
    pub endpoint: String,
    pub outcome: String,
}

pub struct ScheduleView {
    pub name: String,
    pub every: String,
    pub enabled: bool,
    pub next_run: String,
    pub last_run: String,
}

pub struct JobView {
    pub name: String,
    pub key: String,
    pub state: String,
    pub attempts: i32,
    pub when: String,
    pub error: String,
}

pub struct LogView {
    pub at: String,
    pub level: String,
    pub source: String,
    pub message: String,
}

fn every(secs: i32) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("every {} day(s)", s / 86_400),
        s if s % 3_600 == 0 => format!("every {} hour(s)", s / 3_600),
        s => format!("every {} minute(s)", s / 60),
    }
}

fn job_view(j: tether_db::plugin_jobs::JobRow) -> JobView {
    JobView {
        name: j.name,
        key: j.key.unwrap_or_default(),
        state: j.state,
        attempts: j.attempts,
        when: time(j.run_at),
        error: j.last_error.unwrap_or_default(),
    }
}

#[derive(Template)]
#[template(path = "admin_plugin.html")]
struct PluginPage {
    shell: Shell,
    sources: Vec<SourceView>,
    channels: Vec<ChannelView>,
    free_channels: Vec<ChannelView>,
    uses_discord: bool,
    esi_scopes: Vec<String>,
    access: Vec<AccessView>,
    schedules: Vec<ScheduleView>,
    active_jobs: i64,
    upcoming: Vec<JobView>,
    dead: Vec<JobView>,
    logs: Vec<LogView>,
    about: About,
    enabled: bool,
    status: &'static str,
    variant: &'static str,
    failure: Option<String>,
    installed: String,
    error: Option<String>,
}

async fn plugin_page(
    state: &AppState,
    shell: Shell,
    id: &str,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let installed = db::get(&state.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("No app with that id is installed."))?;
    // Stored packages were checked at install; read it for what it declares.
    let package = package::read(&installed.package)
        .map_err(AppError::internal)?
        .package()
        .clone();
    let status = state.plugins.status(id);
    let (label, variant) = status_label(&status, installed.enabled);
    let schedules = tether_db::plugin_jobs::schedules(&state.db, id)
        .await?
        .into_iter()
        .map(|s| ScheduleView {
            name: s.name,
            every: every(s.every_secs),
            enabled: s.enabled,
            next_run: time(s.next_run_at),
            last_run: s.last_enqueued_at.map_or_else(|| "never".to_owned(), time),
        })
        .collect();
    let (active_jobs, upcoming, dead) = tether_db::plugin_jobs::jobs(&state.db, id, 20).await?;
    let logs = tether_db::plugin_jobs::logs(&state.db, id, 50)
        .await?
        .into_iter()
        .map(|l| LogView {
            at: l.at.format("%Y-%m-%d %H:%M:%S").to_string(),
            level: l.level,
            source: l.source,
            message: l.message,
        })
        .collect();
    let sources = tether_db::plugin_esi::data_sources(&state.db, id)
        .await?
        .into_iter()
        .map(|d| {
            let state = if d.in_use() {
                "approved"
            } else if d.approved && !d.account_ok {
                "suspended"
            } else if d.approved {
                "moved"
            } else {
                "waiting"
            };
            SourceView {
                character_id: d.character.id,
                corporation: d
                    .character
                    .corporation_id
                    .map_or_else(|| "unknown".to_owned(), |c| c.to_string()),
                name: d.character.name,
                offered_by: d.offered_by.unwrap_or_else(|| "Someone".to_owned()),
                when: time(d.offered_at),
                state,
            }
        })
        .collect();
    let (channels, free_channels) = match crate::discord::config(state).await {
        Ok(config) => {
            let guild = i64::try_from(config.guild_id).map_err(AppError::internal)?;
            let assigned = tether_db::plugin_esi::channels(&state.db, id, guild).await?;
            let all = tether_db::pings::channels(&state.db, guild).await?;
            let free = all
                .into_iter()
                .filter(|c| !assigned.iter().any(|(id, _)| *id == c.channel_id))
                .map(|c| ChannelView {
                    id: c.channel_id,
                    name: c.name,
                })
                .collect();
            let assigned = assigned
                .into_iter()
                .map(|(id, name)| ChannelView { id, name })
                .collect();
            (assigned, free)
        }
        Err(_) => (Vec::new(), Vec::new()),
    };
    let access = tether_db::plugin_esi::access_log(&state.db, id, 30)
        .await?
        .into_iter()
        .map(|a| AccessView {
            at: a.at.format("%Y-%m-%d %H:%M:%S").to_string(),
            character: a.character.unwrap_or_default(),
            endpoint: a.endpoint,
            outcome: a.outcome,
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &PluginPage {
            shell,
            schedules,
            active_jobs,
            upcoming: upcoming.into_iter().map(job_view).collect(),
            dead: dead.into_iter().map(job_view).collect(),
            logs,
            sources,
            channels,
            free_channels,
            uses_discord: package
                .manifest
                .capabilities
                .discord
                .iter()
                .any(|a| a == "send_message"),
            esi_scopes: package
                .manifest
                .capabilities
                .esi
                .user
                .iter()
                .chain(&package.manifest.capabilities.esi.data_source)
                .cloned()
                .collect(),
            access,
            about: About::new(&package),
            enabled: installed.enabled,
            status: label,
            variant,
            failure: if plugins::sha256(&installed.package) != installed.package_sha256 {
                Some(
                    "The stored package isn't the one that was approved, so it won't load. \
                     What's shown below is what it contains now."
                        .to_owned(),
                )
            } else {
                match status {
                    Status::Failed(why) => Some(why),
                    _ => None,
                }
            },
            installed: time(installed.installed_at),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugins/{id}`
pub async fn plugin(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    plugin_page(&state, shell, id, None).await
}

async fn switch(
    state: AppState,
    session: Option<CurrentSession>,
    id: String,
    enabled: bool,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match plugins::set_enabled(&state, session.account, id, enabled).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/plugins/{id}/enable`
pub async fn enable(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    switch(state, session, id, true).await
}

/// `POST /admin/plugins/{id}/disable`
pub async fn disable(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    switch(state, session, id, false).await
}

#[derive(Debug, Deserialize)]
pub struct ConfirmForm {
    #[serde(default)]
    confirmation: String,
}

/// `POST /admin/plugins/{id}/uninstall`
pub async fn uninstall(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match plugins::uninstall(&state, session.account, id, &form.confirmation).await {
        Ok(()) => Ok(Redirect::to("/admin/plugins").into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

// ---- re-pinning a key -------------------------------------------------------

#[derive(Template)]
#[template(path = "admin_plugin_key.html")]
struct KeyPage {
    shell: Shell,
    plugin_id: String,
    key: String,
    how: &'static str,
    when: String,
    new_key: String,
    error: Option<String>,
}

async fn key_page(
    state: &AppState,
    shell: Shell,
    id: &str,
    new_key: String,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let pin = plugin_keys::pin(&state.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("No key is pinned for that app."))?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &KeyPage {
            shell,
            how: pinned_by(&pin.pinned_by),
            when: time(pin.pinned_at),
            plugin_id: pin.plugin_id,
            key: pin.public_key,
            new_key,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugin-keys/{id}`
pub async fn key(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    key_page(&state, shell, id, String::new(), None).await
}

#[derive(Debug, Deserialize)]
pub struct RepinForm {
    #[serde(default)]
    expected_old: String,
    #[serde(default)]
    new_key: String,
    #[serde(default)]
    confirmation: String,
}

/// `POST /admin/plugin-keys/{id}`
pub async fn repin(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<RepinForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let result = plugins::repin_key(
        &state.db,
        Actor::Account(session.account),
        id,
        &form.expected_old,
        &form.new_key,
        &form.confirmation,
    )
    .await;
    match result {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugin-keys/{id}")).into_response()),
        // Keep what they typed; the key is public.
        Err(err) => key_page(&state, shell, id, form.new_key, Some(err)).await,
    }
}

// ---- data sources and channels ---------------------------------------------

/// `POST /admin/plugins/{id}/sources/{character}/approve`
pub async fn approve_source(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match crate::plugin_consent::approve_source(&state, session.account, id, character).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/plugins/{id}/sources/{character}/remove`
pub async fn remove_source(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match crate::plugin_consent::remove_source_as_admin(&state, session.account, id, character)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ChannelForm {
    channel_id: String,
}

/// `POST /admin/plugins/{id}/channels`
pub async fn assign_channel(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<ChannelForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let Ok(channel) = form.channel_id.trim().parse::<i64>() else {
        let err = AppError::bad_request("Choose a channel.");
        return plugin_page(&state, shell, id, Some(err)).await;
    };
    match crate::plugin_consent::set_channel(&state, session.account, id, channel, true).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/plugins/{id}/channels/{channel}/remove`
pub async fn remove_channel(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, channel)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match crate::plugin_consent::set_channel(&state, session.account, id, channel, false).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}
