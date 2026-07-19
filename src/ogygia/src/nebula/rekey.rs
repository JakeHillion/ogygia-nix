//! `ogygia nebula rekey` — sign missing host certificates by walking the flake.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use ogygia_nixutils::Nix;

use super::config::Config;
use super::flake_eval;
use super::nebula_cert;
use super::nebula_cert::SignArgs;

#[derive(Args)]
pub struct RekeyArgs {
    /// Process every host in the flake.
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Process a single host.
    #[arg(long)]
    pub host: Option<String>,
    /// Re-sign even if a matching cert already exists.
    #[arg(long)]
    pub force: bool,
    /// Show what would happen without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Flake reference; defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub flake: String,
}

pub fn run(args: &RekeyArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(async_run(args))
}

async fn async_run(args: &RekeyArgs) -> Result<()> {
    if !args.all && args.host.is_none() {
        return Err(anyhow!("specify -a/--all or --host <name>"));
    }
    if args.all && args.host.is_some() {
        return Err(anyhow!("-a and --host are mutually exclusive"));
    }

    let config = Config::load()?;
    let ca_key = if !args.dry_run {
        Some(config.ca_key_path()?)
    } else {
        None
    };

    let nix = Nix::default();

    let targets: Vec<String> = if let Some(host) = &args.host {
        vec![host.clone()]
    } else {
        flake_eval::list_hosts(&nix, &args.flake).await?
    };

    let mut current_cert_paths: HashSet<PathBuf> = HashSet::new();
    let mut signed = 0usize;
    let mut skipped = 0usize;

    for host in &targets {
        let info = flake_eval::host_info(&nix, &args.flake, host).await?;
        if !info.enabled {
            tracing::debug!(%host, "skipping (nebula disabled)");
            skipped += 1;
            continue;
        }
        let (spec, spec_hash) = match (info.spec.as_ref(), info.spec_hash.as_ref()) {
            (Some(s), Some(h)) => (s, h),
            _ => {
                tracing::warn!(%host, "nebula.spec is null (pubKey not set?); skipping");
                skipped += 1;
                continue;
            }
        };

        let cert_path = config.cert_path(spec_hash);
        current_cert_paths.insert(cert_path.clone());

        if cert_path.exists() && !args.force {
            tracing::info!(%host, hash = %spec_hash, "cert up to date");
            skipped += 1;
            continue;
        }

        if args.dry_run {
            println!("would sign {}", cert_path.display());
            continue;
        }

        let parent = cert_path
            .parent()
            .ok_or_else(|| anyhow!("invalid cert path"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let pub_tempfile = nebula_cert::pem_to_tempfile(&spec.pub_key)?;
        let networks = nebula_cert::network_spec(&spec.ipv4, &spec.subnet)?;

        sign_with_atomic_rename(
            &cert_path,
            SignArgs {
                ca_cert: &config.ca_cert_path,
                ca_key: ca_key.as_deref().expect("checked above"),
                in_pub: pub_tempfile.path(),
                name: &info.host,
                networks: &networks,
                groups: &spec.groups,
                duration_seconds: info.validity_secs,
                out_cert: &cert_path,
            },
        )
        .await?;

        println!("signed {}", cert_path.display());
        signed += 1;
    }

    let pruned = if args.all && !args.dry_run {
        prune_orphans(&config.cert_dir, &current_cert_paths)?
    } else {
        0
    };

    eprintln!("rekey: {signed} signed, {skipped} unchanged, {pruned} orphans pruned");
    Ok(())
}

async fn sign_with_atomic_rename(final_path: &Path, args: SignArgs<'_>) -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix(".ogygia-nebula-sign-")
        .suffix(".crt")
        .tempfile_in(final_path.parent().unwrap())
        .context("failed to create signing tempfile")?;
    let tmp_path = tmp.path().to_path_buf();
    drop(tmp); // close before nebula-cert writes

    let sign_args = SignArgs {
        out_cert: &tmp_path,
        ..args
    };
    nebula_cert::sign(sign_args).await?;
    std::fs::rename(&tmp_path, final_path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

fn prune_orphans(cert_dir: &Path, keep: &HashSet<PathBuf>) -> Result<usize> {
    let mut pruned = 0usize;
    if !cert_dir.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(cert_dir)
        .with_context(|| format!("failed to read {}", cert_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("crt") {
            continue;
        }
        // Never prune the CA cert.
        if path.file_name().and_then(|n| n.to_str()) == Some("ca.crt") {
            continue;
        }
        if keep.contains(&path) {
            continue;
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove orphan {}", path.display()))?;
        tracing::info!(path = %path.display(), "pruned orphan cert");
        pruned += 1;
    }
    Ok(pruned)
}
