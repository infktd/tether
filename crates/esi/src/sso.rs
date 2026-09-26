//! EVE SSO login (OAuth2 authorization code with PKCE).
//!
//! A plain login asks for no scopes; granting a plugin ESI access asks for
//! the scopes it needs. The tokens a login returns go to the token vault
//! (`crate::vault`).

use std::future::Future;
use std::pin::Pin;
use std::time::SystemTime;

use eve_esi_client::auth::SsoClient;
use tether_core::Secret;

#[derive(Debug, Clone)]
pub struct SsoConfig {
    pub client_id: String,
    /// Must exactly match the callback registered with CCP.
    pub redirect_uri: String,
}

/// Where to send the browser, plus what the callback needs to finish.
#[derive(Debug)]
pub struct PendingLogin {
    pub authorize_url: String,
    pub state: String,
    pub pkce_verifier: Secret<String>,
}

/// Tokens from a login or refresh. Never logged: `Secret` redacts them.
#[derive(Debug, Clone)]
pub struct SsoTokens {
    pub access_token: Secret<String>,
    /// SSO may rotate this on refresh; `None` means keep the old one.
    pub refresh_token: Option<Secret<String>>,
    pub expires_at: Option<SystemTime>,
    /// From a refresh: the owner hash in the new (verified) access token,
    /// so a sold character is caught on refresh, as AA does.
    pub owner_hash: Option<String>,
}

/// A login, from a verified access token.
#[derive(Debug, Clone)]
pub struct SsoIdentity {
    pub character_id: i64,
    pub character_name: String,
    /// CCP's owner hash: changes when the character changes EVE account.
    pub owner_hash: String,
    /// What the character granted.
    pub scopes: Vec<String>,
    pub tokens: SsoTokens,
}

#[derive(Debug, thiserror::Error)]
pub enum SsoError {
    #[error("SSO configuration is invalid: {0}")]
    Config(String),
    #[error("EVE SSO token exchange failed: {0}")]
    Exchange(String),
    #[error("the SSO token did not identify a character")]
    NoCharacter,
    /// The refresh token is dead (`invalid_grant` and friends): the user
    /// must log in with the character again.
    #[error("EVE SSO revoked the token: {0}")]
    Revoked(String),
    /// Worth retrying later.
    #[error("EVE SSO unavailable: {0}")]
    Unavailable(String),
}

pub type SsoFuture<'a> = Pin<Box<dyn Future<Output = Result<SsoIdentity, SsoError>> + Send + 'a>>;
pub type RefreshFuture<'a> = Pin<Box<dyn Future<Output = Result<SsoTokens, SsoError>> + Send + 'a>>;

/// The login provider. A trait so tests can swap in a fake.
///
/// TODO(eve-esi-client): once it re-exports its oauth2 types and allows
/// overriding the SSO URLs, test `EveSso` itself against wiremock instead of
/// only testing the web flow with a fake.
pub trait Sso: Send + Sync {
    /// Starts a login asking for `scopes` (none for a plain login).
    fn begin(&self, config: &SsoConfig, scopes: &[String]) -> Result<PendingLogin, SsoError>;

    fn finish<'a>(
        &'a self,
        config: &'a SsoConfig,
        code: String,
        pkce_verifier: Secret<String>,
    ) -> SsoFuture<'a>;

    /// Exchanges a refresh token for a new access token.
    fn refresh<'a>(
        &'a self,
        config: &'a SsoConfig,
        refresh_token: Secret<String>,
    ) -> RefreshFuture<'a>;
}

/// Only errors about this one token mean it's dead. Errors about the whole
/// application (`invalid_client`, `unauthorized_client`, `access_denied`:
/// a wrong client id, or CCP suspending the app) say nothing about the
/// character, and must never be taken as every token being revoked.
fn refresh_error(err: eve_esi_client::auth::AuthError) -> SsoError {
    use eve_esi_client::auth::AuthError;
    match &err {
        AuthError::Rejected { error, .. }
            if error == "invalid_grant" || error == "invalid_token" =>
        {
            SsoError::Revoked(err.to_string())
        }
        AuthError::NoRefreshToken => SsoError::Revoked(err.to_string()),
        _ => SsoError::Unavailable(err.to_string()),
    }
}

fn tokens_from(set: eve_esi_client::auth::TokenSet) -> SsoTokens {
    SsoTokens {
        access_token: Secret::new(set.access_token),
        refresh_token: set.refresh_token.map(Secret::new),
        expires_at: set.expires_at,
        owner_hash: None,
    }
}

/// The real EVE SSO, via eve-esi-client, with tokens verified against
/// CCP's JWKS.
#[derive(Debug)]
pub struct EveSso {
    verifier: crate::jwt::JwtVerifier,
}

impl EveSso {
    pub fn new(verifier: crate::jwt::JwtVerifier) -> Self {
        Self { verifier }
    }

    fn client(config: &SsoConfig) -> Result<SsoClient, SsoError> {
        tether_net::install_crypto_provider();
        SsoClient::new(config.client_id.clone(), &config.redirect_uri)
            .map_err(|err| SsoError::Config(err.to_string()))
    }
}

impl Sso for EveSso {
    fn begin(&self, config: &SsoConfig, scopes: &[String]) -> Result<PendingLogin, SsoError> {
        let pending = Self::client(config)?.authorize(scopes.iter().cloned());
        Ok(PendingLogin {
            authorize_url: pending.url,
            state: pending.csrf_state.secret().clone(),
            pkce_verifier: Secret::new(pending.pkce_verifier.secret().clone()),
        })
    }

    fn finish<'a>(
        &'a self,
        config: &'a SsoConfig,
        code: String,
        pkce_verifier: Secret<String>,
    ) -> SsoFuture<'a> {
        Box::pin(async move {
            let verifier = oauth2::PkceCodeVerifier::new(pkce_verifier.expose().clone());
            let tokens = Self::client(config)?
                .exchange(code, verifier)
                .await
                .map_err(|err| SsoError::Exchange(err.to_string()))?;
            let verified = self
                .verifier
                .verify(&tokens.access_token, &config.client_id)
                .await
                .map_err(|err| SsoError::Exchange(format!("token failed verification: {err}")))?;
            Ok(SsoIdentity {
                character_id: verified.character_id,
                character_name: verified.character_name,
                owner_hash: verified.owner_hash,
                scopes: verified.scopes,
                tokens: tokens_from(tokens),
            })
        })
    }

    fn refresh<'a>(
        &'a self,
        config: &'a SsoConfig,
        refresh_token: Secret<String>,
    ) -> RefreshFuture<'a> {
        Box::pin(async move {
            let set = Self::client(config)?
                .refresh(refresh_token.expose())
                .await
                .map_err(refresh_error)?;
            let mut tokens = tokens_from(set);
            // A token that doesn't verify isn't proof of a sale: treat it as
            // SSO trouble, not a revocation.
            let verified = self
                .verifier
                .verify(tokens.access_token.expose(), &config.client_id)
                .await
                .map_err(|err| {
                    SsoError::Unavailable(format!("token failed verification: {err}"))
                })?;
            tokens.owner_hash = Some(verified.owner_hash);
            Ok(tokens)
        })
    }
}

#[cfg(test)]
mod tests {
    use eve_esi_client::auth::AuthError;

    use super::{SsoError, refresh_error};

    fn rejected(error: &str) -> AuthError {
        AuthError::Rejected {
            error: error.to_owned(),
            description: None,
        }
    }

    #[test]
    fn only_errors_about_the_token_are_revocations() {
        for dead in ["invalid_grant", "invalid_token"] {
            assert!(
                matches!(refresh_error(rejected(dead)), SsoError::Revoked(_)),
                "{dead}"
            );
        }
        // About the whole app (a wrong client id, a suspension): never a
        // reason to drop every character.
        for app in ["invalid_client", "unauthorized_client", "access_denied"] {
            assert!(
                matches!(refresh_error(rejected(app)), SsoError::Unavailable(_)),
                "{app}"
            );
        }
    }
}
