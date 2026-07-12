//! Push command for sending locally-built store paths to irisd.
//!
//! This command reads store paths from stdin, computes their closure,
//! signs ultimate (locally-built) paths using `nix store sign`, and
//! notifies irisd to rescan them for DHT advertisement.

use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use clap::Args;

use super::client::IrisdClient;
use super::nix::compute_closure;
use super::nix::parse_key_name;
use super::nix::query_ultimate_paths;
use super::nix::read_store_paths_from_stdin;
use super::nix::sign_store_paths;

/// Arguments for the push command
#[derive(Args)]
pub struct PushArgs {
    /// URL of irisd server
    #[arg(
        long,
        env = "OGYGIA_IRISD_URL",
        default_value = "http://127.0.0.1:35742"
    )]
    pub irisd_url: String,

    /// Path to signing key file (nix key generate-secret format)
    #[arg(long, env = "OGYGIA_SIGNING_KEY")]
    pub signing_key: PathBuf,

    /// Push only the specified paths, not their closures
    #[arg(long)]
    pub no_closure: bool,
}

/// Execute the push command
pub fn run(args: &PushArgs) -> Result<()> {
    // Build async runtime
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build tokio runtime")?;

    runtime.block_on(async_run(args))
}

async fn async_run(args: &PushArgs) -> Result<()> {
    // Create irisd client
    let client = IrisdClient::new(&args.irisd_url)
        .with_context(|| format!("Failed to create irisd client for {}", args.irisd_url))?;

    tracing::info!("Connecting to irisd at {}", args.irisd_url);

    // Read store paths from stdin
    let input_paths = read_store_paths_from_stdin().await?;

    if input_paths.is_empty() {
        tracing::warn!("No store paths provided on stdin");
        return Ok(());
    }

    // Compute closure unless --no-closure is specified
    let store_paths = if args.no_closure {
        tracing::info!("Processing {} store paths...", input_paths.len());
        input_paths
    } else {
        tracing::info!("Computing closure for {} paths...", input_paths.len());
        let closure = compute_closure(&input_paths).await?;
        tracing::info!("Closure contains {} store paths", closure.len());
        closure
    };

    // Query path info and filter to ultimate (locally-built) paths
    tracing::info!("Querying path info for {} paths...", store_paths.len());
    let ultimate_paths = query_ultimate_paths(&store_paths).await?;

    if ultimate_paths.is_empty() {
        tracing::warn!("No ultimate (locally-built) paths found to push");
        return Ok(());
    }

    tracing::info!(
        "Found {} ultimate paths out of {} total",
        ultimate_paths.len(),
        store_paths.len()
    );

    // Sign paths using nix store sign
    let key_name = parse_key_name(&args.signing_key)?;

    // Filter out paths that already have our signature
    let unsigned: Vec<_> = ultimate_paths
        .iter()
        .filter(|p| !p.signatures.iter().any(|s| s.name() == key_name))
        .collect();

    if unsigned.is_empty() {
        tracing::info!(
            "All {} paths already signed with {}",
            ultimate_paths.len(),
            key_name
        );
        return Ok(());
    }

    tracing::info!(
        "Signing {} paths ({} already signed with {})...",
        unsigned.len(),
        ultimate_paths.len() - unsigned.len(),
        key_name
    );
    let sign_result = sign_store_paths(&args.signing_key, unsigned.iter().map(|p| &p.path)).await?;

    if sign_result.failed > 0 {
        tracing::warn!(
            "Signing: {} succeeded, {} failed",
            sign_result.signed,
            sign_result.failed
        );
    } else {
        tracing::info!("Signed {} paths", sign_result.signed);
    }

    let paths_to_rescan: Vec<_> = unsigned.into_iter().map(|p| &p.path).collect();

    // Notify irisd to rescan the newly signed paths
    tracing::info!(
        "Notifying irisd to rescan {} paths...",
        paths_to_rescan.len()
    );
    let rescan_result = client
        .rescan(paths_to_rescan)
        .await
        .context("Failed to rescan paths")?;

    tracing::info!(
        "Done: {} rescanned, {} errors",
        rescan_result.rescanned,
        rescan_result.errors
    );

    if rescan_result.errors > 0 {
        anyhow::bail!("{} paths failed during rescan", rescan_result.errors);
    }

    Ok(())
}
