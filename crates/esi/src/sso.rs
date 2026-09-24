//! EVE SSO login (OAuth2 authorization code with PKCE).
//!
//! Milestone 0 only needs to know which character logged in, so no scopes
//! are requested and no tokens are kept. The token vault is F9.

use std::future::Future;
use std::pin::Pin;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoIdentity {
    pub character_id: i64,
    pub character_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SsoError {
    #[error("SSO configuration is invalid: {0}")]
    Config(String),
    #[error("EVE SSO token exchange failed: {0}")]
    Exchange(String),
    #[error("the SSO token did not identify a character")]
    NoCharacter,
}

pub type SsoFuture<'a> = Pin<Box<dyn Future<Output = Result<SsoIdentity, SsoError>> + Send + 'a>>;

/// The login provider. A trait so tests can swap in a fake.
///
/// TODO(eve-esi-client): once it re-exports its oauth2 types and allows
/// overriding the SSO URLs, test `EveSso` itself against wiremock instead of
/// only testing the web flow with a fake.
pub trait Sso: Send + Sync {
    fn begin(&self, config: &SsoConfig) -> Result<PendingLogin, SsoError>;

    fn finish<'a>(
        &'a self,
        config: &'a SsoConfig,
        code: String,
        pkce_verifier: Secret<String>,
    ) -> SsoFuture<'a>;
}

/// The real EVE SSO, via eve-esi-client.
#[derive(Debug, Default)]
pub struct EveSso;

impl EveSso {
    fn client(config: &SsoConfig) -> Result<SsoClient, SsoError> {
        SsoClient::new(config.client_id.clone(), &config.redirect_uri)
            .map_err(|err| SsoError::Config(err.to_string()))
    }
}

impl Sso for EveSso {
    fn begin(&self, config: &SsoConfig) -> Result<PendingLogin, SsoError> {
        let pending = Self::client(config)?.authorize(std::iter::empty::<&str>());
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
            // TODO(milestone 1): verify the access token's signature against
            // CCP's JWKS and check iss, aud and exp. Milestone 0 reads the
            // claims unverified, which is acceptable only because the token
            // came straight from CCP's token endpoint over TLS.
            let character_id = tokens
                .character_id()
                .and_then(|id| i64::try_from(id).ok())
                .ok_or(SsoError::NoCharacter)?;
            let character_name = tokens.character_name().ok_or(SsoError::NoCharacter)?;
            // The tokens are dropped here; milestone 0 stores none.
            Ok(SsoIdentity {
                character_id,
                character_name,
            })
        })
    }
}
