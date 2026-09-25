//! The States page (F4, DESIGN.md "Configuration pages"): every state as a
//! card in priority order, what it covers as chips, live counts, and a
//! confirmation listing who moves before any change that moves accounts.
//! Plain forms; htmx boosts them.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::ADMIN_STATES;
use tether_core::states::{Builtin, EntityKind, StateId};
use tether_db::audit::Actor;
use tether_db::states as db;

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::admin::esi_unavailable;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::state_admin::{self, Change, Move};

/// CCP's image for an entity, 64px shown at 20.
fn image_url(kind: EntityKind, id: i64) -> String {
    match kind {
        EntityKind::Alliance => format!("https://images.evetech.net/alliances/{id}/logo?size=64"),
        EntityKind::Corporation => {
            format!("https://images.evetech.net/corporations/{id}/logo?size=64")
        }
        EntityKind::Character => {
            format!("https://images.evetech.net/characters/{id}/portrait?size=64")
        }
    }
}

pub struct Chip {
    pub entity_id: i64,
    pub kind: &'static str,
    pub name: String,
    pub image: String,
}

pub struct Card {
    pub id: i64,
    pub name: String,
    pub style: &'static str,
    pub builtin: bool,
    pub priority: i32,
    pub guest: bool,
    pub accounts: i64,
    pub covers: Vec<Chip>,
    pub can_up: bool,
    pub can_down: bool,
    /// What the state means, in terms of pilots.
    pub about: String,
    /// Required because installed plugins read them (Member only).
    pub plugin_scopes: Vec<ScopeChip>,
    /// Required because an admin added them.
    pub admin_scopes: Vec<ScopeChip>,
    /// Character scopes an admin could still add.
    pub scope_options: Vec<ScopeChip>,
}

pub struct ScopeChip {
    pub scope: String,
    pub description: String,
    /// Which plugins need it (plugin scopes only).
    pub by: String,
}

fn chip(scope: &str, by: String) -> ScopeChip {
    ScopeChip {
        scope: scope.to_owned(),
        description: tether_core::scopes::describe(scope).to_owned(),
        by,
    }
}

#[derive(Template)]
#[template(path = "admin_states.html")]
struct StatesPage {
    shell: Shell,
    cards: Vec<Card>,
    /// Every scope Tether may ask for: all must be enabled on the EVE
    /// application.
    application_scopes: Vec<String>,
    error: Option<String>,
}

async fn states_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let states = db::list(&state.db).await?;
    let covered = db::covered(&state.db).await?;
    let counts = db::counts(&state.db).await?;
    let admin_scopes = tether_db::compliance::all_admin_scopes(&state.db).await?;
    let plugins = tether_db::compliance::plugin_scopes(&state.db).await?;
    let mut plugin_scopes: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for p in &plugins {
        for scope in &p.scopes {
            plugin_scopes
                .entry(scope.as_str())
                .or_default()
                .push(p.name.as_str());
        }
    }
    let mut application_scopes: std::collections::BTreeSet<String> = admin_scopes
        .iter()
        .map(|(_, scope)| scope.clone())
        .chain(plugin_scopes.keys().map(|s| (*s).to_owned()))
        .chain(tether_core::scopes::CORE.iter().map(|s| (*s).to_owned()))
        .collect();
    application_scopes.insert(tether_core::scopes::CORP_MEMBERSHIP.to_owned());
    for running in state.plugins.all_running() {
        application_scopes.extend(
            running
                .manifest
                .capabilities
                .esi
                .data_source
                .iter()
                .cloned(),
        );
    }
    // Guest is last; the one above it can't move down past it.
    let movable = states.iter().filter(|s| !s.is_guest()).count();
    let cards =
        states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                Card {
            id: s.id.0,
            name: s.name.clone(),
            style: s.style(),
            builtin: s.builtin.is_some(),
            priority: s.priority,
            guest: s.is_guest(),
            accounts: counts.get(&s.id).copied().unwrap_or(0),
            covers: covered
                .iter()
                .filter(|c| c.state == s.id)
                .map(|c| Chip {
                    entity_id: c.entity_id,
                    kind: c.kind.as_str(),
                    name: c.name.clone(),
                    image: image_url(c.kind, c.entity_id),
                })
                .collect(),
            can_up: !s.is_guest() && i > 0,
            can_down: !s.is_guest() && i + 1 < movable,
            about: match s.builtin {
                Some(Builtin::Member) => "Your own alliance and corporations.".to_owned(),
                Some(Builtin::Blue) => {
                    "Friends you give more than Guest: allied alliances, corporations or pilots."
                        .to_owned()
                }
                Some(Builtin::Guest) => "Everyone no state above covers. Guests can sign in \
                     and see their profile; nothing else unless you grant it."
                    .to_owned(),
                None => format!("Pilots whose main is covered here are {}.", s.name),
            },
            plugin_scopes: if s.builtin == Some(Builtin::Member) {
                plugin_scopes
                    .iter()
                    .map(|(scope, by)| chip(scope, by.join(", ")))
                    .collect()
            } else {
                Vec::new()
            },
            admin_scopes: admin_scopes
                .iter()
                .filter(|(id, _)| *id == s.id)
                .map(|(_, scope)| chip(scope, String::new()))
                .collect(),
            scope_options: tether_core::scopes::ALL
                .iter()
                .filter(|info| info.kind == tether_core::scopes::ScopeKind::Character)
                .filter(|info| !tether_core::scopes::is_write(info.scope))
                .filter(|info| !admin_scopes.iter().any(|(id, sc)| *id == s.id && sc == info.scope))
                .filter(|info| {
                    s.builtin != Some(Builtin::Member) || !plugin_scopes.contains_key(info.scope)
                })
                .map(|info| chip(info.scope, String::new()))
                .collect(),
        }
            })
            .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &StatesPage {
            shell,
            cards,
            application_scopes: application_scopes.into_iter().collect(),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/states`
pub async fn page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_STATES, "states").await?;
    states_page(&state, shell, None).await
}

#[derive(Template)]
#[template(path = "admin_state_confirm.html")]
struct ConfirmPage {
    shell: Shell,
    summary: String,
    moves: Vec<Move>,
    total: usize,
    action: String,
    fields: Vec<(&'static str, String)>,
}

/// Previews the change; if it moves anyone and isn't confirmed yet, shows
/// who moves where with Apply and Cancel. Otherwise applies it.
async fn change(
    state: &AppState,
    session: Option<CurrentSession>,
    change: Change,
    confirmed: bool,
    action: String,
    fields: Vec<(&'static str, String)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(state, session, ADMIN_STATES, "states").await?;
    if !confirmed {
        let preview = match state_admin::preview(&state.db, &state.esi, &change).await {
            Ok(preview) => preview,
            Err(err) => return states_page(state, shell, Some(err)).await,
        };
        if preview.moves_anyone() {
            let total = preview.moves.iter().map(|m| m.accounts).sum();
            return Ok(render(
                StatusCode::OK,
                &ConfirmPage {
                    shell,
                    summary: preview.summary,
                    moves: preview.moves,
                    total,
                    action,
                    fields,
                },
            ));
        }
    }
    match state_admin::apply(
        &state.db,
        &state.esi,
        Actor::Account(session.account),
        &change,
    )
    .await
    {
        Ok(_) => Ok(Redirect::to("/admin/states").into_response()),
        Err(err) => states_page(state, shell, Some(err)).await,
    }
}

fn is_confirmed(confirm: Option<&str>) -> bool {
    confirm == Some("1")
}

#[derive(Debug, Deserialize)]
pub struct NameForm {
    name: String,
}

/// `POST /admin/states`: a new state (moves nobody: it covers nobody yet).
pub async fn create(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NameForm>,
) -> Result<Response, PageError> {
    let action = "/admin/states".to_owned();
    let fields = vec![("name", form.name.clone())];
    let create = Change::Create { name: form.name };
    change(&state, session, create, true, action, fields).await
}

/// `POST /admin/states/{id}/rename`
pub async fn rename(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<NameForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/rename");
    let fields = vec![("name", form.name.clone())];
    let rename = Change::Rename {
        state: StateId(id),
        name: form.name,
    };
    change(&state, session, rename, true, action, fields).await
}

#[derive(Debug, Deserialize)]
pub struct ConfirmForm {
    confirm: Option<String>,
}

/// `POST /admin/states/{id}/delete`
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/delete");
    let delete = Change::Delete { state: StateId(id) };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, delete, confirmed, action, Vec::new()).await
}

#[derive(Debug, Deserialize)]
pub struct MoveForm {
    direction: String,
    /// The neighbour the admin saw, carried through the confirmation.
    past: Option<i64>,
    confirm: Option<String>,
}

/// `POST /admin/states/{id}/move`
pub async fn move_state(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<MoveForm>,
) -> Result<Response, PageError> {
    let up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => return Err(AppError::bad_request("Move up or down.").into()),
    };
    // Pin the neighbour now, so confirming can't swap with another one.
    let past = match form.past {
        Some(past) => Some(StateId(past)),
        None => {
            let states = db::list(&state.db).await?;
            let at = states.iter().position(|s| s.id.0 == id);
            let other = match (at, up) {
                (Some(at), true) => at.checked_sub(1).and_then(|i| states.get(i)),
                (Some(at), false) => states.get(at + 1),
                (None, _) => None,
            };
            other.map(|s| s.id)
        }
    };
    let action = format!("/admin/states/{id}/move");
    let mut fields = vec![("direction", form.direction.clone())];
    if let Some(past) = past {
        fields.push(("past", past.0.to_string()));
    }
    let moved = Change::Move {
        state: StateId(id),
        up,
        past,
    };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, moved, confirmed, action, fields).await
}

#[derive(Debug, Deserialize)]
pub struct AddForm {
    entity_id: i64,
    confirm: Option<String>,
}

/// `POST /admin/states/{id}/covers`
pub async fn add(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<AddForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/covers");
    let fields = vec![("entity_id", form.entity_id.to_string())];
    let add = Change::Add {
        state: StateId(id),
        entity_id: form.entity_id,
    };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, add, confirmed, action, fields).await
}

/// `POST /admin/states/{id}/covers/{entity_id}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, entity_id)): Path<(i64, i64)>,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/covers/{entity_id}/remove");
    let remove = Change::Remove {
        state: StateId(id),
        entity_id,
    };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, remove, confirmed, action, Vec::new()).await
}

pub struct SearchRow {
    pub id: i64,
    pub name: String,
    pub kind: &'static str,
    pub image: String,
}

#[derive(Template)]
#[template(path = "admin_state_search.html")]
struct SearchFragment {
    state_id: i64,
    state_name: String,
    results: Vec<SearchRow>,
}

#[derive(Debug, Deserialize)]
pub struct SearchForm {
    state_id: i64,
    name: String,
}

/// `POST /admin/states/search` (htmx): exact-name lookup via ESI, with
/// logos, before anything is added.
pub async fn search(
    State(state): State<AppState>,
    session: CurrentSession,
    Form(form): Form<SearchForm>,
) -> Result<Response, PageError> {
    session.require(&state, ADMIN_STATES).await?;
    let target = db::get(&state.db, StateId(form.state_id))
        .await?
        .ok_or_else(|| AppError::not_found("No such state."))?;
    let name = form.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request("Enter a name.").into());
    }
    let resolved = state
        .esi
        .resolve_names(&[name.to_owned()], tether_esi::Priority::Interactive)
        .await
        .map_err(esi_unavailable)?;
    let mut results = Vec::new();
    for (kind, found) in [
        (EntityKind::Alliance, resolved.alliances),
        (EntityKind::Corporation, resolved.corporations),
        (EntityKind::Character, resolved.characters),
    ] {
        results.extend(found.into_iter().map(|e| SearchRow {
            image: image_url(kind, e.id),
            id: e.id,
            name: e.name,
            kind: kind.as_str(),
        }));
    }
    Ok(render(
        StatusCode::OK,
        &SearchFragment {
            state_id: target.id.0,
            state_name: target.name,
            results,
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct ScopeForm {
    scope: String,
    confirm: Option<String>,
}

/// `POST /admin/states/{id}/scopes`: require a scope. Everyone in the state
/// who lacks it becomes Guest until they register again, so it asks first.
pub async fn add_scope(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<ScopeForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/scopes");
    let fields = vec![("scope", form.scope.clone())];
    let add = Change::AddScope {
        state: StateId(id),
        scope: form.scope,
    };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, add, confirmed, action, fields).await
}

/// `POST /admin/states/{id}/scopes/remove`
pub async fn remove_scope(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<ScopeForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/scopes/remove");
    let fields = vec![("scope", form.scope.clone())];
    let remove = Change::RemoveScope {
        state: StateId(id),
        scope: form.scope,
    };
    change(&state, session, remove, true, action, fields).await
}

#[derive(Debug, Deserialize)]
pub struct PriorityForm {
    priority: i32,
    confirm: Option<String>,
}

/// `POST /admin/states/{id}/priority`: AA's editable priority number.
pub async fn set_priority(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<PriorityForm>,
) -> Result<Response, PageError> {
    let action = format!("/admin/states/{id}/priority");
    let fields = vec![("priority", form.priority.to_string())];
    let update = Change::SetPriority {
        state: StateId(id),
        priority: form.priority,
    };
    let confirmed = is_confirmed(form.confirm.as_deref());
    change(&state, session, update, confirmed, action, fields).await
}
