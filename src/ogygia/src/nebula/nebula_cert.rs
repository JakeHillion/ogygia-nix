//! Wrapper around the `nebula-cert` binary.
//!
//! Lazily discovers `nebula-cert` on PATH or in known NixOS fallback
//! locations and exposes async helpers for the subcommands ogygia needs:
//! `sign`, `keygen`, `ca`.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use tokio::process::Command;

const FALLBACKS: &[&str] = &[
    "/run/current-system/sw/bin/nebula-cert",
    "/nix/var/nix/profiles/default/bin/nebula-cert",
];

static BIN: OnceLock<&'static str> = OnceLock::new();

pub fn bin() -> &'static str {
    BIN.get_or_init(|| {
        // A Nix build embeds the store path of nebula-cert here so the
        // derivation carries a runtime dependency on nebula. Unset for plain
        // `cargo build` (e.g. the dev shell), which falls back to discovery.
        if let Some(path) = option_env!("OGYGIA_NEBULA_CERT_BIN") {
            tracing::debug!("using embedded nebula-cert path: {}", path);
            return path;
        }
        if which::which("nebula-cert").is_ok() {
            tracing::debug!("using nebula-cert from PATH");
            return "nebula-cert";
        }
        for path in FALLBACKS {
            if Path::new(path).exists() {
                tracing::debug!("using nebula-cert fallback: {}", path);
                return path;
            }
        }
        tracing::warn!("nebula-cert not found on PATH or in fallback locations");
        "nebula-cert"
    })
}

/// Inputs to `nebula-cert sign`.
pub struct SignArgs<'a> {
    pub ca_cert: &'a Path,
    pub ca_key: &'a Path,
    pub in_pub: &'a Path,
    pub name: &'a str,
    pub networks: &'a str,
    pub groups: &'a [String],
    pub duration_seconds: u64,
    pub out_cert: &'a Path,
}

/// Run `nebula-cert sign` and write the result to `out_cert`.
///
/// When the CA key is encrypted, `nebula-cert` prompts for the passphrase on
/// the controlling terminal, so stdin/stdout/stderr are inherited rather than
/// captured — otherwise the prompt would be swallowed and the read would hit a
/// closed stdin.
pub async fn sign(args: SignArgs<'_>) -> Result<()> {
    let duration = format!("{}s", args.duration_seconds);
    let groups = args.groups.join(",");

    let mut cmd = Command::new(bin());
    cmd.arg("sign");
    cmd.arg("-ca-key").arg(args.ca_key);
    cmd.arg("-ca-crt").arg(args.ca_cert);
    cmd.arg("-in-pub").arg(args.in_pub);
    cmd.arg("-name").arg(args.name);
    cmd.arg("-networks").arg(args.networks);
    if !groups.is_empty() {
        cmd.arg("-groups").arg(&groups);
    }
    cmd.arg("-duration").arg(&duration);
    cmd.arg("-out-crt").arg(args.out_cert);

    let status = cmd
        .status()
        .await
        .context("failed to spawn nebula-cert sign")?;

    if !status.success() {
        return Err(anyhow!("nebula-cert sign failed (see output above)"));
    }
    Ok(())
}

/// Compute the host-IP-plus-mask string passed to `nebula-cert sign -networks`,
/// e.g. ipv4 "10.42.0.1" + subnet "10.42.0.0/16" → "10.42.0.1/16".
pub fn network_spec(ipv4: &str, subnet: &str) -> Result<String> {
    let mask = subnet
        .split_once('/')
        .map(|(_, m)| m)
        .ok_or_else(|| anyhow!("subnet must be CIDR (got {subnet})"))?;
    Ok(format!("{ipv4}/{mask}"))
}

/// Materialise a PEM string into a tempfile, returning a handle that deletes
/// itself on drop. Used to pass `pubKey` content to `nebula-cert sign -in-pub`.
pub fn pem_to_tempfile(pem: &str) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut tf = tempfile::Builder::new()
        .prefix("ogygia-nebula-pub-")
        .suffix(".pem")
        .tempfile()
        .context("failed to create tempfile for pubkey")?;
    tf.as_file_mut()
        .write_all(pem.as_bytes())
        .context("failed to write pubkey tempfile")?;
    if !pem.ends_with('\n') {
        tf.as_file_mut()
            .write_all(b"\n")
            .context("failed to write pubkey tempfile newline")?;
    }
    Ok(tf)
}

/// Resolve `nebula-cert ca` invocation: generate a new CA cert+key pair.
pub async fn create_ca(
    name: &str,
    out_cert: &Path,
    out_key: &Path,
    duration_seconds: u64,
) -> Result<()> {
    let duration = format!("{}s", duration_seconds);
    let mut cmd = Command::new(bin());
    cmd.arg("ca");
    cmd.arg("-name").arg(name);
    cmd.arg("-out-crt").arg(out_cert);
    cmd.arg("-out-key").arg(out_key);
    cmd.arg("-duration").arg(&duration);
    let output = cmd
        .output()
        .await
        .context("failed to spawn nebula-cert ca")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nebula-cert ca failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Default CA key path under the user's config home.
pub fn default_ca_key_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("ogygia").join("nebula").join("ca.key")
}
