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
use crate::{AppState, setup, states};
use tether_db::audit::{self, Actor};

/// `__Host-` cookies must be Secure, Path=/ and have no Domain, so they
/// can't be set or shadowed by other subdomains.
pub const SESSION_COOKIE: &str = "__Host-tether_session";
/// Binds a pending login to the browser that started it.
pub const LOGIN_COOKIE: &str = "__Host-tether_login";

const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);
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
    let return_to = safe_return_to(query.return_to.as_deref());
    start_login(&state, jar, &return_to, db::Purpose::Login, &[], None).await
}

/// Sends the browser to EVE SSO asking for `scopes`, remembering why.
pub(crate) async fn start_login(
    state: &AppState,
    jar: CookieJar,
    return_to: &str,
    purpose: db::Purpose,
    scopes: &[String],
    started_by: Option<AccountId>,
) -> Result<Response, AppError> {
    let config = sso_config(state).await?;
    let pending = state
        .sso
        .begin(&config, scopes)
        .map_err(AppError::internal)?;
    let browser = new_token().map_err(AppError::internal)?;

    db::insert_login_attempt(
        &state.db,
        db::NewLoginAttempt {
            state: &pending.state,
            browser_hash: &hash_token(browser.expose()),
            pkce_verifier: &pending.pkce_verifier,
            return_to,
            ttl: LOGIN_TTL,
            purpose,
            scopes,
            started_by,
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
        // Asking for a scope Tether's EVE application doesn't allow.
        let message = if error == "invalid_scope" {
            "EVE refused a scope Tether asked for. An admin needs to enable every required \
             scope on Tether's EVE application (developers.eveonline.com)."
        } else {
            "Login was cancelled."
        };
        return Err(AppError::new(StatusCode::BAD_REQUEST, message));
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

    // Signed in already?
    let current = match jar.get(SESSION_COOKIE) {
        Some(cookie) => find_session(&state, cookie.value())
            .await?
            .map(|s| s.account),
        None => None,
    };
    let login = accounts::Login {
        character_id: identity.character_id,
        character_name: &identity.character_name,
        owner_hash: &identity.owner_hash,
    };
    // Two kinds of login, as in Alliance Auth. A plain login signs in (only
    // with the main); anything a signed-in account started (Add Character,
    // offers) links the character to that account, moving it from another
    // account if need be: SSO just proved control of it.
    let (account, became_owner, lost) = if attempt.purpose == db::Purpose::Login {
        // The browser that entered the setup token claims ownership (F3).
        let claim_owner = setup::has_setup_session(&state, &jar).await?;
        let result = accounts::sign_in(&state.db, login, claim_owner).await?;
        tracing::info!(
            character_id = identity.character_id,
            character = identity.character_name,
            outcome = ?result.outcome,
            "SSO login"
        );
        if let Some(lost) = &result.lost {
            record_lost(&state, lost).await?;
        }
        let account = match result.outcome {
            accounts::SignIn::NotMain => {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "Unable to authenticate as the selected character. Please log in with the \
                     main character associated with this account.",
                ));
            }
            accounts::SignIn::Deactivated => {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "This account has been deactivated.",
                ));
            }
            accounts::SignIn::Existing(a)
            | accounts::SignIn::Reattached(a)
            | accounts::SignIn::Created(a) => a,
        };
        (account, result.became_owner, None)
    } else {
        // Only for the account that started it, still signed in here.
        let Some(account) = current.filter(|c| Some(*c) == attempt.started_by) else {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "This was started from another session. Please sign in and try again.",
            ));
        };
        let result = accounts::link(&state.db, login, account).await?;
        if result.outcome == accounts::Linked::Deactivated {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "That character belongs to a deactivated account. Ask an admin.",
            ));
        }
        tracing::info!(
            character_id = identity.character_id,
            account = account.0,
            outcome = ?result.outcome,
            "character linked"
        );
        (account, false, result.lost)
    };
    if let Some(lost) = &lost {
        record_lost(&state, lost).await?;
    }

    if let Err(err) = state
        .vault
        .store(identity.character_id, &identity.tokens, &identity.scopes)
        .await
    {
        return Err(AppError::internal(err));
    }
    match &attempt.purpose {
        db::Purpose::DataSource(plugin) => {
            crate::plugin_consent::finish(&state, account, &identity, plugin).await?;
        }
        db::Purpose::CorpSource => {
            crate::compliance::finish_corp_offer(&state, account, &identity).await?;
        }
        db::Purpose::Login | db::Purpose::Register => {}
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

    // State from the main's current affiliation. An ESI outage must not
    // block login: keep the stored state and retry in the background.
    if let Err(err) = states::refresh_account(
        &state.db,
        &state.esi,
        account,
        tether_esi::Priority::Interactive,
    )
    .await
    {
        tracing::warn!(account = account.0, error = %err, "state refresh at login failed; queued a retry");
        states::enqueue_refresh(&state.db, account).await?;
        // Compliance needs no ESI: a character just added without scopes
        // counts at once.
        states::evaluate_account(&state.db, account).await?;
    }

    // Rotate: drop any session this browser already had, then issue a new
    // token.
    if let Some(old) = jar.get(SESSION_COOKIE) {
        db::delete_session(&state.db, &hash_token(old.value())).await?;
    }
    let token = new_token().map_err(AppError::internal)?;
    db::create_session(&state.db, &hash_token(token.expose()), account, SESSION_TTL).await?;

    let jar = jar.add(cookie(SESSION_COOKIE, &token, SESSION_TTL)?);
    // Not every character registered with the state's scopes yet: show
    // what to do (F11).
    let return_to = if tether_db::compliance::not_compliant_state(&state.db, account)
        .await?
        .is_some()
    {
        "/register"
    } else {
        attempt.return_to.as_str()
    };
    Ok((jar, Redirect::to(return_to)).into_response())
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

async fn record_lost(state: &AppState, lost: &accounts::Lost) -> Result<(), AppError> {
    crate::ownership::after_lost(&state.db, lost).await?;
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
