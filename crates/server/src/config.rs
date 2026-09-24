//! Configuration, read from the environment (flags work too, for local use).
//!
//! `.env` in a deployment holds only DOMAIN, POSTGRES_PASSWORD and
//! SETUP_TOKEN; compose turns those into the variables below. Everything else
//! is configured in the browser and stored in the database.

use std::net::SocketAddr;

use clap::Args;
use tether_core::Secret;

#[derive(Args, Debug)]
pub struct ServeConfig {
    /// Address the HTTP server binds to.
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    pub listen: SocketAddr,

    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: Secret<String>,

    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value_t = 10)]
    pub database_max_connections: u32,

    /// Background job workers (tokio tasks in this process).
    #[arg(long, env = "JOB_WORKERS", default_value_t = 4)]
    pub job_workers: usize,

    /// Public domain of this instance, e.g. auth.example.com.
    #[arg(long, env = "DOMAIN")]
    pub domain: String,

    /// Override the public base URL (default `https://{DOMAIN}`), e.g.
    /// `http://localhost:8080` when running without Caddy.
    #[arg(long, env = "PUBLIC_URL", value_parser = parse_public_url)]
    pub public_url: Option<String>,

    /// 64 hex characters (32 bytes) encrypting tokens and secrets at rest.
    /// install.sh generates it; it never enters the database.
    #[arg(long, env = "ENCRYPTION_KEY", hide_env_values = true)]
    pub encryption_key: Secret<String>,

    /// Token required by the first-run wizard until an owner exists. At
    /// least 32 characters; install.sh generates 64. If unset, one is
    /// generated at startup and logged.
    #[arg(long, env = "SETUP_TOKEN", hide_env_values = true)]
    pub setup_token: Option<Secret<String>>,
}

impl ServeConfig {
    /// Checks that clap can't express. Done after parsing because clap's own
    /// errors echo the rejected value, and these values are secrets.
    pub fn validate(&self) -> Result<(), String> {
        tether_core::crypto::EncryptionKey::from_hex(&self.encryption_key)
            .map_err(|err| err.to_string())?;
        if let Some(token) = &self.setup_token
            && token.expose().trim().len() < 32
        {
            // A short token means someone set it by hand: refuse, don't warn.
            return Err(format!(
                "SETUP_TOKEN must be at least 32 characters (got {}); deploy/install.sh generates a suitable one",
                token.expose().trim().len()
            ));
        }
        Ok(())
    }

    pub fn public_url(&self) -> String {
        self.public_url
            .clone()
            .unwrap_or_else(|| format!("https://{}", self.domain))
    }
}

/// What the admin commands need, read from the same environment as the
/// server, so `docker compose exec app tether <command>` just works.
#[derive(clap::Parser, Debug)]
pub struct ToolConfig {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: Secret<String>,

    #[arg(long, env = "DOMAIN")]
    pub domain: String,

    #[arg(long, env = "PUBLIC_URL", value_parser = parse_public_url)]
    pub public_url: Option<String>,
}

impl ToolConfig {
    pub fn public_url(&self) -> String {
        self.public_url
            .clone()
            .unwrap_or_else(|| format!("https://{}", self.domain))
    }
}

fn parse_public_url(value: &str) -> Result<String, String> {
    let trimmed = value.trim_end_matches('/');
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        Ok(trimmed.to_owned())
    } else {
        Err("must start with https:// or http://".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches, Parser};

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[derive(Parser, Debug)]
    struct Wrapper {
        #[command(flatten)]
        config: ServeConfig,
    }

    /// Parses flags only. Env lookups are switched off so tests don't depend
    /// on the machine (CI's test job sets DATABASE_URL, for one).
    fn parse(args: &[&str]) -> Result<ServeConfig, clap::Error> {
        let mut argv = vec!["tether"];
        argv.extend_from_slice(args);
        let matches = Wrapper::command()
            .mut_args(|arg| arg.env(None::<&str>))
            .try_get_matches_from(argv)?;
        Wrapper::from_arg_matches(&matches).map(|w| w.config)
    }

    #[test]
    fn defaults_public_url_to_https_domain() {
        let config = parse(&[
            "--database-url",
            "postgres://u:p@db/x",
            "--encryption-key",
            KEY,
            "--domain",
            "auth.example.com",
        ])
        .unwrap();
        assert_eq!(config.public_url(), "https://auth.example.com");
        assert_eq!(config.listen.port(), 8080);
        assert!(config.setup_token.is_none());
    }

    #[test]
    fn public_url_override_is_validated_and_trimmed() {
        let base = [
            "--database-url",
            "postgres://u:p@db/x",
            "--encryption-key",
            KEY,
            "--domain",
            "d",
        ];
        let ok = parse(&[&base[..], &["--public-url", "http://localhost:8080/"]].concat()).unwrap();
        assert_eq!(ok.public_url(), "http://localhost:8080");
        assert!(parse(&[&base[..], &["--public-url", "localhost"]].concat()).is_err());
    }

    #[test]
    fn debug_output_hides_secrets() {
        let config = parse(&[
            "--database-url",
            "postgres://u:hunter2@db/x",
            "--encryption-key",
            KEY,
            "--domain",
            "d",
            "--setup-token",
            "tok-hunter3-0123456789abcdef0123456789",
        ])
        .unwrap();
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("hunter2") && !debug.contains("hunter3"),
            "{debug}"
        );
    }

    #[test]
    fn short_setup_token_is_refused() {
        let base = [
            "--database-url",
            "postgres://x",
            "--encryption-key",
            KEY,
            "--domain",
            "d",
        ];
        let short = parse(&[&base[..], &["--setup-token", "hunter2"]].concat()).unwrap();
        let err = short.validate().unwrap_err();
        assert!(err.contains("at least 32 characters"), "{err}");
        assert!(
            !err.contains("hunter2"),
            "the token must not be echoed: {err}"
        );
        let long = "a".repeat(32);
        let long = parse(&[&base[..], &["--setup-token", &long]].concat()).unwrap();
        assert!(long.validate().is_ok());
        assert!(parse(&base).unwrap().validate().is_ok());
    }

    #[test]
    fn bad_encryption_key_is_refused_without_echo() {
        let config = parse(&[
            "--database-url",
            "postgres://x",
            "--domain",
            "d",
            "--encryption-key",
            "hunter2-not-hex",
        ])
        .unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.contains("64 hex characters"), "{err}");
        assert!(!err.contains("hunter2"), "{err}");
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn database_url_and_domain_are_required() {
        assert!(parse(&["--domain", "d"]).is_err());
        assert!(parse(&["--database-url", "postgres://x"]).is_err());
    }
}
