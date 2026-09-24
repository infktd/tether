//! EVE SSO login, logout and the session extractor.

use std::time::Duration;

use axum::extract::{FromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;
use tether_core::{Secret, hash_token, new_token};
use tether_db::{auth as db, settings};
use tether_esi::sso::SsoConfig;

use crate::AppState;
use crate::error::AppError;

/// `__Host-` cookies must be Secure, Path=/ and have no Domain, so they
/// can't be set or shadowed by other subdomains.
pub const SESSION_COOKIE: &str = "__Host-tether_session";
/// Binds a pending login to the browser that started it.
pub const LOGIN_COOKIE: &str = "__Host-tether_login";

const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const SESSION_TOUCH_EVERY: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    return_to: Option<String>,
}

/// `GET /auth/login`: start an EVE SSO login.
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let config = sso_config(&state).await?;
    let pending = state.sso.begin(&config).map_err(AppError::internal)?;
    let browser = new_token().map_err(AppError::internal)?;
    let return_to = safe_return_to(query.return_to.as_deref());

    db::insert_login_attempt(
        &state.db,
        db::NewLoginAttempt {
            state: &pending.state,
            browser_hash: &hash_token(browser.expose()),
            pkce_verifier: &pending.pkce_verifier,
            return_to: &return_to,
            ttl: LOGIN_TTL,
        },
    )
    .await?;

    let jar = jar.add(cookie(LOGIN_COOKIE, &browser, LOGIN_TTL)?);
    Ok((jar, Redirect::to(&pending.authorize_url)).into_response())
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /auth/callback`: CCP redirects here after the user logs in.
pub async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let browser = jar.get(LOGIN_COOKIE).map(|c| c.value().to_owned());
    let jar = jar.remove(removal(LOGIN_COOKIE));
    let expired = || {
        AppError::new(
            StatusCode::BAD_REQUEST,
            "This login has expired or was already used. Please log in again.",
        )
    };
    if let Some(error) = query.error {
        tracing::info!(error, "SSO login cancelled or refused");
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "Login was cancelled.",
        ));
    }
    let (Some(code), Some(oauth_state)) = (query.code, query.state) else {
        return Err(expired());
    };
    let Some(browser) = browser else {
        return Err(expired());
    };
    let Some(attempt) =
        db::take_login_attempt(&state.db, &oauth_state, &hash_token(&browser)).await?
    else {
        return Err(expired());
    };

    let config = sso_config(&state).await?;
    let identity = state
        .sso
        .finish(&config, code, attempt.pkce_verifier)
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "SSO exchange failed");
            AppError::new(
                StatusCode::BAD_GATEWAY,
                "EVE SSO did not confirm the login. Please try again.",
            )
        })?;

    // Rotate: drop any session this browser already had, then issue a new
    // token.
    if let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &hash_token(old.value())).await?;
    }
    let token = new_token().map_err(AppError::internal)?;
    db::create_session(
        &state.db,
        &hash_token(token.expose()),
        identity.character_id,
        &identity.character_name,
        SESSION_TTL,
    )
    .await?;
    tracing::info!(
        character_id = identity.character_id,
        character = identity.character_name,
        "logged in"
    );

    let jar = jar.add(cookie(SESSION_COOKIE, &token, SESSION_TTL)?);
    Ok((jar, Redirect::to(&attempt.return_to)).into_response())
}

/// `POST /auth/logout`.
pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    if let Some(session) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &hash_token(session.value())).await?;
    }
    let jar = jar.remove(removal(SESSION_COOKIE));
    Ok((jar, Redirect::to("/")).into_response())
}

/// The logged-in character. Rejects with 401 when there is no live session.
#[derive(Debug, Clone)]
pub struct CurrentSession {
    pub character_id: i64,
    pub character_name: String,
}

impl FromRequestParts<AppState> for CurrentSession {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).ok_or_else(AppError::unauthorized)?;
        let record = db::find_session(
            &state.db,
            &hash_token(token.value()),
            SESSION_TTL,
            SESSION_TOUCH_EVERY,
        )
        .await?
        .ok_or_else(AppError::unauthorized)?;
        Ok(Self {
            character_id: record.character_id,
            character_name: record.character_name,
        })
    }
}

async fn sso_config(state: &AppState) -> Result<SsoConfig, AppError> {
    let client_id = settings::get_string(&state.db, settings::SSO_CLIENT_ID)
        .await?
        .ok_or_else(|| {
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "EVE SSO isn't configured yet. Finish the setup wizard first.",
            )
        })?;
    Ok(SsoConfig {
        client_id,
        redirect_uri: state.site.sso_callback_url(),
    })
}

/// Parsed from a header string because `Cookie::max_age` takes a
/// `time::Duration`, which axum-extra doesn't re-export. Values are hex
/// tokens, so nothing needs escaping.
fn cookie(name: &str, value: &Secret<String>, ttl: Duration) -> Result<Cookie<'static>, AppError> {
    Cookie::parse(format!(
        "{name}={}; Max-Age={}; Path=/; Secure; HttpOnly; SameSite=Lax",
        value.expose(),
        ttl.as_secs()
    ))
    .map_err(AppError::internal)
}

/// Removal cookies need the same attributes, or browsers ignore them for
/// `__Host-` names.
fn removal(name: &'static str) -> Cookie<'static> {
    Cookie::build(name)
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/")
        .build()
}

/// Only local paths, so `return_to` can't become an open redirect.
fn safe_return_to(value: Option<&str>) -> String {
    match value {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.contains('\\')
                && !path.chars().any(char::is_control)
                && path.len() <= 512 =>
        {
            path.to_owned()
        }
        _ => "/".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::safe_return_to;

    #[test]
    fn return_to_only_allows_local_paths() {
        assert_eq!(
            safe_return_to(Some("/profile?tab=alts")),
            "/profile?tab=alts"
        );
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "profile",
            "/a\r\nSet-Cookie: x",
        ] {
            assert_eq!(safe_return_to(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(safe_return_to(None), "/");
    }
}
