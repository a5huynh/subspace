//! `subspace-server`: run the worker fleet and serve the console API.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use subspace::provider::Provider;
use subspace::provider::anthropic::AnthropicProvider;
use subspace_server::channel::router::DEFAULT_ROUTER_MODEL;
use subspace_server::store::SqliteStore;
use subspace_server::{Homes, WorkerManager, api};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "subspace-server",
    version,
    about = "Run Subspace workers and serve the console API"
)]
struct Cli {
    /// Data directory (worker homes, global skills, database) [default: ~/.subspace]
    #[arg(long, env = "SUBSPACE_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Address to listen on. There is no authentication: keep this on loopback.
    #[arg(long, env = "SUBSPACE_BIND", default_value = "127.0.0.1:7700")]
    bind: SocketAddr,

    /// Model that picks who answers channel posts mentioning nobody
    #[arg(long, env = "SUBSPACE_ROUTER_MODEL", default_value = DEFAULT_ROUTER_MODEL)]
    router_model: String,

    /// Anthropic API key
    #[arg(long, env = "ANTHROPIC_API_KEY", hide_env_values = true)]
    anthropic_api_key: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let data_dir = match cli.data_dir {
        Some(dir) => dir,
        None => dirs::home_dir()
            .context("could not determine the home directory; pass --data-dir")?
            .join(".subspace"),
    };
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data dir {}", data_dir.display()))?;

    let db_url = format!("sqlite://{}", data_dir.join("subspace.db").display());
    let store = Arc::new(
        SqliteStore::connect(&db_url)
            .await
            .context("opening database")?,
    );
    let homes = Homes::open(&data_dir)?;
    let provider: Arc<dyn Provider> = Arc::new(AnthropicProvider::new(cli.anthropic_api_key));

    let manager = Arc::new(
        WorkerManager::with_persistence(provider, store, homes)
            .with_router_model(&cli.router_model),
    );
    let started = manager.boot().await.context("restoring workers")?;
    tracing::info!(
        "data dir {} ({started} workers restored); routing with {}",
        data_dir.display(),
        cli.router_model
    );

    if !cli.bind.ip().is_loopback() {
        tracing::warn!(
            "listening on non-loopback address {}: the API has no authentication",
            cli.bind
        );
    }
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;
    // Printed on stdout (logs go to stderr) so scripts can find the real port.
    println!(
        "subspace-server listening on http://{}",
        listener.local_addr()?
    );

    axum::serve(listener, api::router(manager))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
