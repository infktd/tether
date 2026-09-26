//! `/pings`: send a fleet ping (aa-fleetpings' fields) and see recent
//! ones; `/admin/pings`: what the form offers and who may use what.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{ADMIN_DISCORD, FLEET_PING};
use tether_db::ping_options::{self, Kind, PingOption, Restriction};
use tether_db::pings::{self as db, Target};

use super::admin::{GroupOption, StateOption, guard, parse_grantee, state_options};
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::pings::{self, MAX_FIELD, MAX_MESSAGE, Offer, PingForm, target_value};

pub struct TargetOption {
    pub value: String,
    pub label: String,
}

pub struct PingRow {
    pub when: String,
    pub sender: String,
    pub channel: String,
    pub target: String,
    /// The headline for a detailed ping, else the message.
    pub message: String,
    /// `sent`, `waiting` or `failed`.
    pub status: &'static str,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "pings.html")]
struct PingsPage {
    shell: Shell,
    offer: Offer,
    targets: Vec<TargetOption>,
    recent: Vec<PingRow>,
    max_message: usize,
    max_field: usize,
    form: PingForm,
    /// Whether the account may change what the form offers.
    settings: bool,
    error: Option<String>,
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
    session: &CurrentSession,
    shell: Shell,
    mut form: PingForm,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let offer = pings::offer(state, session.account).await?;
    // The FC is the sender unless they say otherwise.
    if form.fc_name.is_empty() && error.is_none() {
        form.fc_name = shell.user.name.clone();
    }
    let targets = offer
        .targets
        .iter()
        .map(|t| TargetOption {
            value: target_value(t),
            label: target_label(t),
        })
        .collect();
    let settings = tether_db::permissions::effective(&state.db, session.account)
        .await?
        .contains(ADMIN_DISCORD);
    // Limits cover reading too: only pings to channels, with fleet types
    // and doctrines, the viewer may use (settings admins see them all).
    let visible = |p: &db::Ping| {
        settings
            || (offer.channels.iter().any(|c| c.channel_id == p.channel_id)
                && p.details
                    .fleet_type
                    .as_ref()
                    .is_none_or(|t| offer.fleet_types.iter().any(|o| &o.name == t))
                && p.details.doctrine.as_ref().is_none_or(|d| {
                    !offer
                        .closed_doctrines
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(d))
                }))
    };
    let recent = db::recent(&state.db, 100, pings::STALE_AFTER)
        .await?
        .into_iter()
        .filter(visible)
        .take(20)
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
            message: if p.details.is_empty() {
                p.message
            } else {
                match &p.details.fleet_name {
                    Some(name) => format!("{}: {name}", pings::headline(&p.details)),
                    None => pings::headline(&p.details),
                }
            },
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &PingsPage {
            shell,
            offer,
            targets,
            recent,
            max_message: MAX_MESSAGE,
            max_field: MAX_FIELD,
            form,
            settings,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /pings`
pub async fn pings_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, FLEET_PING, "pings").await?;
    page(&state, &session, shell, PingForm::default(), None).await
}

/// `POST /pings`
pub async fn send(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<PingForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, FLEET_PING, "pings").await?;
    match pings::send(&state, session.account, &form).await {
        Ok(_) => Ok(Redirect::to("/pings").into_response()),
        Err(err) => page(&state, &session, shell, form, Some(err)).await,
    }
}

#[derive(Template)]
#[template(path = "pings_preview.html")]
struct PreviewFragment {
    text: Option<String>,
    error: Option<String>,
}

/// `POST /pings/preview` (htmx): the ping as plain text to paste into EVE
/// or another chat, checked as sending would be; nothing is sent.
pub async fn preview(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<PingForm>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(&state, FLEET_PING).await?;
    let offer = pings::offer(&state, session.account).await?;
    let fragment = match pings::details(&form, &offer) {
        Ok(details) => PreviewFragment {
            text: Some(pings::copy_text(&details, &form.message)),
            error: None,
        },
        Err(err) => PreviewFragment {
            text: None,
            error: Some(err.message().to_owned()),
        },
    };
    Ok(render(StatusCode::OK, &fragment))
}

// ---- /admin/pings ----------------------------------------------------------

/// Something that can be limited, with who it's limited to.
pub struct Limitable {
    pub item: String,
    pub label: String,
    /// For options: its id, to delete it.
    pub option: Option<PingOption>,
    pub limits: Vec<Restriction>,
}

#[derive(Template)]
#[template(path = "admin_pings.html")]
struct SettingsPage {
    shell: Shell,
    mass_mentions: bool,
    channels: Vec<Limitable>,
    targets: Vec<Limitable>,
    fleet_types: Vec<Limitable>,
    doctrines: Vec<Limitable>,
    formups: Vec<PingOption>,
    comms: Vec<PingOption>,
    states: Vec<StateOption>,
    groups: Vec<GroupOption>,
    error: Option<String>,
}

async fn settings_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let restrictions = ping_options::restrictions(&state.db).await?;
    let limits = |item: &str| -> Vec<Restriction> {
        restrictions
            .iter()
            .filter(|r| r.item == item)
            .cloned()
            .collect()
    };
    let channels = match crate::discord::config(state).await {
        Ok(config) => pings::channels_for(state, &config).await?,
        Err(_) => Vec::new(),
    }
    .into_iter()
    .map(|c| {
        let item = pings::channel_item(c.channel_id);
        Limitable {
            limits: limits(&item),
            label: format!("#{}", c.name),
            item,
            option: None,
        }
    })
    .collect();
    let targets = pings::targets(state)
        .await?
        .into_iter()
        .filter_map(|t| {
            let item = pings::target_item(&t)?;
            Some(Limitable {
                limits: limits(&item),
                label: target_label(&t),
                item,
                option: None,
            })
        })
        .collect();
    let (mut fleet_types, mut doctrines, mut formups, mut comms) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for option in ping_options::list(&state.db).await? {
        match option.kind {
            Kind::FleetType | Kind::Doctrine => {
                let item = option.item();
                let limitable = Limitable {
                    limits: limits(&item),
                    label: option.name.clone(),
                    item,
                    option: Some(option.clone()),
                };
                if option.kind == Kind::FleetType {
                    fleet_types.push(limitable);
                } else {
                    doctrines.push(limitable);
                }
            }
            Kind::Formup => formups.push(option),
            Kind::Comms => comms.push(option),
        }
    }
    let groups = tether_db::groups::summaries(&state.db)
        .await?
        .into_iter()
        .map(|g| GroupOption {
            id: g.group.id.0,
            name: g.group.name,
        })
        .collect();
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &SettingsPage {
            shell,
            mass_mentions: tether_db::settings::get_bool_or(
                &state.db,
                tether_db::settings::PINGS_MASS_MENTIONS,
                true,
            )
            .await?,
            channels,
            targets,
            fleet_types,
            doctrines,
            formups,
            comms,
            states: state_options(state).await?,
            groups,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

async fn done(
    state: &AppState,
    shell: Shell,
    result: Result<(), AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(()) => Ok(Redirect::to("/admin/pings").into_response()),
        Err(err) => settings_page(state, shell, Some(err)).await,
    }
}

/// `GET /admin/pings`
pub async fn settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    settings_page(&state, shell, None).await
}

#[derive(Debug, Deserialize)]
pub struct MassForm {
    #[serde(default)]
    mass_mentions: Option<String>,
}

/// `POST /admin/pings/settings`
pub async fn save_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MassForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    let result =
        pings::set_mass_mentions(&state, session.account, form.mass_mentions.is_some()).await;
    done(&state, shell, result).await
}

#[derive(Debug, Deserialize)]
pub struct OptionForm {
    kind: String,
    name: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    color: String,
}

/// `POST /admin/pings/options`
pub async fn add_option(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<OptionForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    let result = pings::add_option(
        &state,
        session.account,
        &form.kind,
        &form.name,
        &form.link,
        &form.color,
    )
    .await;
    done(&state, shell, result).await
}

/// `POST /admin/pings/options/{id}/delete`
pub async fn delete_option(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    let result = pings::delete_option(&state, session.account, id).await;
    done(&state, shell, result).await
}

#[derive(Debug, Deserialize)]
pub struct RestrictForm {
    item: String,
    /// `state:<id>` or `group:<id>`.
    grantee: String,
}

/// `POST /admin/pings/restrictions`
pub async fn restrict(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<RestrictForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    let result = match parse_grantee(&form.grantee) {
        Ok(grantee) => pings::restrict(&state, session.account, &form.item, grantee).await,
        Err(err) => Err(err),
    };
    done(&state, shell, result).await
}

/// `POST /admin/pings/restrictions/{id}/remove`
pub async fn unrestrict(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "pings_settings").await?;
    let result = pings::unrestrict(&state, session.account, id).await;
    done(&state, shell, result).await
}
