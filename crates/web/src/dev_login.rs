//! Fixture sessions for manual testing (`dev-login` feature).
//!
//! Compiled only into debug builds: `lib.rs` refuses to compile this feature
//! without `debug_assertions`, and CI proves a release build with it fails.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Serialize;
use tether_core::tiers::Tier;
use tether_core::{hash_token, new_token};
use tether_db::{accounts, auth as db, tiers as tier_db};

use crate::AppState;
use crate::auth::{SESSION_COOKIE, SESSION_TTL, cookie};
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct Fixture {
    pub name: &'static str,
    pub character_id: i64,
    pub character_name: &'static str,
    /// Forced tier. Overwritten if tiers are re-evaluated, since fixture
    /// characters have no affiliation.
    pub tier: &'static str,
}

/// Negative character ids: EVE never issues them, so fixtures can't collide
/// with a real character that logs in to the same database.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "owner",
        character_id: -1,
        character_name: "Dev Owner",
        tier: "member",
    },
    Fixture {
        name: "member",
        character_id: -2,
        character_name: "Dev Member",
        tier: "member",
    },
    Fixture {
        name: "allied",
        character_id: -3,
        character_name: "Dev Allied",
        tier: "allied",
    },
    Fixture {
        name: "guest",
        character_id: -4,
        character_name: "Dev Guest",
        tier: "guest",
    },
];

/// `GET /dev/login`: the available fixtures.
pub async fn list() -> Json<&'static [Fixture]> {
    Json(FIXTURES)
}

/// `GET /dev/login/{fixture}`: sign in as a fixture and go home.
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    let fixture = FIXTURES
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| AppError::not_found("No such fixture; see /dev/login."))?;
    let outcome = accounts::sign_in(
        &state.db,
        accounts::Login {
            character_id: fixture.character_id,
            character_name: fixture.character_name,
            owner_hash: "dev-fixture",
        },
        None,
        fixture.name == "owner",
    )
    .await?
    .outcome;
    let account = outcome
        .account()
        .ok_or_else(|| AppError::internal("fixture character linked to another account"))?;
    let tier = Tier::parse(fixture.tier).unwrap_or(Tier::Guest);
    tier_db::set_account_tier(&state.db, account, tier).await?;

    if let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &hash_token(old.value())).await?;
    }
    let token = new_token().map_err(AppError::internal)?;
    db::create_session(&state.db, &hash_token(token.expose()), account, SESSION_TTL).await?;
    tracing::warn!(
        fixture = fixture.name,
        account = account.0,
        "dev-login session created"
    );
    let jar = jar.add(cookie(SESSION_COOKIE, &token, SESSION_TTL)?);
    Ok((jar, Redirect::to("/")).into_response())
}
