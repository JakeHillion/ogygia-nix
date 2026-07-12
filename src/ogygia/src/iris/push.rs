//! Push command for sending locally-built store paths to irisd.
//!
//! This command reads store paths from stdin, computes their closure,
//! signs ultimate (locally-built) paths using `nix store sign`, and
//! notifies irisd to rescan them for DHT advertisement.

use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::stream;
use ogygia_nixutils::Nix;
use ogygia_nixutils::StoreHash;

use super::client::IrisdClient;
use super::nix::parse_key_name;
use super::nix::read_store_paths_from_stdin;

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

    let nix = Nix::default();

    // Read store paths from stdin
    let input_paths = read_store_paths_from_stdin().await?;

    if input_paths.is_empty() {
        tracing::warn!("No store paths provided on stdin");
        return Ok(());
    }

    let key_name = parse_key_name(&args.signing_key)?;

    // The store paths to consider, as a stream: the closure of the inputs, or
    // the inputs themselves with --no-closure.
    let store_paths = if args.no_closure {
        tracing::info!("Processing {} store paths...", input_paths.len());
        stream::iter(input_paths)
            .map(Ok::<_, anyhow::Error>)
            .boxed()
    } else {
        tracing::info!("Computing closure of {} paths...", input_paths.len());
        nix.compute_closure(input_paths).boxed()
    };

    // Keep only ultimate (locally-built) paths that don't already carry our
    // signature; both checks query the local Nix installation per path.
    let to_sign: Vec<String> = {
        let nix = &nix;
        let key = key_name.as_str();
        store_paths
            .try_filter_map(move |path| async move {
                if !nix.is_ultimate(&path).await? {
                    return Ok(None);
                }
                let hash = StoreHash::from_store_path(&path)?;
                let already_signed = nix
                    .find_path_info(&hash)
                    .await?
                    .is_some_and(|info| info.signatures.iter().any(|s| s.name() == key));
                Ok((!already_signed).then_some(path))
            })
            .try_collect()
            .await?
    };

    if to_sign.is_empty() {
        tracing::info!("No unsigned ultimate paths to push");
        return Ok(());
    }

    tracing::info!("Signing {} paths with {}...", to_sign.len(), key_name);
    nix.sign_paths(&args.signing_key, stream::iter(&to_sign))
        .await?;

    // Notify irisd to rescan the newly signed paths
    tracing::info!("Notifying irisd to rescan {} paths...", to_sign.len());
    let rescan_result = client
        .rescan(&to_sign)
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
