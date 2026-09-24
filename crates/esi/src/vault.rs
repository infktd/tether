//! The token vault (F9): the only place refresh tokens are decrypted.
//!
//! Refresh tokens are stored encrypted (`core.character_tokens`). Access
//! tokens live only in memory and are refreshed shortly before they expire,
//! one refresh per character at a time. A refresh that SSO rejects for good
//! marks the token revoked; the user fixes it by logging in with the
//! character again. Plugins never see any of this (N8).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::json;
use tether_core::Secret;
use tether_core::crypto::{CryptoError, EncryptionKey};
use tether_db::audit::{self, Actor};
use tether_db::tokens::{self, TokenState};
use tether_db::{PgPool, settings};

use crate::sso::{Sso, SsoConfig, SsoError, SsoTokens};

/// Refresh when less than this is left, so a token never expires mid-use.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("no token stored for this character")]
    NoToken,
    #[error("the character hasn't granted: {}", .0.join(", "))]
    MissingScopes(Vec<String>),
    #[error("the token was revoked; the character must log in again")]
    Revoked,
    #[error("EVE SSO isn't configured")]
    NotConfigured,
    #[error("EVE SSO unavailable: {0}")]
    Unavailable(String),
    #[error("stored token can't be decrypted (was ENCRYPTION_KEY changed?)")]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

struct Cached {
    token: Secret<String>,
    expires_at: SystemTime,
}

pub struct TokenVault {
    db: PgPool,
    key: EncryptionKey,
    sso: Arc<dyn Sso>,
    callback_url: String,
    cache: Mutex<HashMap<i64, Cached>>,
    refreshing: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for TokenVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenVault").finish_non_exhaustive()
    }
}

fn context(character_id: i64) -> String {
    format!("token:{character_id}")
}

impl TokenVault {
    pub fn new(db: PgPool, key: EncryptionKey, sso: Arc<dyn Sso>, callback_url: String) -> Self {
        Self {
            db,
            key,
            sso,
            callback_url,
            cache: Mutex::default(),
            refreshing: Mutex::default(),
        }
    }

    /// Stores the tokens from a login. `scopes` are what the character
    /// granted.
    pub async fn store(
        &self,
        character_id: i64,
        tokens: &SsoTokens,
        scopes: &[String],
    ) -> Result<(), VaultError> {
        if let Some(refresh) = &tokens.refresh_token {
            let sealed = self.key.seal(refresh, &context(character_id))?;
            tokens::upsert(&self.db, character_id, &sealed, scopes).await?;
        }
        self.remember(character_id, tokens);
        Ok(())
    }

    /// A valid access token carrying `required` scopes, refreshing if needed.
    pub async fn access_token(
        &self,
        character_id: i64,
        required: &[&str],
    ) -> Result<Secret<String>, VaultError> {
        let stored = tokens::get(&self.db, character_id)
            .await?
            .ok_or(VaultError::NoToken)?;
        if stored.state == TokenState::Revoked {
            return Err(VaultError::Revoked);
        }
        let missing: Vec<String> = required
            .iter()
            .filter(|s| !stored.scopes.iter().any(|have| have == *s))
            .map(|s| (*s).to_owned())
            .collect();
        if !missing.is_empty() {
            return Err(VaultError::MissingScopes(missing));
        }
        if let Some(token) = self.cached(character_id) {
            return Ok(token);
        }

        // One refresh per character: others wait, then reuse its result.
        let lock = {
            let mut refreshing = self.refreshing.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(refreshing.entry(character_id).or_default())
        };
        let _guard = lock.lock().await;
        if let Some(token) = self.cached(character_id) {
            return Ok(token);
        }
        self.refresh(character_id).await
    }

    async fn refresh(&self, character_id: i64) -> Result<Secret<String>, VaultError> {
        let stored = tokens::get(&self.db, character_id)
            .await?
            .ok_or(VaultError::NoToken)?;
        if stored.state == TokenState::Revoked {
            return Err(VaultError::Revoked);
        }
        let refresh = self.key.open(&stored.sealed, &context(character_id))?;
        let config = self.config().await?;
        match self.sso.refresh(&config, refresh).await {
            Ok(fresh) => {
                let rotated = match &fresh.refresh_token {
                    Some(t) => Some(self.key.seal(t, &context(character_id))?),
                    None => None,
                };
                tokens::record_refresh(&self.db, character_id, rotated.as_deref()).await?;
                self.remember(character_id, &fresh);
                Ok(fresh.access_token)
            }
            Err(SsoError::Revoked(reason)) => {
                self.forget(character_id);
                let mut tx = self.db.begin().await?;
                if tokens::mark_revoked(&mut *tx, character_id, &reason).await? {
                    audit::record(
                        &mut *tx,
                        Actor::System,
                        "token.revoked",
                        Some(&format!("character:{character_id}")),
                        json!({ "reason": reason }),
                    )
                    .await?;
                }
                tx.commit().await?;
                tracing::warn!(character_id, reason, "refresh token revoked");
                Err(VaultError::Revoked)
            }
            Err(err) => Err(VaultError::Unavailable(err.to_string())),
        }
    }

    async fn config(&self) -> Result<SsoConfig, VaultError> {
        let client_id = settings::get_string(&self.db, settings::SSO_CLIENT_ID)
            .await?
            .ok_or(VaultError::NotConfigured)?;
        Ok(SsoConfig {
            client_id,
            redirect_uri: self.callback_url.clone(),
        })
    }

    fn cached(&self, character_id: i64) -> Option<Secret<String>> {
        let cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let entry = cache.get(&character_id)?;
        let fresh_enough = entry
            .expires_at
            .duration_since(SystemTime::now())
            .is_ok_and(|left| left > REFRESH_MARGIN);
        fresh_enough.then(|| entry.token.clone())
    }

    fn remember(&self, character_id: i64, tokens: &SsoTokens) {
        let Some(expires_at) = tokens.expires_at else {
            return;
        };
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.insert(
            character_id,
            Cached {
                token: tokens.access_token.clone(),
                expires_at,
            },
        );
    }

    fn forget(&self, character_id: i64) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.remove(&character_id);
    }
}
