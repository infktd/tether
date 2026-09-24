//! The `tether` binary: runs the server and hosts the admin CLI.

mod config;

use std::io::IsTerminal;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use config::ServeConfig;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        Some(Command::Serve(config)) => serve(config).await,
        None => match cli.serve {
            Some(config) => serve(config).await,
            None => anyhow::bail!("missing configuration; see `tether --help`"),
        },
    }
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

    let esi = tether_esi::Esi::new(
        &format!(
            "tether/{} (+{})",
            env!("CARGO_PKG_VERSION"),
            config.public_url()
        ),
        None,
    )?;

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

    let state = tether_web::AppState {
        db,
        esi,
        setup_token: std::sync::Arc::new(setup_token),
        limits: std::sync::Arc::default(),
        sso: std::sync::Arc::new(tether_esi::sso::EveSso),
        site: std::sync::Arc::new(tether_web::Site::new(config.public_url())),
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
