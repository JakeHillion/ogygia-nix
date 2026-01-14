//! Nix store queries.
//!
//! Provides functions to query information about Nix store paths
//! using the `nix path-info` command.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

/// Fallback paths for nix binary when not in PATH.
const NIX_FALLBACKS: &[&str] = &[
    "/run/current-system/sw/bin/nix",
    "/nix/var/nix/profiles/default/bin/nix",
];

static NIX_BIN: OnceLock<&'static str> = OnceLock::new();

/// Find the nix binary, checking PATH first then falling back to known locations.
fn nix_bin() -> &'static str {
    NIX_BIN.get_or_init(|| {
        // Check PATH first
        if which::which("nix").is_ok() {
            tracing::info!("using nix from PATH");
            return "nix";
        }
        // Try fallback paths
        for path in NIX_FALLBACKS {
            if std::path::Path::new(path).exists() {
                tracing::info!("using nix fallback: {}", path);
                return path;
            }
        }
        // Last resort, hope it's in PATH
        tracing::warn!("nix not found in PATH or fallback locations, hoping for the best");
        "nix"
    })
}

/// Information about a Nix store path from `nix path-info --json`
#[derive(Debug)]
pub struct PathInfo {
    /// Full store path
    pub path: String,
    /// Signatures on this path (format: "key-name:base64-signature")
    pub signatures: Vec<String>,
}

/// Raw JSON structure from `nix path-info --json` (path is the map key)
#[derive(Deserialize)]
struct RawPathInfo {
    #[serde(default)]
    ultimate: bool,
    #[serde(default)]
    signatures: Vec<String>,
}

/// Compute the closure of store paths using `nix path-info -r`.
///
/// Returns all paths in the closure, including the input paths.
pub async fn compute_closure(paths: &[String]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let output = Command::new(nix_bin())
        .args(["path-info", "-r"])
        .args(paths)
        .output()
        .await
        .context("Failed to run nix path-info -r")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix path-info -r failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8(output.stdout).context("Invalid UTF-8 in nix output")?;

    let closure: Vec<String> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|s| s.to_string())
        .collect();

    Ok(closure)
}

/// Read store paths from stdin, one per line.
///
/// Skips empty lines and lines that don't start with /nix/store/.
pub async fn read_store_paths_from_stdin() -> Result<Vec<String>> {
    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    let mut paths = Vec::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("/nix/store/") {
            tracing::warn!("skipping non-store path: {}", line);
            continue;
        }
        paths.push(line.to_string());
    }

    Ok(paths)
}

/// Result of signing store paths.
#[derive(Debug)]
pub struct SignResult {
    /// Number of paths successfully signed
    pub signed: usize,
    /// Number of paths that failed to sign
    pub failed: usize,
}

/// Sign store paths using `nix store sign --key-file`.
///
/// Accepts any iterator of path-like items and signs them in batches
/// to avoid command line length limits.
pub async fn sign_store_paths<I, P>(key_file: &Path, paths: I) -> Result<SignResult>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    const BATCH_SIZE: usize = 100;

    let mut signed = 0;
    let mut failed = 0;

    // Process paths in batches to avoid command line limits
    let mut iter = paths.into_iter().peekable();
    while iter.peek().is_some() {
        let batch: Vec<_> = iter.by_ref().take(BATCH_SIZE).collect();

        let mut cmd = Command::new(nix_bin());
        cmd.args(["store", "sign", "--key-file"]);
        cmd.arg(key_file);
        for p in &batch {
            cmd.arg(p.as_ref());
        }

        let output = cmd.output().await.context("Failed to run nix store sign")?;

        if output.status.success() {
            signed += batch.len();
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("nix store sign failed for batch: {}", stderr.trim());
            failed += batch.len();
        }
    }

    Ok(SignResult { signed, failed })
}

/// Query path info for multiple paths and filter to only ultimate (locally-built) paths.
///
/// Returns PathInfo for paths where `ultimate: true`, meaning they were built
/// locally rather than substituted from a cache.
pub async fn query_ultimate_paths(paths: &[String]) -> Result<Vec<PathInfo>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Query all paths in batch
    let mut cmd = Command::new(nix_bin());
    cmd.args(["path-info", "--json"]);
    cmd.args(paths);

    let output = cmd.output().await.context("Failed to run nix path-info")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix path-info failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8(output.stdout).context("Invalid UTF-8 in nix output")?;

    // Parse the JSON map
    let infos: HashMap<String, RawPathInfo> =
        serde_json::from_str(&stdout).context("Failed to parse nix path-info JSON")?;

    // Filter to only ultimate paths
    let ultimate_paths: Vec<PathInfo> = infos
        .into_iter()
        .filter(|(_, raw)| raw.ultimate)
        .map(|(path, raw)| PathInfo {
            path,
            signatures: raw.signatures,
        })
        .collect();

    Ok(ultimate_paths)
}

/// Parse key name from a Nix signing key file.
///
/// The key file format is "key-name:base64-secret-key".
/// Returns the key name portion.
pub fn parse_key_name(key_file: &Path) -> Result<String> {
    let content = std::fs::read_to_string(key_file).context("Failed to read signing key file")?;
    let name = content
        .trim()
        .split(':')
        .next()
        .ok_or_else(|| anyhow!("Invalid key format: missing colon separator"))?;
    Ok(name.to_string())
}
