//! EVE SSO login, logout and the session extractor.

use std::time::Duration;

use axum::extract::{FromRequestParts, OptionalFromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::Deserialize;
use tether_core::{Secret, hash_token, new_token};
use tether_db::accounts::{self, AccountId};
use tether_db::{auth as db, settings};
use tether_esi::sso::SsoConfig;

use crate::error::AppError;
use crate::{AppState, setup, tiers};
use tether_db::audit::{self, Actor};

/// `__Host-` cookies must be Secure, Path=/ and have no Domain, so they
/// can't be set or shadowed by other subdomains.
pub const SESSION_COOKIE: &str = "__Host-tether_session";
/// Binds a pending login to the browser that started it.
pub const LOGIN_COOKIE: &str = "__Host-tether_login";

const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
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
            err
        });
    // Remembered for `doctor`: a success proves the client id and callback
    // URL are registered correctly; a failure usually means they aren't.
    let now = chrono::Utc::now().to_rfc3339();
    let identity = match identity {
        Ok(identity) => {
            settings::set(
                &state.db,
                settings::SSO_LAST_SUCCESS,
                serde_json::json!({ "at": now }),
            )
            .await?;
            identity
        }
        Err(err) => {
            settings::set(
                &state.db,
                settings::SSO_LAST_ERROR,
                serde_json::json!({ "at": now, "error": err.to_string() }),
            )
            .await?;
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                "EVE SSO did not confirm the login. Please try again.",
            ));
        }
    };

    // Signed in already? Then this login adds an alt to that account.
    let current = match jar.get(SESSION_COOKIE) {
        Some(cookie) => find_session(&state, cookie.value())
            .await?
            .map(|s| s.account),
        None => None,
    };
    // The browser that entered the setup token claims ownership (F3).
    let claim_owner = setup::has_setup_session(&state, &jar).await?;
    let result = accounts::sign_in(
        &state.db,
        accounts::Login {
            character_id: identity.character_id,
            character_name: &identity.character_name,
            owner_hash: &identity.owner_hash,
        },
        current,
        claim_owner,
    )
    .await?;
    let (outcome, became_owner) = (result.outcome, result.became_owner);
    if let Some(transfer) = &result.transfer {
        record_transfer(&state, identity.character_id, transfer).await?;
    }
    tracing::info!(
        character_id = identity.character_id,
        character = identity.character_name,
        outcome = ?outcome,
        "SSO login"
    );
    let Some(account) = outcome.account() else {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "That character is already linked to another account.",
        ));
    };

    if let Err(err) = state
        .vault
        .store(identity.character_id, &identity.tokens, &identity.scopes)
        .await
    {
        return Err(AppError::internal(err));
    }

    let mut jar = jar;
    if became_owner {
        let mut tx = state.db.begin().await?;
        tether_db::setup::end_sessions(&mut *tx).await?;
        audit::record(
            &mut *tx,
            Actor::Account(account),
            "setup.owner",
            Some(&format!("account:{}", account.0)),
            serde_json::json!({ "character_id": identity.character_id }),
        )
        .await?;
        tx.commit().await?;
        tracing::info!(account = account.0, "owner claimed; setup token disabled");
        jar = jar.remove(removal(setup::SETUP_COOKIE));
    }

    // Tier from the main's current affiliation. An ESI outage must not
    // block login: keep the stored tier and retry in the background.
    if let Err(err) = tiers::refresh_account(&state.db, &state.esi, account).await {
        tracing::warn!(account = account.0, error = %err, "tier refresh at login failed; queued a retry");
        tiers::enqueue_refresh(&state.db, account).await?;
    }

    // Rotate: drop any session this browser already had, then issue a new
    // token.
    if let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &hash_token(old.value())).await?;
    }
    let token = new_token().map_err(AppError::internal)?;
    db::create_session(&state.db, &hash_token(token.expose()), account, SESSION_TTL).await?;

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

/// The signed-in account. Rejects with 401 when there is no live session.
#[derive(Debug, Clone)]
pub struct CurrentSession {
    pub account: AccountId,
}

impl FromRequestParts<AppState> for CurrentSession {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).ok_or_else(AppError::unauthorized)?;
        let record = find_session(state, token.value())
            .await?
            .ok_or_else(AppError::unauthorized)?;
        Ok(Self {
            account: record.account,
        })
    }
}

/// `Option<CurrentSession>`: `None` when not signed in, instead of a 401.
impl OptionalFromRequestParts<AppState> for CurrentSession {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, AppError> {
        match <Self as FromRequestParts<AppState>>::from_request_parts(parts, state).await {
            Ok(session) => Ok(Some(session)),
            Err(err) if err.status() == StatusCode::UNAUTHORIZED => Ok(None),
            Err(err) => Err(err),
        }
    }
}

impl CurrentSession {
    /// Fails with 403 unless the account holds `permission`.
    pub async fn require(&self, state: &AppState, permission: &str) -> Result<(), AppError> {
        let permissions = tether_db::permissions::effective(&state.db, self.account).await?;
        if permissions.contains(permission) {
            Ok(())
        } else {
            tracing::info!(account = self.account.0, permission, "permission denied");
            Err(AppError::forbidden())
        }
    }
}

/// Audits a character that changed EVE account and re-evaluates the tier of
/// the account that lost it.
async fn record_transfer(
    state: &AppState,
    character_id: i64,
    transfer: &accounts::Transfer,
) -> Result<(), AppError> {
    audit::record(
        &state.db,
        Actor::System,
        "character.transferred",
        Some(&format!("character:{character_id}")),
        serde_json::json!({
            "from_account": transfer.from.0,
            "account_deleted": transfer.account_deleted,
            "owner_lost": transfer.owner_lost,
        }),
    )
    .await?;
    if transfer.owner_lost {
        tracing::warn!(
            character_id,
            "the owner's only character moved to another EVE account; the owner account is gone \
             and first-run setup is open again to whoever holds SETUP_TOKEN"
        );
    } else {
        tracing::info!(
            character_id,
            from = transfer.from.0,
            "character transferred to another EVE account"
        );
    }
    if !transfer.account_deleted {
        tiers::evaluate_account(&state.db, transfer.from).await?;
    }
    Ok(())
}

async fn find_session(
    state: &AppState,
    token: &str,
) -> Result<Option<db::SessionRecord>, AppError> {
    Ok(db::find_session(
        &state.db,
        &hash_token(token),
        SESSION_TTL,
        SESSION_TOUCH_EVERY,
    )
    .await?)
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
pub(crate) fn cookie(
    name: &str,
    value: &Secret<String>,
    ttl: Duration,
) -> Result<Cookie<'static>, AppError> {
    Cookie::parse(format!(
        "{name}={}; Max-Age={}; Path=/; Secure; HttpOnly; SameSite=Lax",
        value.expose(),
        ttl.as_secs()
    ))
    .map_err(AppError::internal)
}

/// Removal cookies need the same attributes, or browsers ignore them for
/// `__Host-` names.
pub(crate) fn removal(name: &'static str) -> Cookie<'static> {
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
