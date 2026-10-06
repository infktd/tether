//! An app's data sources (AA's owners, added with its Add Owner), around
//! the app's own pages: the host draws an "Add data source" button in the
//! page header for those who may add a character as the app's data source
//! (`plugin_consent::may_offer`), and on the app's main page its data
//! sources: every one, with Remove, for app admins (`admin.plugins`); the
//! viewer's own, with Withdraw, for everyone else. Data sources are in use
//! once added, as in AA: nobody approves them.
//! Plugins never see any of it.

use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::permissions::ADMIN_PLUGINS;
use tether_db::accounts::AccountId;
use tether_db::plugin_esi;

use super::PageError;
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugin_consent;
use crate::plugins::Running;

/// One data source, as the owners card shows it.
pub struct OwnerRow {
    pub character_id: i64,
    pub name: String,
    pub corporation_id: i64,
    pub corporation: String,
    pub offered_by: String,
    pub when: String,
    /// active, suspended or moved.
    pub state: &'static str,
    /// On the viewer's own account (they may withdraw it).
    pub own: bool,
}

/// A source withdrawn or removed lately.
pub struct GoneRow {
    pub name: String,
    pub what: String,
    pub when: String,
}

/// What an app's page shows of its owners, for apps with data sources.
pub struct Owners {
    pub plugin_id: String,
    /// "Add data source" in the header.
    pub can_offer: bool,
    /// Sees every source, with Remove (app admins).
    pub can_manage: bool,
    /// The owners card (the app's main page, for those with something in
    /// it).
    pub panel: bool,
    pub scopes: Vec<String>,
    pub rows: Vec<OwnerRow>,
    pub gone: Vec<GoneRow>,
}

fn time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// `active` (in use), `suspended` (its account is deactivated or
/// blacklisted, or the character left the account that added it), or
/// `moved` (it changed corporation since it was added, or its corporation
/// wasn't known: adding it again brings it up to date).
pub fn source_state(d: &plugin_esi::DataSource) -> &'static str {
    if d.in_use() {
        "active"
    } else if !d.account_ok {
        "suspended"
    } else {
        "moved"
    }
}

/// The owners of `running`'s app for this viewer (`perms` their
/// effective permissions), or `None` if the app has no data sources. The
/// sources themselves are read only for the app's main page.
pub async fn owners(
    state: &AppState,
    session: &CurrentSession,
    running: &Running,
    perms: &BTreeSet<String>,
    main_page: bool,
) -> Result<Option<Owners>, AppError> {
    let manifest = &running.manifest;
    if manifest.capabilities.esi.data_source.is_empty() {
        return Ok(None);
    }
    let holds = |p: &str| {
        perms.contains(p)
            && session
                .token_scopes
                .as_ref()
                .is_none_or(|scopes| scopes.contains(p))
    };
    let id = manifest.plugin.id.clone();
    let mut owners = Owners {
        can_offer: session.token_scopes.is_none()
            && plugin_consent::may_offer(manifest, |p| holds(p)),
        can_manage: holds(ADMIN_PLUGINS),
        panel: false,
        scopes: manifest.capabilities.esi.data_source.clone(),
        rows: Vec::new(),
        gone: Vec::new(),
        plugin_id: id,
    };
    if !main_page {
        return Ok(Some(owners));
    }
    let mine: Vec<i64> = plugin_esi::account_characters(&state.db, session.account)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    let sources: Vec<plugin_esi::DataSource> =
        plugin_esi::data_sources(&state.db, &owners.plugin_id)
            .await?
            .into_iter()
            .filter(|d| owners.can_manage || mine.contains(&d.character.id))
            .collect();
    let ids: Vec<i64> = sources
        .iter()
        .filter_map(|d| d.character.corporation_id)
        .collect();
    let names = tether_db::compliance::cached_names(&state.db, &ids).await?;
    owners.rows = sources
        .into_iter()
        .map(|d| {
            let corporation_id = d.character.corporation_id.unwrap_or(0);
            OwnerRow {
                state: source_state(&d),
                own: mine.contains(&d.character.id),
                character_id: d.character.id,
                corporation: names
                    .get(&corporation_id)
                    .cloned()
                    .unwrap_or_else(|| "Unknown corporation".to_owned()),
                corporation_id,
                name: d.character.name,
                offered_by: d.offered_by.unwrap_or_else(|| "Someone".to_owned()),
                when: time(d.offered_at),
            }
        })
        .collect();
    if owners.can_manage {
        owners.gone = plugin_esi::gone_data_sources(&state.db, &owners.plugin_id)
            .await?
            .into_iter()
            .map(gone_row)
            .collect();
    }
    owners.panel = owners.can_offer || owners.can_manage || !owners.rows.is_empty();
    Ok(Some(owners))
}

pub fn gone_row(g: plugin_esi::GoneSource) -> GoneRow {
    let who = g.actor_name.unwrap_or_else(|| "Someone".to_owned());
    GoneRow {
        name: g
            .character_name
            .unwrap_or_else(|| "Unknown character".to_owned()),
        what: if g.action.ends_with("withdrawn") {
            format!("Withdrawn by {who}")
        } else {
            format!("Removed by {who}")
        },
        when: time(g.at),
    }
}

/// A plugin id from the path, checked (anything else can't exist).
fn plugin_id(id: &str) -> Result<&str, AppError> {
    tether_plugins::manifest::check_id(id)
        .map(|()| id)
        .map_err(|_| AppError::not_found("No such app."))
}

/// Back to the app's page, or the Dashboard if it isn't running, with a
/// toast saying what happened.
fn back(state: &AppState, id: &str, message: &str) -> Response {
    if state.plugins.running(id).is_some() {
        super::stay::back(&format!("/plugins/{id}"), message)
    } else {
        super::stay::back("/dashboard", message)
    }
}

/// Add owner's form: the app page it was on (`back`, a link path), which
/// the login comes back to. The header's button leaves it out: the app's
/// main page.
#[derive(Debug, Default, Deserialize)]
pub struct AddForm {
    #[serde(default)]
    back: String,
}

/// `POST /apps/{id}/owners/add`: Add owner, off to EVE SSO to log in with
/// the character to add; back on the app's page afterwards.
pub async fn add(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Path(id): Path<String>,
    axum::Form(form): axum::Form<AddForm>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    // A browser's login: an access token can't go to EVE.
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    let id = plugin_id(&id)?;
    Ok(plugin_consent::start_offer(&state, jar, session.account, id, &form.back).await?)
}

/// `POST /apps/{id}/owners/{character}/withdraw`: the character's owner
/// withdraws it. Always allowed for
/// one's own characters.
pub async fn withdraw(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
    Query(from): Query<WithdrawFrom>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let id = plugin_id(&id)?;
    plugin_consent::withdraw_offer(&state, session.account, id, character).await?;
    if from.from.as_deref() == Some("tokens") {
        return Ok(super::stay::back("/tokens", "Withdrawn."));
    }
    Ok(back(&state, id, "Withdrawn."))
}

/// Where a withdrawal came from: `?from=tokens` is Token Management.
#[derive(Debug, Default, Deserialize)]
pub struct WithdrawFrom {
    #[serde(default)]
    from: Option<String>,
}

/// One of the account's characters that is an app's owner, for Token
/// Management: wherever else they are, pilots always see what their
/// characters are used for, and can withdraw them.
pub struct OwnSource {
    pub plugin_id: String,
    pub plugin_name: String,
    pub character_id: i64,
    pub name: String,
    /// active, suspended or moved.
    pub state: &'static str,
}

/// Every running app's sources that are the account's characters.
pub async fn own_sources(state: &AppState, account: AccountId) -> Result<Vec<OwnSource>, AppError> {
    let mine: Vec<i64> = plugin_esi::account_characters(&state.db, account)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    let mut out = Vec::new();
    for running in state.plugins.all_running() {
        if running.manifest.capabilities.esi.data_source.is_empty() {
            continue;
        }
        let id = &running.manifest.plugin.id;
        for d in plugin_esi::data_sources(&state.db, id).await? {
            if mine.contains(&d.character.id) {
                out.push(OwnSource {
                    plugin_id: id.clone(),
                    plugin_name: running.manifest.plugin.name.clone(),
                    character_id: d.character.id,
                    state: source_state(&d),
                    name: d.character.name,
                });
            }
        }
    }
    Ok(out)
}

/// An app admin (`admin.plugins`).
async fn app_admin(
    state: &AppState,
    session: Option<CurrentSession>,
) -> Result<AccountId, AppError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(state, ADMIN_PLUGINS).await?;
    Ok(session.account)
}

/// `POST /apps/{id}/owners/{character}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let admin = app_admin(&state, session).await?;
    let id = plugin_id(&id)?;
    plugin_consent::remove_source_as_admin(&state, admin, id, character).await?;
    Ok(back(&state, id, "Removed."))
}
