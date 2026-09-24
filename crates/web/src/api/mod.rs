//! JSON API.

pub mod admin;
pub mod groups;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tether_core::tiers::Tier;
use tether_db::accounts;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct Me {
    pub account_id: i64,
    pub is_owner: bool,
    pub tier: &'static str,
    pub main: CharacterRef,
    pub characters: Vec<CharacterSummary>,
    pub groups: Vec<String>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CharacterRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CharacterSummary {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
}

/// `GET /api/me`: the signed-in account and its characters.
pub async fn me(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Me>, AppError> {
    let account = accounts::get(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let tier = tether_db::tiers::account_tier(&state.db, session.account)
        .await?
        .unwrap_or(Tier::Guest);
    Ok(Json(Me {
        account_id: account.id.0,
        is_owner: account.is_owner,
        tier: tier.as_str(),
        characters: account
            .characters
            .iter()
            .map(|c| CharacterSummary {
                id: c.id,
                name: c.name.clone(),
                is_main: c.id == account.main.id,
            })
            .collect(),
        main: CharacterRef {
            id: account.main.id,
            name: account.main.name,
        },
        groups: tether_db::groups::names_for(&state.db, session.account).await?,
        permissions: tether_db::permissions::effective(&state.db, session.account)
            .await?
            .into_iter()
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SetMain {
    pub character_id: i64,
}

/// `POST /api/me/main`: make one of your characters the main.
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
        // The tier follows the main; affiliations were fetched at login.
        crate::tiers::evaluate_account(&state.db, session.account).await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("That character isn't on your account."))
    }
}
