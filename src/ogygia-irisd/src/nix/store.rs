//! Nix store path utilities
//!
//! This module provides utilities for querying Nix store paths using `nix path-info`.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use tokio::process::Command;

use crate::nix::narinfo::Compression;
use crate::nix::narinfo::NarInfo;

/// Information about a store path from `nix path-info --json`
#[derive(Debug, Clone)]
pub struct PathInfo {
    /// Full store path
    pub path: String,
    /// NAR hash (sha256)
    pub nar_hash: String,
    /// NAR size in bytes
    pub nar_size: u64,
    /// References (other store paths this depends on)
    pub references: Vec<String>,
    /// Deriver path (optional)
    pub deriver: Option<String>,
    /// Signatures
    pub signatures: Vec<String>,
    /// Content-addressed info (optional)
    pub ca: Option<String>,
}

/// Raw JSON structure from nix path-info
/// Note: The path itself is the map key in the JSON output
#[derive(Debug, Deserialize)]
struct PathInfoJson {
    #[serde(rename = "narHash")]
    nar_hash: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    #[serde(default)]
    references: Vec<String>,
    deriver: Option<String>,
    #[serde(default)]
    signatures: Vec<String>,
    ca: Option<String>,
}

impl PathInfo {
    /// Query path info for a store path using `nix path-info --json`.
    pub async fn from_store_path(store_path: &Path) -> Result<Self> {
        let mut infos = Self::from_store_paths([store_path]).await?;
        infos
            .pop()
            .ok_or_else(|| anyhow!("No path info returned for {}", store_path.display()))
    }

    /// Query path info for multiple store paths in a single `nix path-info --json` invocation.
    pub async fn from_store_paths(
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<Vec<Self>> {
        let mut cmd = Command::new("nix");
        cmd.arg("path-info").arg("--json");
        let mut count = 0;
        for path in paths {
            cmd.arg(path.as_ref());
            count += 1;
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd
            .output()
            .await
            .context("Failed to execute nix path-info")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("nix path-info failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // nix path-info --json returns a map: {"/nix/store/...": {...}}
        // Some paths may have null values instead of objects.
        let infos: HashMap<String, Option<PathInfoJson>> = serde_json::from_str(&stdout)
            .with_context(|| format!("Failed to parse path-info JSON: {}", stdout))?;

        Ok(infos
            .into_iter()
            .filter_map(|(path, info)| {
                info.map(|info| PathInfo {
                    path,
                    nar_hash: info.nar_hash,
                    nar_size: info.nar_size,
                    references: info.references,
                    deriver: info.deriver,
                    signatures: info.signatures,
                    ca: info.ca,
                })
            })
            .collect())
    }

    /// Whether this path is suitable for serving to peers.
    ///
    /// A path is serveable if it is content-addressed (self-verifying) or
    /// has at least one signature.
    pub fn is_serveable(&self) -> bool {
        self.ca.is_some() || !self.signatures.is_empty()
    }

    /// Convert to NarInfo format
    ///
    /// Note: FileHash and FileSize are set to placeholders since we don't know
    /// the compressed size until we actually generate and compress the NAR.
    /// These should be updated after the first NAR generation.
    pub fn to_narinfo(&self, _hash: &str) -> NarInfo {
        // Extract just the store path name (without /nix/store/ prefix)
        let store_name = self.path.strip_prefix("/nix/store/").unwrap_or(&self.path);

        // Generate URL for NAR file (use zstd compression)
        let url = format!("nar/{}.nar.zst", store_name);

        // For now, use placeholder values for FileHash and FileSize
        // since we compress on-the-fly and don't know these until first generation
        // The hash is the nar_hash (uncompressed) as a placeholder
        let file_hash = self.nar_hash.clone();
        let file_size = self.nar_size; // Placeholder - actual compressed size unknown

        // Extract reference names (just the name part, not full paths)
        let references: Vec<String> = self
            .references
            .iter()
            .filter_map(|r| r.strip_prefix("/nix/store/"))
            .map(String::from)
            .collect();

        // Extract deriver name
        let deriver = self
            .deriver
            .as_ref()
            .and_then(|d| d.strip_prefix("/nix/store/").map(String::from));

        NarInfo {
            store_path: self.path.clone(),
            url,
            compression: Compression::Zstd,
            file_hash,
            file_size,
            nar_hash: self.nar_hash.clone(),
            nar_size: self.nar_size,
            references,
            deriver,
            signatures: self.signatures.clone(),
            ca: self.ca.clone(),
        }
    }
}

/// Find a store path by its hash prefix.
///
/// Scans `/nix/store` for an entry matching `{hash}-*`, ignoring lock files.
pub async fn find_store_path(hash: &str) -> Option<PathBuf> {
    let store_dir = Path::new("/nix/store");
    let pattern = format!("{}-", hash);

    let mut read_dir = tokio::fs::read_dir(store_dir).await.ok()?;

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(&pattern) && !name.ends_with(".lock") {
                return Some(store_dir.join(name));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_info_to_narinfo() {
        let path_info = PathInfo {
            path: "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10".to_string(),
            nar_hash: "sha256:0123456789abcdef".to_string(),
            nar_size: 12345,
            references: vec!["/nix/store/xyz789abc123def456ghi012jkl345mno-glibc-2.35".to_string()],
            deriver: Some(
                "/nix/store/drv123abc456def789ghi012jkl345mno-hello-2.10.drv".to_string(),
            ),
            signatures: vec!["cache.nixos.org-1:signature123".to_string()],
            ca: None,
        };

        let narinfo = path_info.to_narinfo("abc123def456ghi789jkl012mno345pq");

        assert_eq!(
            narinfo.store_path,
            "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10"
        );
        assert_eq!(
            narinfo.url,
            "nar/abc123def456ghi789jkl012mno345pq-hello-2.10.nar.zst"
        );
        assert_eq!(narinfo.compression, Compression::Zstd);
        assert_eq!(narinfo.nar_hash, "sha256:0123456789abcdef");
        assert_eq!(narinfo.nar_size, 12345);
        assert_eq!(narinfo.references.len(), 1);
        assert_eq!(narinfo.signatures.len(), 1);
    }

    #[test]
    fn test_is_serveable() {
        let base = PathInfo {
            path: "/nix/store/abc-test".to_string(),
            nar_hash: "sha256:000".to_string(),
            nar_size: 1,
            references: vec![],
            deriver: None,
            signatures: vec![],
            ca: None,
        };

        // No signatures, no CA → not serveable
        assert!(!base.is_serveable());

        // Has signature → serveable
        let signed = PathInfo {
            signatures: vec!["key:sig".to_string()],
            ..base.clone()
        };
        assert!(signed.is_serveable());

        // Content-addressed, no signatures → serveable
        let ca = PathInfo {
            ca: Some("fixed:sha256:abc".to_string()),
            ..base.clone()
        };
        assert!(ca.is_serveable());
    }
}
