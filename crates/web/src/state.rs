use std::sync::Arc;

use tether_core::Secret;
use tether_core::crypto::EncryptionKey;
use tether_db::PgPool;
use tether_discord::Discord;
use tether_esi::Esi;

use crate::ratelimit::RateLimiter;
use tether_esi::sso::Sso;
use tether_esi::vault::TokenVault;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub esi: Esi,
    pub sso: Arc<dyn Sso>,
    pub vault: Arc<TokenVault>,
    /// Seals instance secrets (the Discord bot token and client secret).
    pub key: EncryptionKey,
    pub discord: Arc<Discord>,
    pub site: Arc<Site>,
    /// Required by the first-run wizard until an owner exists.
    pub setup_token: Arc<Secret<String>>,
    pub limits: Arc<Limits>,
    /// Plugins running in this process.
    pub plugins: Arc<crate::plugins::Plugins>,
    /// Unread-count changes, for the live bell.
    pub notices: crate::notifications::Notices,
}

/// Rate limits for endpoints worth guessing at.
#[derive(Debug)]
pub struct Limits {
    pub setup_unlock: RateLimiter,
    /// Plugin form posts, per account and plugin.
    pub plugin_submits: RateLimiter<(i64, String)>,
    /// Plugin page views, per account and plugin: each runs the plugin.
    pub plugin_pages: RateLimiter<(i64, String)>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            setup_unlock: RateLimiter::new(5, std::time::Duration::from_secs(60)),
            plugin_submits: RateLimiter::new(30, std::time::Duration::from_secs(60)),
            plugin_pages: RateLimiter::new(120, std::time::Duration::from_secs(60)),
        }
    }
}

/// Where this instance is reachable from browsers.
#[derive(Debug)]
pub struct Site {
    public_url: String,
    origin: String,
}

impl Site {
    /// `public_url` is e.g. `https://auth.example.com` (no trailing slash).
    pub fn new(public_url: impl Into<String>) -> Self {
        let public_url: String = public_url.into();
        let public_url = public_url.trim_end_matches('/').to_owned();
        // Origin is scheme://host[:port], i.e. everything before any path.
        let after_scheme = public_url.find("://").map_or(0, |i| i + 3);
        let origin = match public_url[after_scheme..].find('/') {
            Some(i) => public_url[..after_scheme + i].to_owned(),
            None => public_url.clone(),
        };
        Self { public_url, origin }
    }

    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn sso_callback_url(&self) -> String {
        format!("{}/auth/callback", self.public_url)
    }

    /// The redirect registered with the Discord application.
    pub fn discord_callback_url(&self) -> String {
        format!("{}/discord/callback", self.public_url)
    }
}

#[cfg(test)]
mod tests {
    use super::Site;

    #[test]
    fn origin_drops_any_path() {
        assert_eq!(
            Site::new("https://a.example.com/").origin(),
            "https://a.example.com"
        );
        assert_eq!(
            Site::new("http://localhost:8080").origin(),
            "http://localhost:8080"
        );
        assert_eq!(Site::new("https://x.io/tether").origin(), "https://x.io");
        assert_eq!(
            Site::new("https://a.example.com").sso_callback_url(),
            "https://a.example.com/auth/callback"
        );
    }
}
