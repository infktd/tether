//! Fixture sessions for manual testing (`dev-login` feature).
//!
//! Compiled only into debug builds: `lib.rs` refuses to compile this feature
//! without `debug_assertions`, and CI proves a release build with it fails.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Serialize;
use tether_core::states::Builtin;
use tether_core::{hash_token, new_token};
use tether_db::{accounts, auth as db, states as state_db};

use crate::AppState;
use crate::auth::{SESSION_COOKIE, SESSION_TTL, cookie};
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct Fixture {
    pub name: &'static str,
    pub character_id: i64,
    pub character_name: &'static str,
    /// Forced state: `member`, `blue` or `guest`. Overwritten if states
    /// are re-evaluated, since fixture characters have no affiliation.
    pub state: &'static str,
}

/// Negative character ids: EVE never issues them, so fixtures can't collide
/// with a real character that logs in to the same database.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "owner",
        character_id: -1,
        character_name: "Dev Owner",
        state: "member",
    },
    Fixture {
        name: "member",
        character_id: -2,
        character_name: "Dev Member",
        state: "member",
    },
    Fixture {
        name: "blue",
        character_id: -3,
        character_name: "Dev Blue",
        state: "blue",
    },
    Fixture {
        name: "guest",
        character_id: -4,
        character_name: "Dev Guest",
        state: "guest",
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
        fixture.name == "owner",
    )
    .await?
    .outcome;
    let account = outcome
        .account()
        .ok_or_else(|| AppError::internal("fixture character linked to another account"))?;
    let forced = Builtin::parse(fixture.state).unwrap_or(Builtin::Guest);
    let forced = state_db::builtin(&state.db, forced).await?;
    state_db::set_account_state(&state.db, account, forced.id, true).await?;

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
