//! Plugin admin pages (F15): installed plugins, the apps that come with
//! Tether, the approval screen, a plugin's page (enable, disable,
//! uninstall) and re-pinning a publisher key. Installing from an uploaded
//! .zip is for app developers: only in a development build (`dev-upload`).

use askama::Template;
use axum::Form;
#[cfg(feature = "dev-upload")]
use axum::extract::multipart::{Field, Multipart, MultipartRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::ADMIN_PLUGINS;
use tether_db::audit::Actor;
use tether_db::{plugin_keys, plugins as db};
use tether_plugins::manifest;
use tether_plugins::package::{self, Package, Trust};

use super::admin::guard;
use super::toolbar::{self, ListQuery, ToolbarView};
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugin_review::{
    Capability, Changes, PageRuleRow, PermissionRow, capabilities, page_rules, permissions,
};
use crate::plugins::{self, Status};

/// Request bodies on the upload route: the largest package and signature,
/// plus room for the multipart framing.
#[cfg(feature = "dev-upload")]
pub const UPLOAD_BODY_LIMIT: usize =
    package::MAX_PACKAGE_BYTES + package::MAX_SIGNATURE_BYTES + 16 * 1024;
/// How long an upload's body may take to arrive: it holds the one upload
/// slot meanwhile.
#[cfg(feature = "dev-upload")]
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

/// A package's identity, for the approval screen and the plugin's page.
pub struct About {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub repository: Option<String>,
    /// The publisher key it's signed with; `None` for an app bundled into
    /// Tether, which isn't signed.
    pub key: Option<String>,
    pub storage: bool,
    pub migrations: usize,
    pub assets: usize,
    pub capabilities: Vec<Capability>,
    pub permissions: Vec<PermissionRow>,
    /// Who may open which pages; the rest are for admins only.
    pub pages: Vec<PageRuleRow>,
}

impl About {
    fn new(package: &Package, bundled: bool) -> Self {
        let m = &package.manifest;
        Self {
            id: m.plugin.id.clone(),
            name: m.plugin.name.clone(),
            version: m.plugin.version.clone(),
            description: m.plugin.description.clone(),
            repository: m.plugin.repository.clone(),
            key: if bundled {
                None
            } else {
                m.publisher.as_ref().map(|p| p.key.clone())
            },
            storage: m.capabilities.storage,
            migrations: package.migrations.len(),
            assets: package.assets.len(),
            capabilities: capabilities(m),
            permissions: permissions(m),
            pages: page_rules(m),
        }
    }
}

// ---- the list ---------------------------------------------------------------

/// One app on the Apps page: installed, or included with Tether and not
/// yet installed. Each app appears once.
pub struct PluginRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub status: &'static str,
    /// The status line's tone.
    pub tone: &'static str,
    /// It comes with Tether.
    pub included: bool,
    pub is_installed: bool,
    /// A newer version its repository publishes, or that comes with this
    /// Tether.
    pub update: Option<String>,
    /// This Tether carries its package rebuilt, at the same version.
    pub rebuilt: bool,
    /// Where reviewing the install or the update happens, for an app
    /// included with Tether.
    pub review: Option<String>,
    /// It failed to load because it doesn't fit this Tether's app
    /// interface; the error, for admins.
    pub incompatible: Option<String>,
}

/// An app that comes with Tether, and whether it's installed.
pub struct BundledRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// The version installed, if any.
    pub installed: Option<String>,
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
#[template(path = "admin_data_sources.html")]
struct DataSourcesPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Data sources in all, before the toolbar's search and filters.
    total: usize,
    rows: Vec<tether_web_core::pages::plugin_access::SourceRow>,
    broken: usize,
}

/// Data sources' toolbar: the search, the app, and whether they work
/// (`working`, `broken`).
#[derive(Debug, Default, Deserialize)]
pub struct SourcesParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    status: String,
}

/// `GET /admin/data-sources`: every app's data sources, those not working
/// first; each app's own page adds and removes them. The sidebar's foot
/// and Health link here.
pub async fn data_sources(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<SourcesParams>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "data_sources").await?;
    let all = tether_web_core::pages::plugin_access::every_source(&state).await?;
    let broken = all.iter().filter(|r| r.broken()).count();
    let total = all.len();
    let mut apps: Vec<String> = Vec::new();
    for r in &all {
        if !apps.contains(&r.app) {
            apps.push(r.app.clone());
        }
    }
    apps.sort_by_key(|a| a.to_lowercase());
    let app = Some(params.app.trim()).filter(|a| apps.iter().any(|x| x == a));
    let statuses = [("working", "Working"), ("broken", "Not working")];
    let status = statuses
        .iter()
        .find(|(value, _)| *value == params.status.trim())
        .map(|(value, _)| *value);
    let list = ListQuery::new("/admin/data-sources")
        .param("q", &params.q)
        .param("app", app.unwrap_or(""))
        .param("status", status.unwrap_or(""));
    let words = list.words();
    let rows = all
        .into_iter()
        .filter(|r| app.is_none_or(|a| r.app == a))
        .filter(|r| status.is_none_or(|s| r.broken() == (s == "broken")))
        .filter(|r| toolbar::matches(&words, &[&r.app, &r.name, &r.corporation]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search apps, pilots and corporations")
        .filter(
            &list,
            "App",
            "app",
            apps.iter().map(|a| (a.clone(), a.clone())),
        )
        .filter(&list, "Status", "status", statuses);
    Ok(render(
        StatusCode::OK,
        &DataSourcesPage {
            shell,
            toolbar,
            total,
            rows,
            broken,
        },
    ))
}

#[derive(Template)]
#[template(path = "admin_plugins.html")]
struct PluginsPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under the toolbar's search or filter.
    searched: bool,
    /// Installed apps, by name.
    installed: Vec<PluginRow>,
    /// Included with Tether and not installed yet, by name.
    available: Vec<PluginRow>,
    uploads: Vec<UploadRow>,
    pins: Vec<PinRow>,
    upload_hours: i32,
    max_mib: usize,
    /// Whether installing from GitHub is set up here.
    github: bool,
    /// Whether this is a development build that installs from a file.
    upload_form: bool,
    /// Updates and rebuilds of included apps waiting for review.
    included_updates: usize,
    error: Option<String>,
}

use plugins::newer;

/// An app's status as a status line: its word and tone (DESIGN.md).
fn status_label(status: &Status, enabled: bool) -> (&'static str, &'static str) {
    match (status, enabled) {
        (Status::Running, _) => ("Running", "ok"),
        (Status::Failed(_) | Status::Incompatible(_), _) => ("Failed to load", "danger"),
        (Status::Stopped, true) => ("Starting", "off"),
        (Status::Stopped, false) => ("Disabled", "off"),
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

/// The Apps page's toolbar: its search and the apps' status (`running`,
/// `disabled`, `failed`, `update`: an update waits; `not_installed`).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    status: String,
}

const STATUSES: [(&str, &str); 5] = [
    ("running", "Running"),
    ("disabled", "Disabled"),
    ("failed", "Failed to load"),
    ("update", "Update waiting"),
    ("not_installed", "Not installed"),
];

fn has_status(p: &PluginRow, status: &str) -> bool {
    match status {
        "running" => p.status == "Running",
        "disabled" => p.status == "Disabled",
        "failed" => p.status == "Failed to load",
        "update" => p.update.is_some() || (p.is_installed && p.review.is_some()),
        _ => !p.is_installed,
    }
}

async fn list_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
    params: &ListParams,
) -> Result<Response, PageError> {
    let latest = tether_db::plugin_sources::latest(&state.db).await?;
    let installed = db::list(&state.db).await?;
    let included = state.plugins.bundled();
    let bundled: Vec<BundledRow> = included
        .all()
        .into_iter()
        .map(|app| {
            let plugin = &app.package.manifest.plugin;
            let current = installed.iter().find(|p| p.id == plugin.id);
            BundledRow {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                description: plugin.description.clone(),
                installed: current.map(|p| p.version.clone()),
            }
        })
        .collect();
    let mut plugins: Vec<PluginRow> = installed
        .iter()
        .map(|p| {
            let status = state.plugins.status(&p.id);
            let (label, tone) = status_label(&status, p.enabled);
            let bundled_app = included.get(&p.id);
            // A bundled app's updates come with Tether, never from GitHub.
            let offer = bundled_app.and_then(|app| {
                plugins::bundled_offer(&p.version, p.origin, &p.package_sha256, app)
            });
            let update = match (bundled_app, offer) {
                (Some(app), Some(plugins::Offer::Newer)) => {
                    Some(app.package.manifest.plugin.version.clone())
                }
                (Some(_), _) => None,
                (None, _) => latest
                    .iter()
                    .find(|(id, v)| *id == p.id && newer(v, &p.version))
                    .map(|(_, v)| v.clone()),
            };
            PluginRow {
                status: label,
                tone,
                included: bundled_app.is_some(),
                is_installed: true,
                description: bundled_app
                    .and_then(|a| a.package.manifest.plugin.description.clone()),
                review: offer.map(|_| format!("/admin/plugin-bundled/{}", p.id)),
                rebuilt: offer == Some(plugins::Offer::Rebuilt),
                incompatible: match status {
                    Status::Incompatible(why) => Some(why),
                    _ => None,
                },
                update,
                id: p.id.clone(),
                name: p.name.clone(),
                version: p.version.clone(),
            }
        })
        .collect();
    // What "Approve all included updates" would take: updates of apps
    // installed from the bundle.
    let included_updates = installed
        .iter()
        .filter(|p| p.origin == db::Origin::Bundled)
        .filter(|p| {
            included.get(&p.id).is_some_and(|app| {
                plugins::bundled_offer(&p.version, p.origin, &p.package_sha256, app).is_some()
            })
        })
        .count();
    // Included with Tether but not installed: after the installed ones.
    for b in &bundled {
        if b.installed.is_none() {
            plugins.push(PluginRow {
                id: b.id.clone(),
                name: b.name.clone(),
                description: b.description.clone(),
                version: b.version.clone(),
                status: "Not installed",
                tone: "off",
                included: true,
                is_installed: false,
                update: None,
                rebuilt: false,
                review: Some(format!("/admin/plugin-bundled/{}", b.id)),
                incompatible: None,
            });
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    let status = STATUSES
        .iter()
        .find(|(value, _)| *value == params.status.trim())
        .map(|(value, _)| *value);
    let list = ListQuery::new("/admin/plugins")
        .param("q", &params.q)
        .param("status", status.unwrap_or(""));
    let words = list.words();
    let (installed, available): (Vec<PluginRow>, Vec<PluginRow>) = plugins
        .into_iter()
        .filter(|p| status.is_none_or(|s| has_status(p, s)))
        .filter(|p| {
            toolbar::matches(
                &words,
                &[&p.name, &p.id, p.description.as_deref().unwrap_or("")],
            )
        })
        .partition(|p| p.is_installed);
    let toolbar = ToolbarView::new(&list)
        .search("Search apps")
        .filter(&list, "Status", "status", STATUSES);
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
            searched: list.href() != list.path,
            toolbar,
            installed,
            available,
            uploads,
            pins,
            upload_hours: plugins::UPLOAD_HOURS,
            max_mib: package::MAX_PACKAGE_BYTES / (1024 * 1024),
            github: state.plugins.github().is_some(),
            upload_form: cfg!(feature = "dev-upload"),
            included_updates,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugins`
pub async fn list(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    list_page(&state, shell, None, &params).await
}

#[cfg(feature = "dev-upload")]
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

#[cfg(feature = "dev-upload")]
fn multipart_error(err: axum::extract::multipart::MultipartError) -> AppError {
    let status = err.status();
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        AppError::new(status, "The upload is too big.")
    } else {
        AppError::bad_request("The upload couldn't be read. Choose the files again.")
    }
}

/// The package and its signature, and nothing else.
#[cfg(feature = "dev-upload")]
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
/// permission is checked before any of the body is read. Development
/// builds only (`dev-upload`).
#[cfg(feature = "dev-upload")]
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
        return list_page(&state, shell, Some(err), &ListParams::default()).await;
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
        Err(err) => list_page(&state, shell, Some(err), &ListParams::default()).await,
    }
}

// ---- approval ---------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin_plugin_review.html")]
struct ReviewPage {
    shell: Shell,
    /// Where the form approving it posts.
    approve_action: String,
    /// Where discarding an upload posts (a bundled app has nothing to
    /// discard).
    discard_action: Option<String>,
    /// A bundled package's SHA-256, sent back on approval.
    bundled_sha256: Option<String>,
    about: About,
    trust_title: &'static str,
    trust_detail: String,
    /// When it was uploaded (not for a bundled app).
    uploaded: Option<String>,
    /// What is installed now, sent back on approval.
    base: String,
    /// The GitHub repository it was fetched from.
    source: Option<String>,
    /// Approving points the app's updates at `source` instead of here.
    source_was: Option<String>,
    upgrade: Option<UpgradeView>,
    error: Option<String>,
}

/// What an upgrade (or a rollback) changes in what the plugin asks for.
pub struct UpgradeView {
    pub from: String,
    /// `None` when the installed version's package can't be read.
    pub changes: Option<Changes>,
    pub new_migrations: usize,
    pub snapshots: bool,
    /// The [`plugins::base`] the review compares against.
    pub base: String,
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
    let changes = match &pending.installed {
        Some(old) => Some(Changes::counted(state, &old.manifest, &pending.package.manifest).await?),
        None => None,
    };
    let upgrade = pending.installed_version.map(|from| UpgradeView {
        from,
        changes,
        new_migrations: pending.new_migrations,
        snapshots: state.plugins.snapshots_on(),
        base: pending.base.clone(),
    });
    Ok(render(
        code,
        &ReviewPage {
            shell,
            approve_action: format!("/admin/plugin-uploads/{upload_id}/approve"),
            discard_action: Some(format!("/admin/plugin-uploads/{upload_id}/discard")),
            bundled_sha256: None,
            about: About::new(&pending.package, false),
            trust_title,
            trust_detail,
            uploaded: Some(time(pending.upload.uploaded_at)),
            base: pending.base,
            source_was: match (&pending.upload.source, &pending.installed_source) {
                (Some(new), Some(old)) if new != old => Some(old.clone()),
                _ => None,
            },
            source: pending.upload.source.clone(),
            upgrade,
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

#[derive(Deserialize)]
pub struct ApproveForm {
    /// What was installed when the review was shown.
    #[serde(default)]
    reviewed: Option<String>,
}

/// `POST /admin/plugin-uploads/{id}/approve`
pub async fn approve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(upload_id): Path<i64>,
    Form(form): Form<ApproveForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    match plugins::approve(&state, session.account, upload_id, form.reviewed).await {
        Ok(id) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        // The upload is gone (or never was): back to the list.
        Err(err) if err.status() == StatusCode::NOT_FOUND => {
            list_page(&state, shell, Some(err), &ListParams::default()).await
        }
        Err(err) => review_page(&state, shell, upload_id, Some(err)).await,
    }
}

async fn bundled_review_page(
    state: &AppState,
    shell: Shell,
    id: &str,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let review = plugins::bundled_review(state, id).await?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let changes = match &review.installed {
        Some(old) => Some(Changes::counted(state, &old.manifest, &review.package.manifest).await?),
        None => None,
    };
    let upgrade = review.installed_version.map(|from| UpgradeView {
        from,
        changes,
        new_migrations: review.new_migrations,
        snapshots: state.plugins.snapshots_on(),
        base: review.base.clone(),
    });
    Ok(render(
        code,
        &ReviewPage {
            shell,
            approve_action: format!("/admin/plugin-bundled/{id}/approve"),
            discard_action: None,
            bundled_sha256: Some(review.sha256),
            about: About::new(&review.package, true),
            trust_title: "Comes with Tether",
            trust_detail: bundled_trust(id),
            uploaded: None,
            base: review.base,
            source: None,
            source_was: None,
            upgrade,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// What trusting a bundled app means; for Member Audit, the one thing it
/// learns that no other app can.
fn bundled_trust(id: &str) -> String {
    let mut text = "This app is part of Tether: it ships in the same image as Tether itself and \
                    is exactly as trusted, so it isn't signed and pins no key. Nothing else can \
                    install or update an app with this id. Look at what it asks for before \
                    approving, as for any app."
        .to_owned();
    if id == crate::plugin_services::OWNERS_APP {
        text.push_str(
            " Unlike any other app, it learns which characters share an account: for each \
             character it reads (of pilots holding one of its permissions, in any state), the \
             owner's main and state, for its scopes by the owner's main.",
        );
    }
    text
}

/// `GET /admin/plugin-bundled/{id}`: an app that comes with Tether, to
/// review before installing it or upgrading to it.
pub async fn review_bundled(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    bundled_review_page(&state, shell, id, None).await
}

#[derive(Deserialize)]
pub struct ApproveBundledForm {
    /// The bundled package's SHA-256 the review showed.
    #[serde(default)]
    package: String,
    /// What was installed when the review was shown (required here).
    #[serde(default)]
    reviewed: String,
}

/// `POST /admin/plugin-bundled/{id}/approve`
pub async fn approve_bundled(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<ApproveBundledForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let result =
        plugins::approve_bundled(&state, session.account, id, &form.package, form.reviewed).await;
    match result {
        Ok(id) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) if err.status() == StatusCode::NOT_FOUND => {
            list_page(&state, shell, Some(err), &ListParams::default()).await
        }
        Err(err) => bundled_review_page(&state, shell, id, Some(err)).await,
    }
}

// ---- every included update at once ------------------------------------------

#[derive(Template)]
#[template(path = "admin_plugin_bundled_updates.html")]
struct IncludedUpdatesPage {
    shell: Shell,
    apps: Vec<IncludedUpdate>,
    /// How many are approved together: those whose changes are shown.
    bulk: usize,
    snapshots: bool,
    error: Option<String>,
}

/// An included app's update or rebuild waiting for review.
pub struct IncludedUpdate {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
    /// The same version, rebuilt with this Tether.
    pub rebuilt: bool,
    /// `None` when the installed version's package can't be read: then it
    /// isn't approved with the others, only on its own review, which shows
    /// everything it asks for.
    pub changes: Option<Changes>,
    pub new_migrations: usize,
    /// `<id>:<package SHA-256>:<what is installed>`: what its own review
    /// sends back on approval.
    pub token: String,
}

/// Every app installed from Tether's bundle that this Tether carries an
/// update or rebuild of, by name, with what its own review would show. An
/// app installed from a signed package under a bundled id isn't one: moving
/// it to the bundle changes what it's trusted as, which only its own review
/// shows.
async fn included_updates(state: &AppState) -> Result<Vec<IncludedUpdate>, AppError> {
    let mut apps = Vec::new();
    for p in db::list(&state.db).await? {
        if p.origin != db::Origin::Bundled {
            continue;
        }
        let Some(app) = state.plugins.bundled().get(&p.id) else {
            continue;
        };
        let Some(offer) = plugins::bundled_offer(&p.version, p.origin, &p.package_sha256, app)
        else {
            continue;
        };
        let review = plugins::bundled_review(state, &p.id).await?;
        let changes = match &review.installed {
            Some(old) => {
                Some(Changes::counted(state, &old.manifest, &review.package.manifest).await?)
            }
            None => None,
        };
        apps.push(IncludedUpdate {
            token: format!("{}:{}:{}", p.id, review.sha256, review.base),
            name: review.package.manifest.plugin.name.clone(),
            from: review.installed_version.unwrap_or(p.version),
            to: review.package.manifest.plugin.version.clone(),
            rebuilt: offer == plugins::Offer::Rebuilt,
            changes,
            new_migrations: review.new_migrations,
            id: p.id,
        });
    }
    Ok(apps)
}

async fn included_updates_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let apps = included_updates(state).await?;
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &IncludedUpdatesPage {
            shell,
            bulk: apps.iter().filter(|a| a.changes.is_some()).count(),
            apps,
            snapshots: state.plugins.snapshots_on(),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/plugin-bundled-updates`: every update and rebuild of an
/// included app waiting for review, on one page.
pub async fn review_included_updates(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    included_updates_page(&state, shell, None).await
}

/// `POST /admin/plugin-bundled-updates/approve` (`app` repeated, each an
/// [`IncludedUpdate::token`]): approves each in turn exactly as its own
/// review's Approve would, stopping at the first that fails.
pub async fn approve_included_updates(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let chosen: Vec<(&str, &str, &str)> = fields
        .iter()
        .filter(|(key, _)| key == "app")
        .filter_map(|(_, token)| {
            let mut parts = token.splitn(3, ':');
            Some((parts.next()?, parts.next()?, parts.next()?))
        })
        .collect();
    if chosen.is_empty() {
        let err = AppError::bad_request("Nothing was chosen to approve. Look again.");
        return included_updates_page(&state, shell, Some(err)).await;
    }
    // Only what this page offers now, each once, all checked before any
    // is approved: updates of apps installed from the bundle whose changes
    // it can show. Anything else has its own review.
    let offered = included_updates(&state).await?;
    let mut seen: Vec<&str> = Vec::new();
    for (id, _, _) in &chosen {
        let fits = offered.iter().any(|a| a.id == *id && a.changes.is_some());
        if !fits || seen.contains(id) {
            let err = AppError::bad_request(
                "What's waiting changed since this page was shown. Look again before approving.",
            );
            return included_updates_page(&state, shell, Some(err)).await;
        }
        seen.push(id);
    }
    let mut done: Vec<String> = Vec::new();
    for (i, (id, sha256, reviewed)) in chosen.iter().enumerate() {
        let name = offered
            .iter()
            .find(|a| a.id == *id)
            .map_or_else(|| (*id).to_owned(), |a| a.name.clone());
        let result =
            plugins::approve_bundled(&state, session.account, id, sha256, (*reviewed).to_owned())
                .await;
        if let Err(err) = result {
            let mut message = String::new();
            if !done.is_empty() {
                message.push_str(&format!("Upgraded: {}. ", done.join(", ")));
            }
            message.push_str(&format!("{name} wasn't: {}", err.message()));
            if i + 1 < chosen.len() {
                message.push_str(" The ones after it weren't approved.");
            }
            let err = AppError::new(err.status(), message);
            return included_updates_page(&state, shell, Some(err)).await;
        }
        done.push(name);
    }
    Ok(Redirect::to("/admin/plugins").into_response())
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
        Err(err) => list_page(&state, shell, Some(err), &ListParams::default()).await,
    }
}

// ---- one plugin -------------------------------------------------------------

pub struct ChannelView {
    pub id: i64,
    pub name: String,
}

pub struct SecretView {
    pub name: String,
    pub host: String,
    pub header: String,
    /// When it was last entered, or `None` if it has no value yet.
    pub set: Option<String>,
}

#[derive(Template)]
#[template(path = "admin_plugin.html")]
struct PluginPage {
    shell: Shell,
    /// It runs and has permissions, but none is granted to anyone: only
    /// superusers can use it, or (`open_to_all`) do what they allow.
    ungranted: bool,
    /// Some of its pages are open to every signed-in pilot, no grant
    /// needed.
    open_to_all: bool,
    channels: Vec<ChannelView>,
    free_channels: Vec<ChannelView>,
    uses_discord: bool,
    esi_scopes: Vec<String>,
    http_hosts: Vec<String>,
    /// Declared by the running package, but never approved: refused.
    http_unapproved: Vec<String>,
    http_secrets: Vec<SecretView>,
    /// It runs: its data sources and activity are under its Manage.
    running: bool,
    /// It reads ESI through data sources.
    has_sources: bool,
    /// While it doesn't run (stopped, failed, or not fitting this Tether):
    /// its data sources and activity here, so disabling an app keeps what
    /// it read and sent in view, and its sources can still be removed.
    owners: Option<tether_web_core::pages::plugin_access::Owners>,
    activity: Option<tether_web_core::pages::plugin_activity::Activity>,
    about: About,
    enabled: bool,
    /// It has a settings page (`settings`): opened from here, not from
    /// the app's own header.
    settings: bool,
    status: &'static str,
    /// The status line's tone.
    tone: &'static str,
    failure: Option<String>,
    /// It failed to load because it doesn't fit this Tether's app
    /// interface; the error, for admins.
    incompatible: Option<String>,
    installed: String,
    rollback: Option<RollbackView>,
    updates: UpdatesView,
    /// It comes with Tether: its updates do too.
    included: Option<IncludedView>,
    error: Option<String>,
}

/// An app that comes with Tether: the version this Tether carries.
pub struct IncludedView {
    pub version: String,
    /// Newer than the one installed.
    pub newer: bool,
    /// The same version, rebuilt with this Tether.
    pub rebuilt: bool,
}

/// Where an app's updates come from, and the last check.
pub struct UpdatesView {
    /// `owner/name`.
    pub source: Option<String>,
    pub latest: Option<String>,
    pub newer: bool,
    pub release_url: Option<String>,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    /// Update checks are switched on.
    pub checks_on: bool,
    /// Installing from GitHub is set up here.
    pub github: bool,
}

/// Going back to the version an upgrade replaced.
pub struct RollbackView {
    pub from: String,
    pub to: String,
    pub upgraded_at: String,
    /// When the snapshot its data goes back to was taken.
    pub restore: Option<String>,
    pub deletes_data: bool,
    pub blocked: Option<String>,
    /// What it asks for goes back to what the earlier version asked for.
    pub changes: Option<Changes>,
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
    let (label, tone) = status_label(&status, installed.enabled);
    let sources = tether_db::plugin_sources::status(&state.db, id).await?;
    let release_url = match (state.plugins.github(), &sources.source, &sources.latest_url) {
        (Some(github), Some(repo), Some(url)) => github.release_link(repo, url),
        _ => None,
    };
    let updates = UpdatesView {
        newer: sources
            .latest_version
            .as_deref()
            .is_some_and(|l| newer(l, &installed.version)),
        source: sources.source,
        latest: sources.latest_version,
        release_url,
        checked_at: sources.checked_at.map(time),
        error: sources.check_error,
        checks_on: crate::updates::enabled(&state.db).await?,
        github: state.plugins.github().is_some(),
    };
    let included = state.plugins.bundled().get(id).map(|app| {
        let offer = plugins::bundled_offer(
            &installed.version,
            installed.origin,
            &installed.package_sha256,
            app,
        );
        IncludedView {
            newer: offer == Some(plugins::Offer::Newer),
            rebuilt: offer == Some(plugins::Offer::Rebuilt),
            version: app.package.manifest.plugin.version.clone(),
        }
    });
    let plan = plugins::rollback_plan(state, id).await?;
    let rollback_changes = match plan.as_ref().map(|p| (&p.current, &p.earlier)) {
        Some((Some(current), Some(earlier))) => {
            Some(Changes::counted(state, &current.manifest, &earlier.manifest).await?)
        }
        _ => None,
    };
    let rollback = plan.map(|plan| RollbackView {
        changes: rollback_changes,
        from: plan.from,
        to: plan.to,
        upgraded_at: time(plan.upgraded_at),
        restore: plan.restore.map(|s| time(s.header.taken_at)),
        deletes_data: plan.deletes_data,
        blocked: plan.blocked,
    });
    // While it runs, its data sources and activity are under its Manage.
    let running = matches!(status, Status::Running);
    let (owners, activity) = if running {
        (None, None)
    } else {
        use tether_web_core::pages::{plugin_access, plugin_activity};
        let mut owners = plugin_access::for_admin(state, &package.manifest);
        if let Some(owners) = owners.as_mut() {
            plugin_access::load(state, owners, None, true).await?;
        }
        (
            owners,
            Some(plugin_activity::activity(state, id, false).await?),
        )
    };
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
    let approved = tether_db::plugin_http::approved(&state.db, id).await?;
    let http_unapproved = package
        .manifest
        .capabilities
        .http
        .iter()
        .filter(|h| !approved.hosts.contains(h))
        .cloned()
        .collect();
    let set = tether_db::plugin_http::secrets_set(&state.db, id).await?;
    let http_secrets = approved
        .secrets
        .iter()
        .map(|s| SecretView {
            name: s.name.clone(),
            host: s.host.clone(),
            header: s.header.clone(),
            set: set
                .iter()
                .find(|(name, _)| *name == s.name)
                .map(|(_, at)| time(*at)),
        })
        .collect();
    let prefix = format!("plugin.{id}.");
    let open_to_all = package
        .manifest
        .pages
        .iter()
        .any(|rule| rule.permission.is_none());
    let ungranted = running
        && !package.manifest.permissions.is_empty()
        && !tether_db::permissions::list(&state.db)
            .await?
            .iter()
            .any(|g| g.permission.starts_with(&prefix));
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &PluginPage {
            shell,
            ungranted,
            open_to_all,
            http_hosts: approved.hosts,
            http_unapproved,
            http_secrets,
            running,
            has_sources: !package.manifest.capabilities.esi.data_source.is_empty(),
            owners,
            activity,
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
            about: About::new(&package, installed.origin == db::Origin::Bundled),
            enabled: installed.enabled,
            // Only a running app has a settings page to open.
            settings: matches!(status, Status::Running)
                && package
                    .manifest
                    .pages
                    .iter()
                    .any(|p| p.path == tether_plugins::manifest::SETTINGS_PATH),
            status: label,
            tone,
            failure: if plugins::sha256(&installed.package) != installed.package_sha256 {
                Some(
                    "The stored package isn't the one that was approved, so it won't load. \
                     What's shown below is what it contains now."
                        .to_owned(),
                )
            } else {
                match &status {
                    Status::Failed(why) => Some(why.clone()),
                    _ => None,
                }
            },
            incompatible: match status {
                Status::Incompatible(why) => Some(why),
                _ => None,
            },
            installed: time(installed.installed_at),
            rollback,
            updates,
            included,
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

/// `POST /admin/plugins/{id}/schedules/{name}/run`: one of the app's
/// schedules, now.
pub async fn run_schedule(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: axum::http::HeaderMap,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let full = tether_db::plugin_jobs::schedule_name(id, &name);
    let result = super::system::run_schedule(&state, session.account, &full).await;
    if super::is_htmx(&headers) {
        return Ok(super::system::run_now_fragment(&result));
    }
    match result {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/plugins/{id}/rollback`
pub async fn roll_back(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match plugins::roll_back(&state, session.account, id, &form.confirmation).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

// ---- GitHub -----------------------------------------------------------------

/// The one upload slot, or the list page saying it's taken.
fn busy() -> AppError {
    AppError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "Another upload is being checked. Try again in a moment.",
    )
}

#[derive(Deserialize)]
pub struct GitHubForm {
    #[serde(default)]
    repo: String,
    /// For a repository publishing several apps.
    #[serde(default)]
    app: String,
}

/// `POST /admin/plugin-github`: fetch an app's newest release for review.
pub async fn install_github(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<GitHubForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let Some(_permit) = state.plugins.upload_permit() else {
        let err = plugins::reject(&state, session.account, None, 0, busy()).await;
        return list_page(&state, shell, Some(err), &ListParams::default()).await;
    };
    let app = form.app.trim();
    let result = match crate::plugin_github::Repo::parse(&form.repo) {
        None => Err(AppError::bad_request(
            "That isn't a GitHub repository: use https://github.com/<owner>/<name>.",
        )),
        Some(_) if !app.is_empty() && manifest::check_id(app).is_err() => Err(
            AppError::bad_request("That isn't an app id (such as acme.moon-mining)."),
        ),
        Some(repo) => {
            let app = (!app.is_empty()).then_some(app);
            crate::plugin_github::fetch(&state, session.account, &repo, app, None).await
        }
    };
    match result {
        Ok(id) => Ok(Redirect::to(&format!("/admin/plugin-uploads/{id}")).into_response()),
        Err(err) => list_page(&state, shell, Some(err), &ListParams::default()).await,
    }
}

/// `POST /admin/plugins/{id}/update`: fetch the newer version its
/// repository publishes, for review.
pub async fn update(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let installed = db::get(&state.db, id)
        .await?
        .ok_or_else(|| AppError::not_found("No app with that id is installed."))?;
    let source = tether_db::plugin_sources::status(&state.db, id)
        .await?
        .source;
    let Some(repo) = source
        .as_deref()
        .and_then(crate::plugin_github::Repo::parse)
    else {
        let err = AppError::bad_request("Set the repository its updates come from first.");
        return plugin_page(&state, shell, id, Some(err)).await;
    };
    let Some(_permit) = state.plugins.upload_permit() else {
        let err = plugins::reject(&state, session.account, Some(id), 0, busy()).await;
        return plugin_page(&state, shell, id, Some(err)).await;
    };
    let fetched = crate::plugin_github::fetch(
        &state,
        session.account,
        &repo,
        Some(id),
        Some(&installed.version),
    )
    .await;
    match fetched {
        Ok(upload) => Ok(Redirect::to(&format!("/admin/plugin-uploads/{upload}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

#[derive(Deserialize)]
pub struct SourceForm {
    #[serde(default)]
    source: String,
}

/// `POST /admin/plugins/{id}/source`: where its updates come from.
pub async fn set_source(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    Form(form): Form<SourceForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    match crate::plugin_github::set_source(&state, session.account, id, &form.source).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
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

// ---- secrets -------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SecretForm {
    #[serde(default)]
    value: String,
}

// The value never reaches a log or an error.
impl std::fmt::Debug for SecretForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretForm")
            .field("value", &"[redacted]")
            .finish()
    }
}

/// `POST /admin/plugins/{id}/secrets/{name}`: enters or replaces a secret's
/// value, which is never shown again.
pub async fn set_secret(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, name)): Path<(String, String)>,
    Form(form): Form<SecretForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PLUGINS, "plugins").await?;
    let id = plugin_id(&id)?;
    let value = tether_core::Secret::new(form.value);
    match crate::plugin_http::set_secret(&state, session.account, id, &name, &value).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/plugins/{id}")).into_response()),
        Err(err) => plugin_page(&state, shell, id, Some(err)).await,
    }
}

// ---- data sources and channels ---------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use tether_plugins::manifest::Manifest;

    fn manifest(permissions: &str) -> Manifest {
        Manifest::parse(&format!(
            "[plugin]\nid = \"acme.mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
             [capabilities.esi]\ndata_source = [\"esi-industry.read_corporation_mining.v1\"]\n\n\
             [permissions]\n{permissions}\n"
        ))
        .unwrap()
    }

    fn owners_line(m: &Manifest) -> String {
        capabilities(m)
            .into_iter()
            .find(|c| c.title.contains("data sources"))
            .map(|c| c.detail)
            .unwrap_or_default()
    }

    #[test]
    fn the_review_says_who_adds_data_sources() {
        let with = manifest("manage = \"Manage\"\nadd_refinery_owner = \"Can add refinery owner\"");
        let line = owners_line(&with);
        assert!(
            line.contains("holders of Can add refinery owner (add_refinery_owner) and app admins"),
            "{line}"
        );
        assert!(!line.contains("(manage)"), "{line}");
        let none = manifest("manage = \"Manage\"");
        assert!(owners_line(&none).contains("app admins only"));
    }
}
