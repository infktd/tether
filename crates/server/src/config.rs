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

    /// One-time token required by the first-run wizard.
    #[arg(long, env = "SETUP_TOKEN", hide_env_values = true)]
    pub setup_token: Option<Secret<String>>,
}

impl ServeConfig {
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
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct Wrapper {
        #[command(flatten)]
        config: ServeConfig,
    }

    fn parse(args: &[&str]) -> Result<ServeConfig, clap::Error> {
        let mut argv = vec!["tether"];
        argv.extend_from_slice(args);
        Wrapper::try_parse_from(argv).map(|w| w.config)
    }

    #[test]
    fn defaults_public_url_to_https_domain() {
        let config = parse(&[
            "--database-url",
            "postgres://u:p@db/x",
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
        let base = ["--database-url", "postgres://u:p@db/x", "--domain", "d"];
        let ok = parse(&[&base[..], &["--public-url", "http://localhost:8080/"]].concat()).unwrap();
        assert_eq!(ok.public_url(), "http://localhost:8080");
        assert!(parse(&[&base[..], &["--public-url", "localhost"]].concat()).is_err());
    }

    #[test]
    fn debug_output_hides_secrets() {
        let config = parse(&[
            "--database-url",
            "postgres://u:hunter2@db/x",
            "--domain",
            "d",
            "--setup-token",
            "tok-hunter3",
        ])
        .unwrap();
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("hunter2") && !debug.contains("hunter3"),
            "{debug}"
        );
    }

    #[test]
    fn database_url_and_domain_are_required() {
        assert!(parse(&["--domain", "d"]).is_err());
        assert!(parse(&["--database-url", "postgres://x"]).is_err());
    }
}
