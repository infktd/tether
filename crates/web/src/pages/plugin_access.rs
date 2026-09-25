//! The profile page's plugin section: which plugins may read what with
//! your characters (consent to user scopes, revocable), and which of your
//! characters you've offered as a plugin's data source.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use tether_db::plugin_esi;

use super::PageError;
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugin_consent;

pub struct CharacterRef {
    pub id: i64,
    pub name: String,
    pub approved: bool,
}

/// One plugin that asks for ESI access, as the profile shows it.
pub struct PluginAccess {
    pub id: String,
    pub name: String,
    pub user_scopes: Vec<String>,
    pub consented: Vec<CharacterRef>,
    pub source_scopes: Vec<String>,
    pub offered: Vec<CharacterRef>,
}

/// Running plugins that use ESI, with this account's consents and offers.
pub async fn for_profile(
    state: &AppState,
    session: &CurrentSession,
) -> Result<Vec<PluginAccess>, AppError> {
    let account = tether_db::accounts::get(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let name_of = |id: i64| {
        account
            .characters
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
    };
    let consents = plugin_esi::account_consents(&state.db, session.account).await?;
    let mut out = Vec::new();
    for running in state.plugins.all_running() {
        let esi = &running.manifest.capabilities.esi;
        if esi.user.is_empty() && esi.data_source.is_empty() {
            continue;
        }
        let id = running.manifest.plugin.id.clone();
        let consented = consents
            .iter()
            .filter(|(plugin, _)| *plugin == id)
            .filter_map(|(_, character)| {
                Some(CharacterRef {
                    id: *character,
                    name: name_of(*character)?,
                    approved: true,
                })
            })
            .collect();
        let offered = if esi.data_source.is_empty() {
            Vec::new()
        } else {
            plugin_esi::data_sources(&state.db, &id)
                .await?
                .into_iter()
                .filter_map(|d| {
                    Some(CharacterRef {
                        name: name_of(d.character.id)?,
                        id: d.character.id,
                        approved: d.approved,
                    })
                })
                .collect()
        };
        out.push(PluginAccess {
            name: running.manifest.plugin.name.clone(),
            user_scopes: esi.user.clone(),
            source_scopes: esi.data_source.clone(),
            consented,
            offered,
            id,
        });
    }
    Ok(out)
}

/// A plugin id from the path, and the plugin must be running.
fn plugin(state: &AppState, id: &str) -> Result<String, PageError> {
    tether_plugins::manifest::check_id(id)
        .ok()
        .and_then(|()| state.plugins.running(id))
        .map(|r| r.manifest.plugin.id.clone())
        .ok_or_else(|| AppError::not_found("No such plugin is running.").into())
}

/// `POST /profile/plugins/{id}/consent`: off to EVE SSO for its scopes.
pub async fn consent(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let id = plugin(&state, &id)?;
    Ok(plugin_consent::start_consent(&state, jar, session.account, &id).await?)
}

/// `POST /profile/plugins/{id}/consent/{character}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    tether_plugins::manifest::check_id(&id).map_err(|_| AppError::not_found("No such plugin."))?;
    plugin_consent::revoke_consent(&state, session.account, &id, character).await?;
    Ok(Redirect::to("/profile").into_response())
}

/// `POST /profile/plugins/{id}/offer`: off to EVE SSO to link a character
/// as the plugin's data source.
pub async fn offer(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let id = plugin(&state, &id)?;
    Ok(plugin_consent::start_offer(&state, jar, session.account, &id).await?)
}

/// `POST /profile/plugins/{id}/offer/{character}/withdraw`
pub async fn withdraw(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, character)): Path<(String, i64)>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    tether_plugins::manifest::check_id(&id).map_err(|_| AppError::not_found("No such plugin."))?;
    plugin_consent::withdraw_offer(&state, session.account, &id, character).await?;
    Ok(Redirect::to("/profile").into_response())
}
