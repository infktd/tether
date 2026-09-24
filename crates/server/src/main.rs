//! The `tether` binary: runs the server and hosts the admin CLI.

mod config;

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use config::{ServeConfig, ToolConfig};

#[derive(Parser)]
#[command(name = "tether", version, about = "EVE Online alliance platform")]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// With no subcommand, `tether` runs the server.
    #[command(flatten)]
    serve: Option<ServeConfig>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (the default).
    Serve(ServeConfig),
    /// Check DNS, ports, TLS, database, ESI and SSO, with a fix for each
    /// problem.
    Doctor,
    #[command(flatten)]
    Admin(tether_cli::Command),
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx::postgres::notice=warn".into()),
        )
        // No color codes in `docker compose logs` or other non-terminals.
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve(config)) => serve(config).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Doctor) => doctor().await,
        Some(Command::Admin(command)) => admin(command).await.map(|()| ExitCode::SUCCESS),
        None => match cli.serve {
            Some(config) => serve(config).await.map(|()| ExitCode::SUCCESS),
            None => anyhow::bail!("missing configuration; see `tether --help`"),
        },
    }
}

/// Connection for one-off commands: small pool, no long retry loop.
async fn tool_context() -> anyhow::Result<(ToolConfig, tether_db::PgPool, tether_esi::Esi)> {
    let config = ToolConfig::try_parse_from(["tether"])?;
    let db = tether_db::connect(
        &config.database_url,
        &tether_db::ConnectOptions {
            max_connections: 2,
            attempts: 1,
            ..Default::default()
        },
    )
    .await?;
    let esi = tether_esi::Esi::new(&user_agent(&config.public_url()), None)?;
    Ok((config, db, esi))
}

async fn admin(command: tether_cli::Command) -> anyhow::Result<()> {
    let (_, db, esi) = tool_context().await?;
    tether_cli::run(command, &db, &esi, &mut std::io::stdout().lock()).await
}

async fn doctor() -> anyhow::Result<ExitCode> {
    let (config, db, esi) = tool_context().await?;
    let env = tether_cli::doctor::Env {
        db,
        esi,
        domain: config.domain.clone(),
        public_url: config.public_url(),
        sso_metadata_url: tether_cli::doctor::SSO_METADATA_URL.to_owned(),
        http_port: 80,
        https_port: 443,
    };
    let healthy = tether_cli::doctor::run(&env, &mut std::io::stdout().lock()).await?;
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn user_agent(public_url: &str) -> String {
    format!("tether/{} (+{public_url})", env!("CARGO_PKG_VERSION"))
}

async fn serve(config: ServeConfig) -> anyhow::Result<()> {
    config.validate().map_err(anyhow::Error::msg)?;
    if tether_web::DEV_LOGIN {
        tracing::warn!("dev-login is compiled in: /dev/login signs anyone in without SSO");
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        public_url = config.public_url(),
        "starting"
    );

    let db = tether_db::connect(
        &config.database_url,
        &tether_db::ConnectOptions {
            max_connections: config.database_max_connections,
            ..Default::default()
        },
    )
    .await?;
    tether_db::migrate(&db).await?;
    tracing::info!("database migrations applied");

    let esi = tether_esi::Esi::new(&user_agent(&config.public_url()), None)?;

    let mut registry = tether_jobs::Registry::new();
    tether_web::tiers::register_jobs(&mut registry, db.clone(), esi.clone());
    let workers = tether_jobs::WorkerPool::start(
        db.clone(),
        registry,
        tether_jobs::WorkerConfig {
            workers: config.job_workers,
            ..Default::default()
        },
    );

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("binding {}", config.listen))?;
    tracing::info!(listen = %config.listen, "listening");
    let setup_token = match config.setup_token.clone() {
        Some(token) => tether_core::Secret::new(token.expose().trim().to_owned()),
        None => tether_core::new_token()?,
    };
    if !tether_db::accounts::owner_exists(&db).await? {
        // Deliberate exception to "no secrets in logs": the fallback way to
        // find the token (F8). Stops once an owner exists.
        tracing::warn!(
            setup_url = format!("{}/setup", config.public_url()),
            setup_token = setup_token.expose(),
            "first-run setup: open the setup page and enter this token"
        );
    }

    let key = tether_core::crypto::EncryptionKey::from_hex(&config.encryption_key)?;
    let verifier = tether_esi::jwt::JwtVerifier::new(
        &user_agent(&config.public_url()),
        tether_esi::jwt::CCP_JWKS_URL,
    )?;
    let sso: std::sync::Arc<dyn tether_esi::sso::Sso> =
        std::sync::Arc::new(tether_esi::sso::EveSso::new(verifier));
    let site = tether_web::Site::new(config.public_url());
    let vault = std::sync::Arc::new(tether_esi::vault::TokenVault::new(
        db.clone(),
        key,
        sso.clone(),
        site.sso_callback_url(),
    ));
    let state = tether_web::AppState {
        vault,
        db,
        esi,
        setup_token: std::sync::Arc::new(setup_token),
        limits: std::sync::Arc::default(),
        sso,
        site: std::sync::Arc::new(site),
    };
    let app =
        tether_web::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving HTTP")?;

    // HTTP has drained; let running jobs finish before exiting.
    workers.shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "installing Ctrl-C handler");
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => tracing::error!(error = %err, "installing SIGTERM handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
