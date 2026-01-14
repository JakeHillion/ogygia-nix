//! ogygia-irisd: Peer-to-peer Nix binary cache with bloom filter peer lookup

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use tokio::signal;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod bloom;
mod config;
mod nix;
mod server;
mod store;

#[derive(Parser)]
#[command(
    name = "ogygia-irisd",
    version,
    about = "Peer-to-peer Nix binary cache"
)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Disable store watching for new paths
    #[arg(long)]
    no_watch: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let config = Arc::new(config::load_config(cli.config.as_deref())?);
    tracing::info!("Loaded configuration");

    tracing::info!("ogygia-irisd starting");
    tracing::info!("HTTP listen: {:?}", config.server.listen);
    tracing::info!("Peers: {:?}", config.peers.urls);
    // Initialize bloom filters
    let local_bloom = Arc::new(bloom::local::LocalBloom::new(
        config.bloom.false_positive_rate,
        config.bloom.rebuild_threshold,
    ));
    let peer_blooms = Arc::new(bloom::peers::PeerBlooms::new(
        Duration::from_secs(config.bloom.peer_bloom_ttl_secs),
        local_bloom.num_bits(),
        local_bloom.num_hashes(),
    ));

    // HTTP client for peer requests
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");

    // Cancellation token for coordinated shutdown
    let token = CancellationToken::new();

    // Signal handler: cancel the token on SIGTERM/SIGINT
    {
        let token = token.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            token.cancel();
        });
    }

    // Rebuild channel: watcher sends rebuild requests, scanner receives
    let (rebuild_tx, rebuild_rx) = tokio::sync::mpsc::channel::<()>(1);

    // Initial store scan (must complete before serving)
    let scanner = store::scanner::StoreScanner::new(Arc::clone(&local_bloom), rebuild_rx);
    let stats = scanner.scan("Store scan").await?;
    tracing::info!(
        "Store scan: {} paths, {} indexed",
        stats.total_paths,
        stats.indexed,
    );

    // Spawn background tasks
    let mut tasks: Vec<JoinHandle<Result<()>>> = Vec::new();

    // Rebuild loop (services rebuild requests from the watcher)
    tasks.push(tokio::spawn(
        async move { scanner.run_rebuild_loop().await },
    ));

    // Store watcher (optional, runs until cancelled)
    if !cli.no_watch {
        let bloom = Arc::clone(&local_bloom);
        let token = token.clone();
        tasks.push(tokio::spawn(async move {
            let watcher = Arc::new(store::watcher::StoreWatcher::new(bloom, rebuild_tx));
            watcher.start(token).await
        }));
    }

    // Peer bloom eviction (runs until cancelled)
    {
        let peer_blooms = Arc::clone(&peer_blooms);
        let token = token.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let sleep_dur = peer_blooms
                    .next_eviction_time()
                    .await
                    .map(|t| t.saturating_duration_since(Instant::now()))
                    .unwrap_or(peer_blooms.ttl());
                tokio::select! {
                    _ = token.cancelled() => {
                        tracing::info!("Peer bloom eviction shutting down");
                        return Ok(());
                    }
                    _ = tokio::time::sleep(sleep_dur) => {
                        peer_blooms.evict_expired().await;
                    }
                }
            }
        }));
    }

    // HTTP server (runs until token is cancelled)
    tasks.push(tokio::spawn(server::start(
        config,
        local_bloom,
        peer_blooms,
        http_client,
        token.clone(),
    )));

    // Wait for all tasks — if any returns an error, propagate it.
    let futs = tasks.into_iter().map(|h| async move {
        h.await
            .map_err(|e| anyhow::anyhow!("task panicked: {}", e))?
    });
    futures::future::try_join_all(futs).await?;

    tracing::info!("ogygia-irisd stopped");
    Ok(())
}

async fn shutdown_signal() {
    let mut sigterm =
        signal::unix::signal(signal::unix::SignalKind::terminate()).expect("install SIGTERM");
    tokio::select! {
        _ = signal::ctrl_c() => tracing::info!("Received SIGINT, shutting down"),
        _ = sigterm.recv() => tracing::info!("Received SIGTERM, shutting down"),
    }
}
