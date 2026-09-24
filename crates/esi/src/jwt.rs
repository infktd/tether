//! Verifies EVE SSO access tokens against CCP's published keys (JWKS).
//!
//! A token is trusted only if its signature checks out with one of CCP's
//! current keys, it was issued by `login.eveonline.com`, it's addressed to
//! both this application's client id and "EVE Online", and it hasn't
//! expired. The claims give the character, CCP's owner hash (changes when
//! the character moves to another EVE account) and the granted scopes.

use std::time::{Duration, Instant};

use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

pub const CCP_JWKS_URL: &str = "https://login.eveonline.com/oauth/jwks";
const ISSUERS: [&str; 2] = ["login.eveonline.com", "https://login.eveonline.com"];
const EVE_AUDIENCE: &str = "EVE Online";
/// Keys are re-fetched after this, or sooner when a token names an
/// unknown key (CCP rotated).
const KEYS_TTL: Duration = Duration::from_secs(60 * 60);
/// Unknown key ids can't make us fetch more often than this.
const MIN_REFETCH: Duration = Duration::from_secs(60);
const LEEWAY_SECS: u64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("token header unreadable: {0}")]
    Header(String),
    #[error("token names key {0:?}, which CCP doesn't publish")]
    UnknownKey(String),
    #[error("token rejected: {0}")]
    Invalid(String),
    #[error("token isn't addressed to EVE Online")]
    NotEveAudience,
    #[error("token subject isn't a character: {0:?}")]
    NotACharacter(String),
    #[error("couldn't fetch CCP's signing keys: {0}")]
    Keys(String),
}

/// What a verified token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    pub character_id: i64,
    pub character_name: String,
    /// Changes when the character is transferred to another EVE account.
    pub owner_hash: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    name: String,
    owner: String,
    #[serde(default)]
    scp: Scopes,
    aud: Audience,
}

/// CCP sends `scp` as a string for one scope and an array for several.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum Scopes {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, value: &str) -> bool {
        match self {
            Self::One(a) => a == value,
            Self::Many(all) => all.iter().any(|a| a == value),
        }
    }
}

struct Keys {
    keys: Vec<Jwk>,
    fetched: Instant,
}

pub struct JwtVerifier {
    http: tether_net::Outbound,
    jwks_url: String,
    keys: RwLock<Option<Keys>>,
}

impl std::fmt::Debug for JwtVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtVerifier")
            .field("jwks_url", &self.jwks_url)
            .finish_non_exhaustive()
    }
}

impl JwtVerifier {
    /// `jwks_url` is [`CCP_JWKS_URL`] outside tests, and must be on the
    /// client's allow-list.
    pub fn new(http: tether_net::Outbound, jwks_url: impl Into<String>) -> Result<Self, JwtError> {
        let jwks_url = jwks_url.into();
        http.allowlist()
            .check(&jwks_url)
            .map_err(|e| JwtError::Keys(e.to_string()))?;
        Ok(Self {
            http,
            jwks_url,
            keys: RwLock::new(None),
        })
    }

    pub async fn verify(&self, token: &str, client_id: &str) -> Result<VerifiedToken, JwtError> {
        let header = decode_header(token).map_err(|e| JwtError::Header(e.to_string()))?;
        let kid = header.kid.clone().unwrap_or_default();
        let jwk = self.key(&kid).await?;
        let key = DecodingKey::from_jwk(&jwk).map_err(|e| JwtError::Invalid(e.to_string()))?;

        // The algorithm comes from CCP's key, never from the token, so a
        // token can't pick a weaker one (or "none").
        let alg = jwk
            .common
            .key_algorithm
            .and_then(|a| a.to_string().parse().ok())
            .ok_or_else(|| JwtError::Invalid(format!("key {kid:?} has no usable algorithm")))?;
        let mut validation = Validation::new(alg);
        validation.set_issuer(&ISSUERS);
        validation.set_audience(&[client_id]);
        validation.leeway = LEEWAY_SECS;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|e| JwtError::Invalid(e.to_string()))?
            .claims;
        if !claims.aud.contains(EVE_AUDIENCE) {
            return Err(JwtError::NotEveAudience);
        }
        let character_id = claims
            .sub
            .strip_prefix("CHARACTER:EVE:")
            .and_then(|id| id.parse().ok())
            .ok_or_else(|| JwtError::NotACharacter(claims.sub.clone()))?;
        Ok(VerifiedToken {
            character_id,
            character_name: claims.name,
            owner_hash: claims.owner,
            scopes: match claims.scp {
                Scopes::None => Vec::new(),
                Scopes::One(s) => vec![s],
                Scopes::Many(all) => all,
            },
        })
    }

    async fn key(&self, kid: &str) -> Result<Jwk, JwtError> {
        {
            let keys = self.keys.read().await;
            if let Some(keys) = keys.as_ref()
                && keys.fetched.elapsed() < KEYS_TTL
                && let Some(jwk) = find(&keys.keys, kid)
            {
                return Ok(jwk);
            }
        }
        let mut keys = self.keys.write().await;
        // Another request may have refreshed while we waited, and unknown
        // key ids mustn't trigger a fetch every time.
        let recent = keys
            .as_ref()
            .is_some_and(|k| k.fetched.elapsed() < MIN_REFETCH);
        if !recent {
            *keys = Some(Keys {
                keys: self.fetch().await?,
                fetched: Instant::now(),
            });
        }
        keys.as_ref()
            .and_then(|k| find(&k.keys, kid))
            .ok_or_else(|| JwtError::UnknownKey(kid.to_owned()))
    }

    async fn fetch(&self) -> Result<Vec<Jwk>, JwtError> {
        let body: serde_json::Value = self
            .http
            .get(&self.jwks_url)
            .map_err(|e| JwtError::Keys(e.to_string()))?
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| JwtError::Keys(e.to_string()))?
            .json()
            .await
            .map_err(|e| JwtError::Keys(e.to_string()))?;
        let raw = body
            .get("keys")
            .and_then(|k| k.as_array())
            .ok_or_else(|| JwtError::Keys("no keys in the JWKS".into()))?;
        // One key we can't parse mustn't take the others down with it.
        let keys: Vec<Jwk> = raw
            .iter()
            .filter_map(|k| serde_json::from_value(k.clone()).ok())
            .collect();
        tracing::info!(keys = keys.len(), "fetched CCP SSO signing keys");
        Ok(keys)
    }
}

fn find(keys: &[Jwk], kid: &str) -> Option<Jwk> {
    keys.iter()
        .find(|k| k.common.key_id.as_deref() == Some(kid))
        .cloned()
}
