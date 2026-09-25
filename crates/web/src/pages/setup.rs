//! The first-run wizard's pages. They call the same logic as the JSON API
//! (`crate::setup`, `crate::api::admin`).

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::permissions::ADMIN_STATES;
use tether_core::states::{Builtin, EntityKind};
use tether_db::audit::Actor;
use tether_db::settings;

use super::{PageError, render};
use crate::AppState;
use crate::admin::esi_unavailable;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::setup::{
    ClientIp, SetupState, SetupStatus, Suggestion, check_public_url, load_status, save_client_id,
    unlock_session,
};
use crate::state_admin::Change;

pub enum SetupView {
    Token,
    Sso,
    Owner,
    Alliance,
    NeedOwnerLogin,
    Complete,
}

pub struct Step {
    pub label: &'static str,
    /// `done`, `current` or `upcoming`.
    pub state: &'static str,
}

#[derive(Template)]
#[template(path = "setup.html")]
struct SetupPage {
    steps: Vec<Step>,
    view: SetupView,
    callback_url: String,
    client_id: String,
    suggested: Option<Suggestion>,
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SetupQuery {
    /// `sso` reopens the client id step before an owner exists.
    change: Option<String>,
}

/// `GET /setup`
pub async fn page(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
    Query(query): Query<SetupQuery>,
) -> Result<Response, PageError> {
    render_page(
        &state,
        &jar,
        session.as_ref(),
        query.change.as_deref() == Some("sso"),
        None,
        StatusCode::OK,
    )
    .await
}

async fn render_page(
    state: &AppState,
    jar: &CookieJar,
    session: Option<&CurrentSession>,
    change_sso: bool,
    error: Option<&AppError>,
    status: StatusCode,
) -> Result<Response, PageError> {
    let s = load_status(state, jar, session).await?;
    let can_manage_states = match session {
        Some(session) => session.require(state, ADMIN_STATES).await.is_ok(),
        None => false,
    };
    let view = choose_view(&s, change_sso, can_manage_states);
    let client_id = settings::get_string(&state.db, settings::SSO_CLIENT_ID)
        .await?
        .unwrap_or_default();
    let page = SetupPage {
        steps: steps(&s),
        view,
        callback_url: s.callback_url,
        client_id,
        suggested: s.suggested,
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

fn choose_view(s: &SetupStatus, change_sso: bool, can_manage_states: bool) -> SetupView {
    match s.state {
        _ if !s.owner_exists && !s.unlocked => SetupView::Token,
        SetupState::NeedsSso => SetupView::Sso,
        SetupState::NeedsOwner if change_sso => SetupView::Sso,
        SetupState::NeedsOwner => SetupView::Owner,
        SetupState::NeedsAlliance if can_manage_states => SetupView::Alliance,
        SetupState::NeedsAlliance => SetupView::NeedOwnerLogin,
        SetupState::Complete => SetupView::Complete,
    }
}

fn steps(s: &SetupStatus) -> Vec<Step> {
    let done = [
        s.owner_exists || s.unlocked,
        s.sso_configured,
        s.owner_exists,
        matches!(s.state, SetupState::Complete),
    ];
    let current = done.iter().position(|d| !d);
    [
        "Setup token",
        "EVE application",
        "Owner login",
        "Member alliance",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, label)| Step {
        label,
        state: if done[i] {
            "done"
        } else if Some(i) == current {
            "current"
        } else {
            "upcoming"
        },
    })
    .collect()
}

/// Re-renders the wizard with the error, keeping its status code.
async fn with_error(
    state: &AppState,
    jar: &CookieJar,
    session: Option<&CurrentSession>,
    err: AppError,
) -> Result<Response, PageError> {
    let status = err.status();
    render_page(state, jar, session, false, Some(&err), status).await
}

#[derive(Debug, Deserialize)]
pub struct TokenForm {
    token: String,
}

/// `POST /setup/unlock`
pub async fn unlock(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Form(form): Form<TokenForm>,
) -> Result<Response, PageError> {
    match unlock_session(&state, ip, &form.token).await {
        Ok(cookie) => Ok((jar.add(cookie), Redirect::to("/setup")).into_response()),
        Err(err) => with_error(&state, &jar, None, err).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct SsoForm {
    client_id: String,
}

/// `POST /setup/sso`
pub async fn sso(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
    Form(form): Form<SsoForm>,
) -> Result<Response, PageError> {
    match save_client_id(&state, &jar, session.as_ref(), &form.client_id).await {
        Ok(()) => Ok(Redirect::to("/setup").into_response()),
        Err(err) => with_error(&state, &jar, session.as_ref(), err).await,
    }
}

#[derive(Template)]
#[template(path = "setup_check.html")]
struct CheckFragment {
    ok: bool,
    detail: String,
}

/// `POST /setup/check` (htmx): does the public URL reach this instance?
pub async fn check(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    // A fragment either way: a redirect would swap a whole page into the
    // result slot.
    if let Err(err) = crate::setup::setup_actor(&state, &jar, session.as_ref()).await {
        let fragment = CheckFragment {
            ok: false,
            detail: err.message().to_owned(),
        };
        return Ok(render(err.status(), &fragment));
    }
    let (ok, detail) = check_public_url(state.site.public_url())
        .await
        .map_err(AppError::internal)?;
    Ok(render(StatusCode::OK, &CheckFragment { ok, detail }))
}

pub struct SearchResult {
    pub id: i64,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Template)]
#[template(path = "setup_search.html")]
struct SearchFragment {
    results: Vec<SearchResult>,
    empty_message: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct SearchForm {
    name: String,
}

/// `POST /setup/alliance/search` (htmx): exact-name lookup via ESI.
pub async fn search(
    State(state): State<AppState>,
    session: CurrentSession,
    Form(form): Form<SearchForm>,
) -> Result<Response, PageError> {
    session.require(&state, ADMIN_STATES).await?;
    let name = form.name.trim().to_owned();
    if name.is_empty() {
        return Err(AppError::bad_request("Enter a name.").into());
    }
    let resolved = state
        .esi
        .resolve_names(&[name], tether_esi::Priority::Interactive)
        .await
        .map_err(esi_unavailable)?;
    let mut results: Vec<SearchResult> = resolved
        .alliances
        .into_iter()
        .map(|e| SearchResult {
            id: e.id,
            name: e.name,
            kind: EntityKind::Alliance.as_str(),
        })
        .collect();
    results.extend(resolved.corporations.into_iter().map(|e| SearchResult {
        id: e.id,
        name: e.name,
        kind: EntityKind::Corporation.as_str(),
    }));
    Ok(render(
        StatusCode::OK,
        &SearchFragment {
            results,
            empty_message: "No alliance or corporation has exactly that name.",
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct AllianceForm {
    entity_id: i64,
}

/// `POST /setup/alliance`: make it Member.
pub async fn choose_alliance(
    State(state): State<AppState>,
    jar: CookieJar,
    session: CurrentSession,
    Form(form): Form<AllianceForm>,
) -> Result<Response, PageError> {
    session.require(&state, ADMIN_STATES).await?;
    let result = async {
        let member = tether_db::states::builtin(&state.db, Builtin::Member).await?;
        let change = Change::Add {
            state: member.id,
            entity_id: form.entity_id,
        };
        crate::state_admin::apply(
            &state.db,
            &state.esi,
            Actor::Account(session.account),
            &change,
        )
        .await
    }
    .await;
    match result {
        Ok(_) => Ok(Redirect::to("/setup").into_response()),
        Err(err) => with_error(&state, &jar, Some(&session), err).await,
    }
}
