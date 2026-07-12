//! Nix helpers for the iris subcommand.
//!
//! Reading store paths from stdin and parsing the signing key file; store-path
//! metadata queries live in [`ogygia_nixutils`].

use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

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
