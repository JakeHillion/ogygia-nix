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
use ogygia_nixutils::Signature;
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
    /// Signatures on this path.
    pub signatures: Vec<Signature>,
}

/// Raw JSON structure from `nix path-info --json` (path is the map key)
#[derive(Deserialize)]
struct RawPathInfo {
    #[serde(default)]
    ultimate: bool,
    #[serde(default)]
    signatures: Vec<String>,
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
        .map(|(path, raw)| {
            let signatures = raw
                .signatures
                .iter()
                .map(|s| s.parse())
                .collect::<Result<Vec<Signature>>>()
                .with_context(|| format!("invalid signature for {path}"))?;
            Ok(PathInfo { path, signatures })
        })
        .collect::<Result<Vec<PathInfo>>>()?;

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
