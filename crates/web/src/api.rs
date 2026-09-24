//! JSON API.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tether_db::accounts;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct Me {
    pub account_id: i64,
    pub is_owner: bool,
    pub main: CharacterRef,
    pub characters: Vec<CharacterSummary>,
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
    Ok(Json(Me {
        account_id: account.id.0,
        is_owner: account.is_owner,
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
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::new(
            StatusCode::NOT_FOUND,
            "That character isn't on your account.",
        ))
    }
}
