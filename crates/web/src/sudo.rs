//! Sudo mode, as GitHub's: before an owner-only or especially sensitive
//! action, a browser session must have logged in with EVE SSO, with the
//! account's main, in the last [`WINDOW`]. Otherwise the action is refused
//! and the browser goes to a page naming it ("Confirm it's you"), whose
//! button starts a fresh EVE login (purpose `reauth`). Afterwards the
//! browser is back on the page it came from, where the admin submits again:
//! nothing is replayed, and a POST never is.
//!
//! The gated actions are exactly [`Action::ALL`], each checked with
//! [`check`] where the action happens (so the page and the JSON API share
//! it), after the permission check and before anything changes.
//!
//! Only browser sessions are gated. A personal access token isn't a
//! browser and can't log in again; it is let through, as GitHub's tokens
//! are: making a token is itself gated, a token carries each permission it
//! uses explicitly, and it never counts as the owner (so owner-only actions
//! are refused to it anyway). Nor are the CLI and background jobs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tether_db::auth::Purpose;

use crate::AppState;
use crate::auth::{CurrentSession, safe_path};
use crate::error::AppError;
use crate::pages::{PageError, is_htmx, render};

/// How recent the last EVE login must be.
pub const WINDOW: Duration = Duration::from_secs(15 * 60);

/// Where the browser goes back to when the page it came from is unknown.
const FALLBACK: &str = "/dashboard";

/// An action that needs a recent EVE login. Keep `docs/ARCHITECTURE.md`
/// (Identity and permissions) in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Granting or revoking a permission `tether_core::permissions::is_sensitive`
    /// marks (admin powers, group management, fleet pings to everyone,
    /// compliance, blacklist, Corp Stats, Permissions Audit), or letting
    /// anyone into a group that grants one (`groups::require_grants`).
    SensitivePermission,
    /// Approving an app's install or upgrade.
    AppInstall,
    AppRollback,
    AppUninstall,
    /// Re-pinning an app's publisher key.
    AppKey,
    /// Setting an app's secret.
    AppSecret,
    AccountDeactivate,
    AccountReactivate,
    /// Making a personal access token.
    AccessToken,
    /// Changing setup (the EVE application) once an owner exists.
    Setup,
    /// The Discord application and bot (their secrets included).
    DiscordSettings,
    /// Anything only the owner may do to a Restricted group: its members,
    /// leaders, settings and the flag itself.
    RestrictedGroup,
    /// Changing the main, for the owner and accounts holding a sensitive
    /// permission ([`check_privileged`]): the main is what confirms it's
    /// them, so a stolen session mustn't swap in a character of its own.
    ChangeMain,
    /// Linking a character (Add Character, registering, offers), for the
    /// same accounts: a linked character is a candidate main.
    AddCharacter,
    /// Deleting the main's token, for the same accounts: a main whose
    /// token is gone is cleared, and another character can become it.
    MainToken,
}

impl Action {
    pub const ALL: [Action; 15] = [
        Self::SensitivePermission,
        Self::AppInstall,
        Self::AppRollback,
        Self::AppUninstall,
        Self::AppKey,
        Self::AppSecret,
        Self::AccountDeactivate,
        Self::AccountReactivate,
        Self::AccessToken,
        Self::Setup,
        Self::DiscordSettings,
        Self::RestrictedGroup,
        Self::ChangeMain,
        Self::AddCharacter,
        Self::MainToken,
    ];

    /// Its name in URLs and the audit log.
    pub fn key(self) -> &'static str {
        match self {
            Self::SensitivePermission => "sensitive_permission",
            Self::AppInstall => "app_install",
            Self::AppRollback => "app_rollback",
            Self::AppUninstall => "app_uninstall",
            Self::AppKey => "app_key",
            Self::AppSecret => "app_secret",
            Self::AccountDeactivate => "account_deactivate",
            Self::AccountReactivate => "account_reactivate",
            Self::AccessToken => "access_token",
            Self::Setup => "setup",
            Self::DiscordSettings => "discord_settings",
            Self::RestrictedGroup => "restricted_group",
            Self::ChangeMain => "change_main",
            Self::AddCharacter => "add_character",
            Self::MainToken => "main_token",
        }
    }

    /// What the admin was doing, for the page asking them to log in again.
    pub fn label(self) -> &'static str {
        match self {
            Self::SensitivePermission => "Grant or revoke a sensitive permission",
            Self::AppInstall => "Install or upgrade an app",
            Self::AppRollback => "Roll back an app",
            Self::AppUninstall => "Uninstall an app",
            Self::AppKey => "Re-pin an app's publisher key",
            Self::AppSecret => "Change an app's secret",
            Self::AccountDeactivate => "Deactivate an account",
            Self::AccountReactivate => "Reactivate an account",
            Self::AccessToken => "Create an access token",
            Self::Setup => "Change the instance's setup",
            Self::DiscordSettings => "Change the Discord settings",
            Self::RestrictedGroup => "Change a Restricted group",
            Self::ChangeMain => "Change your main character",
            Self::AddCharacter => "Add a character to your account",
            Self::MainToken => "Delete your main character's token",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// Whether a session last logged in with EVE recently enough.
pub fn is_fresh(reauthenticated_at: Option<DateTime<Utc>>) -> bool {
    reauthenticated_at.is_some_and(|at| {
        let age = Utc::now().signed_duration_since(at);
        age >= chrono::TimeDelta::zero() && age.to_std().is_ok_and(|age| age < WINDOW)
    })
}

/// A browser request's standing, and the action it was refused, if any.
struct Scope {
    fresh: bool,
    required: Mutex<Option<Action>>,
}

tokio::task_local! {
    static SCOPE: Arc<Scope>;
}

/// Refuses `action` unless this request comes from a browser session that
/// logged in with EVE in the last [`WINDOW`] (or isn't a browser session:
/// see the module docs). A refusal is also noted for [`layer`], which
/// sends the browser to confirm it's them, whatever page the handler would
/// have shown.
pub fn check(action: Action) -> Result<(), AppError> {
    let stale = SCOPE
        .try_with(|scope| {
            if scope.fresh {
                return false;
            }
            if let Ok(mut required) = scope.required.lock() {
                required.get_or_insert(action);
            }
            true
        })
        .unwrap_or(false);
    if stale {
        tracing::info!(action = action.key(), "recent login required");
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            format!(
                "Confirm it's you: log in with EVE again to continue ({}).",
                action.label().to_lowercase()
            ),
        ))
    } else {
        Ok(())
    }
}

/// Actions on an account's own characters ([`Action::ChangeMain`],
/// [`Action::AddCharacter`], [`Action::MainToken`]): gated for the owner
/// and accounts holding a sensitive permission, since the main is what
/// confirms it's them. Everyone else does them freely.
pub async fn check_privileged(
    db: &tether_db::PgPool,
    account: tether_db::accounts::AccountId,
    action: Action,
) -> Result<(), AppError> {
    // Nothing to look up when it couldn't be refused anyway.
    if SCOPE.try_with(|scope| scope.fresh).unwrap_or(true) {
        return Ok(());
    }
    let owner = tether_db::accounts::get(db, account)
        .await?
        .is_some_and(|a| a.is_owner);
    let privileged = owner
        || tether_db::permissions::effective(db, account)
            .await?
            .iter()
            .any(|p| tether_core::permissions::is_sensitive(p));
    if privileged { check(action) } else { Ok(()) }
}

/// Runs a browser session's request in its sudo scope. When the request
/// was refused for want of a recent login, a page (or htmx) request goes
/// to the confirmation page instead, naming the action and the page it came
/// from; the JSON API keeps its 403.
pub(crate) async fn layer(
    state: &AppState,
    session: &CurrentSession,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let scope = Arc::new(Scope {
        fresh: is_fresh(session.reauthenticated_at),
        required: Mutex::new(None),
    });
    let api = request.uri().path().starts_with("/api/");
    let htmx = is_htmx(request.headers());
    let from = came_from(state, request.headers());
    let response = SCOPE.scope(scope.clone(), next.run(request)).await;
    let required = scope.required.lock().ok().and_then(|r| *r);
    match required {
        Some(action) if !api => {
            let url = format!(
                "/reauthenticate?action={}&return_to={}",
                action.key(),
                crate::pages::plugin_pages::encode(&from)
            );
            if htmx {
                // htmx would follow a redirect and swap the page into a
                // fragment's place: have it navigate instead.
                let mut response = StatusCode::OK.into_response();
                if let Ok(value) = HeaderValue::from_str(&url) {
                    response.headers_mut().insert("hx-redirect", value);
                }
                response
            } else {
                Redirect::to(&url).into_response()
            }
        }
        _ => response,
    }
}

/// The page a form was posted from: the Referer, when it's this site and a
/// safe path (browsers send it for same-origin requests: `Referrer-Policy:
/// same-origin`), else the Dashboard.
fn came_from(state: &AppState, headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|referer| referer.strip_prefix(state.site.origin()))
        .and_then(safe_path)
        .filter(|path| !path.starts_with("/reauthenticate"))
        .unwrap_or(FALLBACK)
        .to_owned()
}

#[derive(Debug, Deserialize)]
pub struct Confirm {
    #[serde(default)]
    action: String,
    #[serde(default)]
    return_to: String,
}

#[derive(Template)]
#[template(path = "reauthenticate.html")]
struct ConfirmPage<'a> {
    action_key: &'a str,
    label: &'a str,
    main: Option<String>,
    return_to: &'a str,
    minutes: u64,
}

/// `GET /reauthenticate?action=&return_to=`: "Confirm it's you".
pub async fn page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<Confirm>,
) -> Result<Response, PageError> {
    // Signed out: to log in (which lands back here).
    let session = session.ok_or_else(AppError::unauthorized)?;
    let action = Action::from_key(&query.action);
    let return_to = safe_path(&query.return_to).unwrap_or(FALLBACK);
    let main = tether_db::accounts::get(&state.db, session.account)
        .await?
        .and_then(|a| a.main)
        .map(|m| m.name);
    Ok(render(
        StatusCode::OK,
        &ConfirmPage {
            action_key: action.map_or("", Action::key),
            label: action.map_or("Continue", Action::label),
            main,
            return_to,
            minutes: WINDOW.as_secs() / 60,
        },
    ))
}

/// `POST /reauthenticate`: log in with EVE again, then back to `return_to`.
pub async fn start(
    State(state): State<AppState>,
    jar: CookieJar,
    session: CurrentSession,
    Form(form): Form<Confirm>,
) -> Result<Response, PageError> {
    if session.token_scopes.is_some() {
        return Err(AppError::forbidden().into());
    }
    let return_to = safe_path(&form.return_to).unwrap_or(FALLBACK);
    let action = Action::from_key(&form.action).map(|a| a.key().to_owned());
    Ok(crate::auth::start_login(
        &state,
        jar,
        return_to,
        Purpose::Reauth(action),
        &[],
        Some(session.account),
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_key(action.key()), Some(action));
        }
        assert_eq!(Action::from_key("nope"), None);
    }

    #[test]
    fn freshness() {
        let now = Utc::now();
        assert!(is_fresh(Some(now)));
        assert!(is_fresh(Some(now - chrono::TimeDelta::minutes(14))));
        assert!(!is_fresh(Some(now - chrono::TimeDelta::minutes(16))));
        assert!(!is_fresh(None));
        // A clock gone backwards doesn't grant forever.
        assert!(!is_fresh(Some(now + chrono::TimeDelta::hours(1))));
    }

    #[tokio::test]
    async fn only_stale_browser_sessions_are_refused() {
        // No scope (tokens, the CLI, jobs).
        assert!(check(Action::AppInstall).is_ok());
        let fresh = Arc::new(Scope {
            fresh: true,
            required: Mutex::new(None),
        });
        SCOPE
            .scope(fresh.clone(), async {
                assert!(check(Action::AppInstall).is_ok());
            })
            .await;
        assert_eq!(*fresh.required.lock().unwrap(), None);
        let stale = Arc::new(Scope {
            fresh: false,
            required: Mutex::new(None),
        });
        SCOPE
            .scope(stale.clone(), async {
                let err = check(Action::AppUninstall).unwrap_err();
                assert_eq!(err.status(), StatusCode::FORBIDDEN);
                assert!(err.message().contains("uninstall an app"));
                let _ = check(Action::AppKey);
            })
            .await;
        // The first refusal names the page.
        assert_eq!(*stale.required.lock().unwrap(), Some(Action::AppUninstall));
    }
}
