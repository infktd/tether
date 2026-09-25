//! JSON API.

pub mod admin;
pub mod group_management;
pub mod groups;
pub mod notifications;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tether_db::accounts;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Me {
    pub account_id: i64,
    pub is_owner: bool,
    /// The access state's name: Member, Blue, Guest or one an admin made.
    pub state: String,
    /// `None` after the main was sold or lost its token, until Change Main.
    pub main: Option<CharacterRef>,
    pub characters: Vec<CharacterSummary>,
    pub groups: Vec<String>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CharacterRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CharacterSummary {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
}

/// `GET /api/me`: the signed-in account and its characters.
#[utoipa::path(get, path = "/api/me", tag = "account", security(("session" = [])),
    responses((status = 200, body = Me), (status = 401, description = "Not signed in")))]
pub async fn me(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Me>, AppError> {
    let account = accounts::get(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let access = tether_db::states::account_state(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    Ok(Json(Me {
        account_id: account.id.0,
        is_owner: account.is_owner,
        state: access.name,
        characters: account
            .characters
            .iter()
            .map(|c| CharacterSummary {
                id: c.id,
                name: c.name.clone(),
                is_main: account.main.as_ref().is_some_and(|m| m.id == c.id),
            })
            .collect(),
        main: account.main.map(|m| CharacterRef {
            id: m.id,
            name: m.name,
        }),
        groups: tether_db::groups::names_for(&state.db, session.account).await?,
        permissions: tether_db::permissions::effective(&state.db, session.account)
            .await?
            .into_iter()
            .collect(),
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SetMain {
    pub character_id: i64,
}

/// `POST /api/me/main`: make one of your characters the main.
#[utoipa::path(post, path = "/api/me/main", tag = "account", security(("session" = [])), request_body = SetMain,
    responses((status = 204, description = "Main changed; state re-evaluated"),
              (status = 404, description = "Not one of your characters")))]
pub async fn set_main(
    State(state): State<AppState>,
    session: CurrentSession,
    Json(body): Json<SetMain>,
) -> Result<StatusCode, AppError> {
    if accounts::set_main(&state.db, session.account, body.character_id).await? {
        tracing::info!(
            account = session.account.0,
            character_id = body.character_id,
            "main changed"
        );
        // The state follows the main; affiliations were fetched at login.
        crate::states::evaluate_account(&state.db, session.account).await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("That character isn't on your account."))
    }
}
