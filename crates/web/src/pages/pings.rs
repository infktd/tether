//! `/pings`: send a fleet ping and see recent ones.

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::FLEET_PING;
use tether_db::pings::{self as db, Target};

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::pings::{self, MAX_MESSAGE, target_value};

pub struct TargetOption {
    pub value: String,
    pub label: String,
}

pub struct PingRow {
    pub when: String,
    pub sender: String,
    pub channel: String,
    pub target: String,
    pub message: String,
    /// `sent`, `waiting` or `failed`.
    pub status: &'static str,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "pings.html")]
struct PingsPage {
    shell: Shell,
    channels: Vec<db::PingChannel>,
    targets: Vec<TargetOption>,
    recent: Vec<PingRow>,
    max_message: usize,
    form: PingForm,
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PingForm {
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    message: String,
}

fn target_label(target: &Target) -> String {
    match target {
        Target::None => "Nobody (no ping)".to_owned(),
        Target::Here => "@here".to_owned(),
        Target::Everyone => "@everyone".to_owned(),
        Target::Role { name, .. } => format!("@{name}"),
    }
}

async fn page(
    state: &AppState,
    shell: Shell,
    form: PingForm,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let channels = match crate::discord::config(state).await {
        Ok(config) => pings::channels_for(state, &config).await?,
        Err(_) => Vec::new(),
    };
    let targets = pings::targets(state)
        .await?
        .iter()
        .map(|t| TargetOption {
            value: target_value(t),
            label: target_label(t),
        })
        .collect();
    let recent = db::recent(&state.db, 20, pings::STALE_AFTER)
        .await?
        .into_iter()
        .map(|p| PingRow {
            when: p.created_at.format("%Y-%m-%d %H:%M").to_string(),
            sender: p.sender_name,
            channel: p.channel_name,
            target: target_label(&p.target),
            status: if p.sent_at.is_some() {
                "sent"
            } else if p.failed_at.is_some() || p.stale {
                "failed"
            } else {
                "waiting"
            },
            // A ping past the cutoff that no job has closed yet still
            // won't go out, whatever the last attempt said.
            error: if p.stale && p.sent_at.is_none() && p.failed_at.is_none() {
                Some("Not sent: Discord was unavailable for too long".to_owned())
            } else {
                p.error
            },
            message: p.message,
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &PingsPage {
            shell,
            channels,
            targets,
            recent,
            max_message: MAX_MESSAGE,
            form,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /pings`
pub async fn pings_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, FLEET_PING, "pings").await?;
    page(&state, shell, PingForm::default(), None).await
}

/// `POST /pings`
pub async fn send(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<PingForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, FLEET_PING, "pings").await?;
    match pings::send(
        &state,
        session.account,
        &form.channel_id,
        &form.target,
        &form.message,
    )
    .await
    {
        Ok(_) => Ok(Redirect::to("/pings").into_response()),
        Err(err) => page(&state, shell, form, Some(err)).await,
    }
}
