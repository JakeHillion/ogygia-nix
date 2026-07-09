//! Push command for sending locally-built store paths to irisd.
//!
//! This command reads store paths from stdin, computes their closure,
//! signs ultimate (locally-built) paths using `nix store sign`, and
//! notifies irisd to rescan them for DHT advertisement.

use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::ArgAction;
use clap::Args;
use futures::future::join_all;

use super::client::IrisdClient;
use super::client::PullResponse;
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

    /// Remote irisd URLs to pull from after local push (repeatable)
    #[arg(
        long,
        env = "OGYGIA_IRISD_REMOTE_URL",
        action = ArgAction::Append
    )]
    pub remote: Vec<String>,
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
    let key_prefix = format!("{}:", key_name);

    // Filter out paths that already have our signature
    let unsigned: Vec<_> = ultimate_paths
        .iter()
        .filter(|p| !p.signatures.iter().any(|s| s.starts_with(&key_prefix)))
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

    let paths_to_rescan: Vec<PathBuf> = unsigned
        .into_iter()
        .map(|p| p.path.clone().into())
        .collect();

    // Notify irisd to rescan the newly signed paths
    tracing::info!(
        "Notifying irisd to rescan {} paths...",
        paths_to_rescan.len()
    );
    let rescan_result = client
        .rescan(&paths_to_rescan)
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

    // Handle remote pulls if --remote is specified
    if !args.remote.is_empty() {
        // Validate that remote URLs don't match local irisd_url
        for remote_url in &args.remote {
            if remote_url.trim_end_matches('/') == args.irisd_url.trim_end_matches('/') {
                return Err(anyhow!(
                    "remote URL {} must differ from local irisd URL",
                    remote_url
                ));
            }
        }

        // Use the same paths that were rescanned locally for remote pulls
        let paths_to_advertise = paths_to_rescan.clone();

        if paths_to_advertise.is_empty() {
            tracing::info!("No paths to advertise to remotes");
            return Ok(());
        }

        tracing::info!(
            "Initiating remote pulls to {} remotes...",
            args.remote.len()
        );

        // Spawn concurrent tasks for each remote
        let remote_futures: Vec<_> = args
            .remote
            .iter()
            .map(|remote_url| {
                let remote_url = remote_url.clone();
                let paths = paths_to_advertise.clone();

                async move {
                    tracing::info!("Initiating remote pull to {}", remote_url);

                    let remote_client = match IrisdClient::new(&remote_url) {
                        Ok(client) => client,
                        Err(e) => {
                            tracing::error!("Failed to create client for {}: {}", remote_url, e);
                            return (remote_url, Err(e));
                        }
                    };

                    let result = remote_client.pull(&paths).await;
                    (remote_url, result)
                }
            })
            .collect();

        // Wait for all remote pulls to complete
        let remote_results: Vec<(String, Result<PullResponse>)> = join_all(remote_futures).await;

        // Log results and aggregate errors
        let mut total_pulled = 0usize;
        let mut total_skipped = 0usize;
        let mut total_failed = 0usize;
        let mut remote_errors = Vec::new();

        for (remote_url, result) in remote_results {
            match result {
                Ok(response) => {
                    tracing::info!(
                        "Remote pull to {}: {} pulled, {} skipped, {} failed",
                        remote_url,
                        response.pulled,
                        response.skipped,
                        response.failed
                    );
                    total_pulled += response.pulled;
                    total_skipped += response.skipped;
                    total_failed += response.failed;
                    if !response.errors.is_empty() {
                        remote_errors.push(format!(
                            "{}: {}",
                            remote_url,
                            response.errors.join(", ")
                        ));
                    }
                }
                Err(e) => {
                    tracing::error!("Remote pull to {} failed: {}", remote_url, e);
                    remote_errors.push(format!("{}: {}", remote_url, e));
                }
            }
        }

        tracing::info!(
            "Remote pulls complete: {} pulled, {} skipped, {} failed across all remotes",
            total_pulled,
            total_skipped,
            total_failed
        );

        if !remote_errors.is_empty() {
            anyhow::bail!("Remote pull errors:\n  {}", remote_errors.join("\n  "));
        }
    }

    Ok(())
}
