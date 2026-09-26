//! Keeps a Clevis SSS blob bound to the reachable Tang servers in its spec.

mod blob;
mod clevis;
mod config;
mod decision;
mod probe;
mod swap;

use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use clap::Parser;
use serde_json::json;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::decision::Decision;

#[derive(Parser)]
#[command(
    name = "ogygia-clevis",
    version,
    about = "Keeps a Clevis SSS blob bound to the reachable Tang servers in its spec"
)]
struct Args {
    /// Path to the JSON configuration file
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("ogygia_clevis=info,warn")),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;
    run(&config).await
}

async fn run(config: &Config) -> Result<()> {
    let path = &config.secret_file;
    let current_jwe = fs::read(path).with_context(|| format!("reading blob {}", path.display()))?;
    let current = str::from_utf8(&current_jwe)
        .map_err(anyhow::Error::from)
        .and_then(blob::bound_pins)
        .with_context(|| format!("reading pins bound in {}", path.display()))?;

    let reachable = probe::probe_all(&probe::client()?, &config.spec.pins.tang).await;
    let reachable_pins: Vec<_> = reachable.iter().map(|r| r.pin.clone()).collect();

    let (pins, previously) = match decision::decide(&config.spec, &current, &reachable_pins) {
        Decision::Keep(reason) => {
            info!("keeping current blob: {reason}");
            return Ok(());
        }
        Decision::Replace { pins, previously } => (pins, previously),
    };

    let secret = clevis::decrypt(&current_jwe)
        .await
        .context("decrypting current blob")?;
    let tang: Vec<_> = reachable
        .iter()
        .filter(|r| pins.contains(&r.pin))
        .map(|r| json!({ "url": r.pin.url, "thp": r.pin.thp, "adv": r.adv }))
        .collect();
    let new_jwe = clevis::encrypt_sss(
        &json!({ "t": config.spec.t, "pins": { "tang": tang } }),
        &secret,
    )
    .await
    .context("encrypting new blob")?;

    // Never install a blob this host could not decrypt or that binds
    // anything other than intended.
    let roundtrip = clevis::decrypt(&new_jwe)
        .await
        .context("verifying new blob")?;
    ensure!(
        roundtrip == secret,
        "new blob does not decrypt to the current secret"
    );
    let bound = blob::bound_pins(str::from_utf8(&new_jwe).context("new blob is not UTF-8")?)?;
    ensure!(
        pins.iter().all(|pin| bound.contains(pin)),
        "new blob is bound to {bound:?}, expected {pins:?}"
    );

    swap::replace(path, &new_jwe)?;
    info!(
        "replaced blob: bound to {} reachable specced pins (was {previously})",
        pins.len()
    );
    Ok(())
}
