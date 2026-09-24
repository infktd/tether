//! JSON API.

use axum::Json;
use serde::Serialize;

use crate::auth::CurrentSession;

#[derive(Debug, Serialize)]
pub struct Me {
    pub character_id: i64,
    pub character_name: String,
}

/// `GET /api/me`: who is logged in.
pub async fn me(session: CurrentSession) -> Json<Me> {
    Json(Me {
        character_id: session.character_id,
        character_name: session.character_name,
    })
}
