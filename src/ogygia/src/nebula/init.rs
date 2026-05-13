//! `ogygia nebula init` — bootstrap a brand-new CA.

use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;

use super::config::Config;
use super::config::DEFAULT_CA_DURATION_SECS;
use super::config::DEFAULT_NETWORK_NAME;
use super::nebula_cert;

#[derive(Args)]
pub struct InitArgs {
    /// Network name embedded in the CA certificate (also the nebula network name).
    #[arg(long)]
    pub name: Option<String>,
    /// CA validity in seconds. Defaults to ten years.
    #[arg(long, default_value_t = DEFAULT_CA_DURATION_SECS)]
    pub duration: u64,
    /// Overwrite an existing CA cert/key if present.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: &InitArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(async_run(args))
}

async fn async_run(args: &InitArgs) -> Result<()> {
    let config = Config::load()?;
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| DEFAULT_NETWORK_NAME.to_string());

    let ca_key_path = match std::env::var(super::config::ENV_CA_KEY) {
        Ok(p) => PathBuf::from(p),
        Err(_) => nebula_cert::default_ca_key_path(),
    };

    if !args.force {
        if config.ca_cert_path.exists() {
            return Err(anyhow!(
                "CA cert already exists at {}; pass --force to overwrite",
                config.ca_cert_path.display()
            ));
        }
        if ca_key_path.exists() {
            return Err(anyhow!(
                "CA key already exists at {}; pass --force to overwrite",
                ca_key_path.display()
            ));
        }
    }

    if let Some(parent) = config.ca_cert_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Some(parent) = ca_key_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        // Ensure the parent directory of the CA key is 0700.
        let mut perms = std::fs::metadata(parent)?.permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o700);
        std::fs::set_permissions(parent, perms)?;
    }

    tracing::info!(
        ca_cert = %config.ca_cert_path.display(),
        ca_key = %ca_key_path.display(),
        "creating Nebula CA"
    );

    nebula_cert::create_ca(&name, &config.ca_cert_path, &ca_key_path, args.duration).await?;

    // Tighten CA key permissions to 0600 even if nebula-cert already did so.
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).mode(0o600);
    if let Ok(_f) = opts.open(&ca_key_path) {
        // touching with mode 0o600 has no portable way to chmod a closed file —
        // fall back to direct permission set:
    }
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&ca_key_path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&ca_key_path, perms)?;

    println!("Nebula CA created:");
    println!("  cert: {}", config.ca_cert_path.display());
    println!("  key:  {}", ca_key_path.display());
    println!();
    println!(
        "Set OGYGIA_NEBULA_CA_KEY={} in your shell to sign host certs.",
        ca_key_path.display()
    );

    Ok(())
}
