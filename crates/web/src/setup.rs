//! First-run wizard (F8).
//!
//! 1. Enter the setup token from `.env` (also printed to the logs at
//!    startup). This starts a short setup session in this browser.
//! 2. Enter the EVE SSO client id; the wizard shows the exact callback URL
//!    to register with CCP and can check the instance is reachable at it.
//! 3. Log in with EVE SSO from the same browser: that account becomes the
//!    owner, and the wizard's token step is disabled for good.
//! 4. As owner, choose the Member alliance (`/api/admin/tiers`).

use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tether_core::permissions::ADMIN_TIERS;
use tether_core::tiers::EntityKind;
use tether_core::{hash_token, new_token};
use tether_db::audit::{self, Actor};
use tether_db::{accounts, settings, setup, tiers as tier_db};

use crate::AppState;
use crate::auth::{CurrentSession, cookie};
use crate::error::AppError;
use crate::ratelimit::client_ip;

pub const SETUP_COOKIE: &str = "__Host-tether_setup";
const SETUP_TTL: Duration = Duration::from_secs(60 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    /// No SSO client id yet: unlock with the setup token and enter it.
    NeedsSso,
    /// SSO works; log in from the unlocked browser to become owner.
    NeedsOwner,
    /// The owner exists; choose which alliance or corporation is Member.
    NeedsAlliance,
    Complete,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SetupStatus {
    pub state: SetupState,
    /// Register exactly this with CCP.
    pub callback_url: String,
    pub sso_configured: bool,
    pub owner_exists: bool,
    /// This browser holds a valid setup session.
    pub unlocked: bool,
    /// For admins at the alliance step: the owner main's own alliance (or
    /// corporation if it has no alliance).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested: Option<Suggestion>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Suggestion {
    pub id: i64,
    pub name: String,
    pub kind: &'static str,
}

/// `GET /api/setup`: public; says which step comes next.
#[utoipa::path(get, path = "/api/setup", tag = "setup",
    responses((status = 200, body = SetupStatus)))]
pub async fn status(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
) -> Result<Json<SetupStatus>, AppError> {
    let sso_configured = settings::get_string(&state.db, settings::SSO_CLIENT_ID)
        .await?
        .is_some();
    let owner_exists = accounts::owner_exists(&state.db).await?;
    let has_member_rule = tier_db::list_rules(&state.db)
        .await?
        .iter()
        .any(|r| r.tier == tether_core::tiers::Tier::Member);
    let setup_state = match (sso_configured, owner_exists, has_member_rule) {
        (false, false, _) => SetupState::NeedsSso,
        (true, false, _) => SetupState::NeedsOwner,
        (_, true, false) => SetupState::NeedsAlliance,
        (_, true, true) => SetupState::Complete,
    };

    let mut suggested = None;
    if let (SetupState::NeedsAlliance, Some(session)) = (&setup_state, &session)
        && session.require(&state, ADMIN_TIERS).await.is_ok()
    {
        suggested = suggestion(&state, session).await;
    }

    Ok(Json(SetupStatus {
        state: setup_state,
        callback_url: state.site.sso_callback_url(),
        sso_configured,
        owner_exists,
        unlocked: !owner_exists && has_setup_session(&state, &jar).await?,
        suggested,
    }))
}

async fn suggestion(state: &AppState, session: &CurrentSession) -> Option<Suggestion> {
    let affiliation = tier_db::main_affiliation(&state.db, session.account)
        .await
        .ok()??;
    let id = affiliation
        .alliance_id
        .unwrap_or(affiliation.corporation_id);
    let entity = state
        .esi
        .names(&[id])
        .await
        .ok()?
        .into_iter()
        .find(|e| e.id == id)?;
    Some(Suggestion {
        id: entity.id,
        name: entity.name,
        kind: entity.kind.map_or("unknown", EntityKind::as_str),
    })
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UnlockIn {
    pub token: String,
}

/// `POST /api/setup/unlock`: exchange the setup token for a setup session.
#[utoipa::path(post, path = "/api/setup/unlock", tag = "setup", request_body = UnlockIn,
    responses((status = 204, description = "Unlocked; sets the setup cookie"),
              (status = 403, description = "Wrong token"),
              (status = 410, description = "Setup is finished"),
              (status = 429, description = "Too many attempts from this IP")))]
pub async fn unlock(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Json(body): Json<UnlockIn>,
) -> Result<Response, AppError> {
    // Cheap insurance on top of a 256-bit token: 5 attempts a minute per IP.
    if let Some(ip) = ip
        && let Err(retry_after) = state
            .limits
            .setup_unlock
            .check(ip, std::time::Instant::now())
    {
        tracing::warn!(%ip, "setup unlock rate limited");
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            "Too many attempts. Wait a minute and try again.",
        )
            .into_response());
    }
    if accounts::owner_exists(&state.db).await? {
        return Err(finished());
    }
    // Comparing hashes keeps the comparison time independent of the token.
    if hash_token(body.token.trim()) != hash_token(state.setup_token.expose()) {
        tracing::warn!("wrong setup token entered");
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "That setup token is not correct.",
        ));
    }
    let session = new_token().map_err(AppError::internal)?;
    setup::start_session(&state.db, &hash_token(session.expose()), SETUP_TTL).await?;
    audit::record(&state.db, Actor::System, "setup.unlock", None, json!({})).await?;
    let jar = jar.add(cookie(SETUP_COOKIE, &session, SETUP_TTL)?);
    Ok((jar, StatusCode::NO_CONTENT).into_response())
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SsoIn {
    pub client_id: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SsoOut {
    pub callback_url: String,
}

/// `POST /api/setup/sso`: set the EVE SSO client id.
#[utoipa::path(post, path = "/api/setup/sso", tag = "setup", request_body = SsoIn,
    responses((status = 200, body = SsoOut), (status = 400), (status = 401), (status = 403)))]
pub async fn set_sso(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
    Json(body): Json<SsoIn>,
) -> Result<Json<SsoOut>, AppError> {
    let actor = setup_actor(&state, &jar, session.as_ref()).await?;
    let client_id = body.client_id.trim();
    let valid = (16..=64).contains(&client_id.len())
        && client_id.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err(AppError::bad_request(
            "That doesn't look like an EVE SSO client id (16 to 64 letters and digits).",
        ));
    }
    let mut tx = state.db.begin().await?;
    settings::set(&mut *tx, settings::SSO_CLIENT_ID, client_id.into()).await?;
    audit::record(
        &mut *tx,
        actor,
        "setup.sso",
        None,
        json!({ "client_id": client_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(SsoOut {
        callback_url: state.site.sso_callback_url(),
    }))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ProbeQuery {
    pub nonce: String,
}

/// `GET /api/setup/probe?nonce=`: echoes the nonce so the callback check
/// can tell this instance answered.
#[utoipa::path(get, path = "/api/setup/probe", tag = "setup", params(ProbeQuery),
    responses((status = 200, body = String, content_type = "text/plain"), (status = 400)))]
pub async fn probe(Query(query): Query<ProbeQuery>) -> Result<String, AppError> {
    let ok = !query.nonce.is_empty()
        && query.nonce.len() <= 64
        && query.nonce.chars().all(|c| c.is_ascii_alphanumeric());
    if !ok {
        return Err(AppError::bad_request("Bad nonce."));
    }
    Ok(probe_body(&query.nonce))
}

fn probe_body(nonce: &str) -> String {
    format!("tether-probe:{nonce}")
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CheckOut {
    pub ok: bool,
    pub url: String,
    pub detail: String,
}

/// `POST /api/setup/callback-check`: fetch our own public URL the way CCP's
/// redirect will reach it (DNS, TLS, reverse proxy) and confirm this
/// instance answers. Matching the URL registered at CCP is proven by the
/// first login.
#[utoipa::path(post, path = "/api/setup/callback-check", tag = "setup",
    responses((status = 200, body = CheckOut), (status = 401), (status = 403)))]
pub async fn callback_check(
    State(state): State<AppState>,
    jar: CookieJar,
    session: Option<CurrentSession>,
) -> Result<Json<CheckOut>, AppError> {
    setup_actor(&state, &jar, session.as_ref()).await?;
    let (ok, detail) = check_public_url(state.site.public_url())
        .await
        .map_err(AppError::internal)?;
    Ok(Json(CheckOut {
        ok,
        url: state.site.public_url().to_owned(),
        detail,
    }))
}

/// Fetches `{public_url}/api/setup/probe` and checks this instance (not
/// some other server) answered. `Ok((reachable, explanation))`; `Err` only
/// if no nonce could be generated.
pub async fn check_public_url(public_url: &str) -> Result<(bool, String), getrandom::Error> {
    let nonce = new_token()?;
    let nonce = &nonce.expose()[..32];
    let url = format!("{public_url}/api/setup/probe?nonce={nonce}");
    Ok(match fetch(&url).await {
        Ok((status, body)) if status == reqwest::StatusCode::OK && body == probe_body(nonce) => {
            (true, "This instance answered at its public URL.".to_owned())
        }
        Ok((status, _)) => (
            false,
            format!(
                "Something answered with HTTP {status}, but it wasn't this instance. Check DNS and the reverse proxy."
            ),
        ),
        Err(err) => (false, format!("Could not reach the public URL: {err}")),
    })
}

async fn fetch(url: &str) -> Result<(reqwest::StatusCode, String), String> {
    let client = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(|e| error_chain(&e))?;
    let status = response.status();
    let body = response.text().await.map_err(|e| error_chain(&e))?;
    Ok((status, body))
}

/// "outer: inner: root cause", which is where the useful part (DNS, TLS,
/// refused) usually is.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

/// Who may change setup: the unlocked browser before an owner exists, and
/// only the owner afterwards.
async fn setup_actor(
    state: &AppState,
    jar: &CookieJar,
    session: Option<&CurrentSession>,
) -> Result<Actor, AppError> {
    if accounts::owner_exists(&state.db).await? {
        let session = session.ok_or_else(AppError::unauthorized)?;
        let account = accounts::get(&state.db, session.account)
            .await?
            .ok_or_else(AppError::unauthorized)?;
        return if account.is_owner {
            Ok(Actor::Account(session.account))
        } else {
            Err(AppError::forbidden())
        };
    }
    if has_setup_session(state, jar).await? {
        Ok(Actor::System)
    } else {
        Err(AppError::new(
            StatusCode::UNAUTHORIZED,
            "Enter the setup token first.",
        ))
    }
}

pub async fn has_setup_session(state: &AppState, jar: &CookieJar) -> Result<bool, AppError> {
    match jar.get(SETUP_COOKIE) {
        Some(c) => Ok(setup::session_valid(&state.db, &hash_token(c.value())).await?),
        None => Ok(false),
    }
}

/// The requesting client's IP, if known (see [`client_ip`]).
pub struct ClientIp(pub Option<std::net::IpAddr>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(client_ip(parts)))
    }
}

fn finished() -> AppError {
    AppError::new(
        StatusCode::GONE,
        "Setup is finished; the owner manages this instance.",
    )
}
