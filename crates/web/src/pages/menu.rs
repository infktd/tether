//! `/admin/menu` (AA's Menu): arrange the sidebar.

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::ADMIN_SYSTEM;

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::menu::{self, Available, Edit, Section};

/// Every item any account might see: all built-in pages and every running
/// app's links.
fn catalogue(state: &AppState) -> Vec<Available> {
    menu::BUILTINS
        .iter()
        .map(|b| menu::builtin_item(b, None))
        .chain(
            state
                .plugins
                .navigation()
                .into_iter()
                .map(|n| menu::plugin_item(&n.label, &n.href)),
        )
        .collect()
}

/// A place something can go: a section, or a folder in one.
pub struct Place {
    pub reference: String,
    pub label: String,
    pub folder: bool,
}

#[derive(Template)]
#[template(path = "admin_menu.html")]
struct MenuPage {
    shell: Shell,
    sections: Vec<Section>,
    places: Vec<Place>,
    error: Option<String>,
}

async fn menu_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let entries = tether_db::menu::entries(&state.db).await?;
    let sections = menu::build(&entries, catalogue(state));
    let mut places = Vec::new();
    for s in &sections {
        places.push(Place {
            reference: s.reference.clone(),
            label: s.label.clone(),
            folder: false,
        });
        for n in s.nodes.iter().filter(|n| n.is_folder()) {
            places.push(Place {
                reference: n.reference.clone(),
                label: format!("{} › {}", s.label, n.label),
                folder: true,
            });
        }
    }
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &MenuPage {
            shell,
            sections,
            places,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/menu`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_SYSTEM, "menu").await?;
    menu_page(&state, shell, None).await
}

#[derive(Debug, Default, Deserialize)]
pub struct MenuForm {
    #[serde(default)]
    reference: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    new_tab: Option<String>,
    #[serde(default)]
    parent: String,
    #[serde(default)]
    hidden: Option<String>,
    /// Moves: `up` or `down`.
    #[serde(default)]
    dir: String,
}

async fn apply(
    state: AppState,
    session: Option<CurrentSession>,
    edit: impl FnOnce(&MenuForm) -> Edit<'_>,
    form: MenuForm,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_SYSTEM, "menu").await?;
    let result = menu::edit(&state.db, session.account, &catalogue(&state), edit(&form)).await;
    match result {
        Ok(()) => Ok(Redirect::to("/admin/menu").into_response()),
        Err(err) => menu_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/menu/sections`
pub async fn add_section(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::AddSection { label: &f.label },
        form,
    )
    .await
}

/// `POST /admin/menu/folders`
pub async fn add_folder(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::AddFolder {
            label: &f.label,
            parent: &f.parent,
        },
        form,
    )
    .await
}

/// `POST /admin/menu/links`
pub async fn add_link(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::AddLink {
            label: &f.label,
            url: &f.url,
            new_tab: f.new_tab.is_some(),
            parent: &f.parent,
        },
        form,
    )
    .await
}

/// `POST /admin/menu/change`
pub async fn change(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::Change {
            reference: &f.reference,
            label: &f.label,
            url: &f.url,
            new_tab: f.new_tab.is_some(),
            parent: &f.parent,
            hidden: f.hidden.is_some(),
        },
        form,
    )
    .await
}

/// `POST /admin/menu/move`
pub async fn move_entry(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::Move {
            reference: &f.reference,
            up: f.dir == "up",
        },
        form,
    )
    .await
}

/// `POST /admin/menu/delete`
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(
        state,
        session,
        |f| Edit::Delete {
            reference: &f.reference,
        },
        form,
    )
    .await
}

/// `POST /admin/menu/reset`
pub async fn reset(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MenuForm>,
) -> Result<Response, PageError> {
    apply(state, session, |_| Edit::Reset, form).await
}
