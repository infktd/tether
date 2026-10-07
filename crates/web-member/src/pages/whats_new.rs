//! What's new (DESIGN.md): every release's notes that concern the viewer,
//! and the popup's "read".

use askama::Template;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use tether_web_core::whats_new::{self, ReleaseView, Viewer};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Template)]
#[template(path = "whats_new.html")]
struct WhatsNewPage {
    shell: Shell,
    releases: Vec<ReleaseView>,
}

/// `GET /whats-new`: every release, newest first; reading it counts as
/// having seen them.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let mut loaded = load(&state, &session, "whats_new").await?;
    let releases = whats_new::for_viewer(
        whats_new::releases(),
        0,
        &Viewer {
            admin: loaded.shell.nav.any(),
            apps: &loaded.apps,
        },
    );
    tether_db::whats_new::mark_seen(&state.db, session.account, whats_new::latest()).await?;
    // This page says it all: no popup over it.
    loaded.shell.whats_new = None;
    Ok(render(
        StatusCode::OK,
        &WhatsNewPage {
            shell: loaded.shell,
            releases,
        },
    ))
}

/// `POST /whats-new/seen`: the popup opened, so what it shows is read
/// (and the apps updated until now). From the popup nothing comes back; a
/// plain post goes on to the page.
pub async fn seen(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    tether_db::whats_new::mark_seen(&state.db, session.account, whats_new::latest()).await?;
    if headers.contains_key("hx-request") {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Ok(Redirect::to("/whats-new").into_response())
    }
}
