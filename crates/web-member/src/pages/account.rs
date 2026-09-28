//! The account's own pages: where `/` sends a visitor, the login page,
//! the Dashboard, and changing the main.

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_db::{accounts, permissions};

use super::{
    CHARACTER_AUDIT, CharacterRow, DashboardWidget, PageError, Shell, annotate, is_htmx, load,
    render, stay,
};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// `GET /`: send visitors where they belong.
pub async fn home(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Redirect, PageError> {
    if session.is_some() {
        return Ok(Redirect::to("/dashboard"));
    }
    if !accounts::owner_exists(&state.db).await? {
        return Ok(Redirect::to("/setup"));
    }
    Ok(Redirect::to("/login"))
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    setup_complete: bool,
    /// EVE SSO has a client id: until then the page points to the setup
    /// wizard instead of a login that can't work.
    sso_ready: bool,
    /// The site's own name, above the headline and in the tab.
    site_name: Option<String>,
}

/// `GET /login`
pub async fn login(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    if session.is_some() {
        return Ok(Redirect::to("/dashboard").into_response());
    }
    let setup_complete = accounts::owner_exists(&state.db).await?;
    let sso_ready = tether_db::settings::get_string(&state.db, tether_db::settings::SSO_CLIENT_ID)
        .await?
        .is_some();
    Ok(render(
        StatusCode::OK,
        &LoginPage {
            setup_complete,
            sso_ready,
            site_name: crate::site_name::get(&state.db).await?,
        },
    ))
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfilePage {
    shell: Shell,
    state_style: &'static str,
    state_name: String,
    is_owner: bool,
    characters: Vec<CharacterRow>,
    groups: Vec<String>,
    permissions: Vec<String>,
    /// Member Audit's My Characters, leading the Dashboard: the pilot's
    /// character audit, with AA's own panels after it.
    lead: Option<DashboardWidget>,
    widgets: Vec<DashboardWidget>,
}

/// `GET /profile`: the page is the Dashboard now, as in AA.
pub async fn to_dashboard() -> Redirect {
    Redirect::permanent("/dashboard")
}

/// `GET /dashboard`: AA's Dashboard (characters, state, groups). With
/// Member Audit it's the pilot's character audit: My Characters' card grid
/// first, then AA's panels, compactly.
pub async fn profile(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    let mut loaded = load(&state, &session, "profile").await?;
    annotate(&state, session.account, &mut loaded.characters).await?;
    let groups = tether_db::groups::names_for(&state.db, session.account).await?;
    let held = permissions::effective(&state.db, session.account).await?;
    let blacklisted = tether_db::states::account_state(&state.db, session.account)
        .await?
        .is_some_and(|s| s.is_blacklist());
    let mut widgets: Vec<(String, usize, DashboardWidget)> = state
        .plugins
        .widgets()
        .into_iter()
        .filter(|w| {
            tether_web_core::plugins::may_open(&w.access, blacklisted, |p| held.contains(p))
        })
        .map(|w| {
            (
                w.plugin_id.clone(),
                w.index,
                DashboardWidget {
                    url: format!("/dashboard/widgets/{}/{}", w.plugin_id, w.index),
                    title: w.title,
                },
            )
        })
        .collect();
    // With Member Audit (and access to it), the Dashboard is the pilot's
    // character audit, as Jay asked: its My Characters widget first.
    // Without a main it can't show anything (apps see accounts through
    // their main): AA's Characters, with Make main, instead.
    let lead = widgets
        .iter()
        .position(|(plugin, index, _)| plugin == CHARACTER_AUDIT && *index == 0)
        .map(|i| widgets.remove(i).2)
        .filter(|_| !loaded.shell.no_main);
    let widgets = widgets.into_iter().map(|(_, _, w)| w).collect();
    let permissions = held.into_iter().collect();
    Ok(render(
        StatusCode::OK,
        &ProfilePage {
            shell: loaded.shell,
            state_style: loaded.state.style(),
            state_name: loaded.state.name,
            is_owner: loaded.is_owner,
            characters: loaded.characters,
            groups,
            permissions,
            lead,
            widgets,
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct MainForm {
    character_id: i64,
}

/// `POST /profile/main`: Change Main (Make main) to a character already on
/// the account (with a working token). Back to the Dashboard, which htmx
/// reloads in place (the sidebar, the no-main banner and the state follow
/// the main), with a toast; one that can't be the main says why in a
/// toast and changes nothing.
pub async fn make_main(
    State(state): State<AppState>,
    session: CurrentSession,
    headers: HeaderMap,
    Form(form): Form<MainForm>,
) -> Result<Response, PageError> {
    let outcome = crate::ownership::change_main(&state, session.account, form.character_id).await?;
    Ok(match outcome {
        crate::ownership::ChangeMain::Done { name } => {
            stay::back("/dashboard", format!("{name} is your main now."))
        }
        // Without htmx, the Dashboard as it was.
        _ if !is_htmx(&headers) => Redirect::to("/dashboard").into_response(),
        refused => stay::with_toast(
            StatusCode::NO_CONTENT.into_response(),
            stay::Toast::problem(refused.message()),
        ),
    })
}

/// `POST /profile/main/login`: Change Main by logging in with EVE SSO, as
/// Alliance Auth's (its "add new token" on the Change Main page): the
/// character joins the account as with Add Character (moving from another
/// account if need be) and becomes the main. Asks for the same scopes as
/// Add Character, so the login never narrows a grant.
pub async fn change_main_login(
    State(state): State<AppState>,
    jar: axum_extra::extract::CookieJar,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let required = crate::compliance::registration(&state.db, session.account)
        .await?
        .required;
    let scopes = crate::compliance::ask_scopes(&state.db, session.account, required).await?;
    Ok(crate::auth::start_login(
        &state,
        jar,
        "/dashboard",
        tether_db::auth::Purpose::ChangeMain,
        &scopes,
        Some(session.account),
    )
    .await?)
}
