//! Admin → Auto Groups: configs that keep corporation and alliance groups
//! for chosen states (Alliance Auth's Auto Groups).

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use tether_core::permissions::ADMIN_GROUPS;
use tether_core::states::StateId;
use tether_db::autogroups::{self as db, Settings, Source};

use super::admin::{StateOption, guard, state_options};
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

type Fields = Vec<(String, String)>;

fn field<'a>(fields: &'a Fields, name: &str) -> &'a str {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn checked(fields: &Fields, name: &str) -> bool {
    fields.iter().any(|(k, _)| k == name)
}

fn read(fields: &Fields) -> Result<(Settings, Vec<StateId>), AppError> {
    let source = |name: &str| {
        Source::parse(field(fields, name))
            .ok_or_else(|| AppError::bad_request("Choose name or ticker."))
    };
    let settings = Settings {
        corp_groups: checked(fields, "corp_groups"),
        corp_prefix: field(fields, "corp_prefix").to_owned(),
        corp_source: source("corp_source")?,
        alliance_groups: checked(fields, "alliance_groups"),
        alliance_prefix: field(fields, "alliance_prefix").to_owned(),
        alliance_source: source("alliance_source")?,
        replace_spaces: checked(fields, "replace_spaces"),
        replace_with: field(fields, "replace_with").to_owned(),
    };
    let states = fields
        .iter()
        .filter(|(k, _)| k == "states")
        .map(|(_, v)| v.parse().map(StateId))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::bad_request("Choose states from the list."))?;
    Ok((settings, states))
}

pub struct StateChoice {
    pub id: i64,
    pub name: String,
    pub checked: bool,
}

pub struct ConfigView {
    pub id: i64,
    pub settings: Settings,
    pub states: Vec<StateChoice>,
    pub groups: Vec<String>,
}

fn choices(states: &[StateOption], chosen: &[StateId]) -> Vec<StateChoice> {
    states
        .iter()
        .map(|s| StateChoice {
            id: s.id,
            name: s.name.clone(),
            checked: chosen.iter().any(|c| c.0 == s.id),
        })
        .collect()
}

#[derive(Template)]
#[template(path = "admin_autogroups.html")]
struct AutoGroupsPage {
    shell: Shell,
    configs: Vec<ConfigView>,
    new_states: Vec<StateChoice>,
    error: Option<String>,
}

async fn show(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let states = state_options(state).await?;
    let mut configs = Vec::new();
    for config in db::configs(&state.db).await? {
        let groups = db::groups_of(&state.db, config.id)
            .await?
            .into_iter()
            .map(|(_, _, _, name)| name)
            .collect();
        configs.push(ConfigView {
            id: config.id,
            states: choices(&states, &config.states),
            settings: config.settings,
            groups,
        });
    }
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        status,
        &AutoGroupsPage {
            shell,
            configs,
            new_states: choices(&states, &[]),
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// `GET /admin/autogroups`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "autogroups").await?;
    show(&state, shell, None).await
}

async fn done(
    state: &AppState,
    shell: Shell,
    result: Result<(), AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(()) => Ok(Redirect::to("/admin/autogroups").into_response()),
        Err(err) => show(state, shell, Some(err)).await,
    }
}

/// `POST /admin/autogroups`
pub async fn create(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "autogroups").await?;
    let result = match read(&fields) {
        Ok((settings, states)) => {
            crate::autogroups::create(&state.db, session.account, &settings, &states)
                .await
                .map(|_| ())
        }
        Err(err) => Err(err),
    };
    done(&state, shell, result).await
}

/// `POST /admin/autogroups/{id}`
pub async fn update(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "autogroups").await?;
    let result = match read(&fields) {
        Ok((settings, states)) => {
            crate::autogroups::update(&state.db, session.account, id, &settings, &states).await
        }
        Err(err) => Err(err),
    };
    done(&state, shell, result).await
}

/// `POST /admin/autogroups/{id}/delete`: its groups go too.
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "autogroups").await?;
    let result = crate::autogroups::delete(&state.db, session.account, id).await;
    done(&state, shell, result).await
}
