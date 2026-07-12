//! Narinfo parsing and serialization

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use anyhow::Result;
use anyhow::anyhow;

use crate::signature::Signature;
use crate::types::NarHash;

/// Parsed narinfo file
#[derive(Debug, Clone)]
pub struct NarInfo {
    /// Store path (e.g., /nix/store/abc123-hello)
    pub store_path: String,
    /// URL relative to cache root (e.g., nar/xyz.nar.xz)
    pub url: String,
    /// Compression type
    pub compression: Compression,
    /// Hash of compressed file
    pub file_hash: NarHash,
    /// Size of compressed file
    pub file_size: u64,
    /// Hash of uncompressed NAR
    pub nar_hash: NarHash,
    /// Size of uncompressed NAR
    pub nar_size: u64,
    /// Store path references
    pub references: Vec<String>,
    /// Deriver store path (optional)
    pub deriver: Option<String>,
    /// Signatures
    pub signatures: Vec<Signature>,
    /// Content-addressed info (optional)
    pub ca: Option<String>,
}

/// Compression type
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum Compression {
    None,
    Xz,
    Bzip2,
    Gzip,
    Zstd,
}

impl NarInfo {
    /// Check if this narinfo has at least one signature from a trusted key.
    ///
    /// Trusted keys are in format "name:base64-public-key"; a signature is
    /// trusted if its key name matches that of any trusted key.
    pub fn has_trusted_signature(&self, trusted_keys: &[String]) -> bool {
        let trusted_names: Vec<&str> = trusted_keys
            .iter()
            .filter_map(|key| key.split_once(':').map(|(name, _)| name))
            .collect();

        self.signatures
            .iter()
            .any(|sig| trusted_names.contains(&sig.name()))
    }

    /// Check if this narinfo has a meaningful content-addressed (CA) field.
    ///
    /// Returns true if the CA field is present and contains non-whitespace content.
    /// Empty or whitespace-only CA fields are not considered valid CA paths.
    fn has_ca(&self) -> bool {
        self.ca.as_ref().is_some_and(|s| !s.trim().is_empty())
    }

    /// Check if this narinfo is trusted for fetching from peers.
    ///
    /// A narinfo is trusted if:
    /// - It is content-addressed (has a meaningful CA field), OR
    /// - It has at least one signature from a trusted key, OR
    /// - No trusted keys are configured (no trust restriction)
    ///
    /// This matches Nix's behavior where content-addressed paths are self-verifying
    /// and don't require signatures.
    pub fn is_trusted(&self, trusted_keys: &[String]) -> bool {
        // Content-addressed paths are inherently trusted (self-verifying)
        if self.has_ca() {
            return true;
        }

        // If no trusted keys are configured, no trust restriction applies
        if trusted_keys.is_empty() {
            return true;
        }

        // Otherwise, require a trusted signature
        self.has_trusted_signature(trusted_keys)
    }
}

impl FromStr for NarInfo {
    type Err = anyhow::Error;

    /// Parse a narinfo file.
    fn from_str(content: &str) -> Result<Self> {
        let mut fields: HashMap<&str, &str> = HashMap::new();
        let mut signatures = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();

                if key == "Sig" {
                    signatures.push(value.parse()?);
                } else {
                    fields.insert(key, value);
                }
            }
        }

        let store_path = fields
            .get("StorePath")
            .ok_or_else(|| anyhow!("Missing StorePath"))?
            .to_string();

        let url = fields
            .get("URL")
            .ok_or_else(|| anyhow!("Missing URL"))?
            .to_string();

        let compression = fields
            .get("Compression")
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<Compression>()
                    .map_err(|e| anyhow!("Unknown compression type '{}': {}", s, e))
            })
            .transpose()?
            .unwrap_or(Compression::None);

        let file_hash = NarHash::from_sri(
            fields
                .get("FileHash")
                .ok_or_else(|| anyhow!("Missing FileHash"))?,
        )?;

        let file_size = fields
            .get("FileSize")
            .ok_or_else(|| anyhow!("Missing FileSize"))?
            .parse()?;

        let nar_hash = NarHash::from_sri(
            fields
                .get("NarHash")
                .ok_or_else(|| anyhow!("Missing NarHash"))?,
        )?;

        let nar_size = fields
            .get("NarSize")
            .ok_or_else(|| anyhow!("Missing NarSize"))?
            .parse()?;

        let references = fields
            .get("References")
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        let deriver = fields.get("Deriver").map(|s| s.to_string());
        let ca = fields.get("CA").map(|s| s.to_string());

        Ok(NarInfo {
            store_path,
            url,
            compression,
            file_hash,
            file_size,
            nar_hash,
            nar_size,
            references,
            deriver,
            signatures,
            ca,
        })
    }
}

impl fmt::Display for NarInfo {
    /// Serialize to the narinfo file format.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "StorePath: {}", self.store_path)?;
        writeln!(f, "URL: {}", self.url)?;
        writeln!(f, "Compression: {}", self.compression)?;
        writeln!(f, "FileHash: {}", self.file_hash)?;
        writeln!(f, "FileSize: {}", self.file_size)?;
        writeln!(f, "NarHash: {}", self.nar_hash)?;
        writeln!(f, "NarSize: {}", self.nar_size)?;

        if !self.references.is_empty() {
            writeln!(f, "References: {}", self.references.join(" "))?;
        }

        if let Some(ref deriver) = self.deriver {
            writeln!(f, "Deriver: {}", deriver)?;
        }

        for sig in &self.signatures {
            writeln!(f, "Sig: {}", sig)?;
        }

        if let Some(ref ca) = self.ca {
            writeln!(f, "CA: {}", ca)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_NARINFO: &str = r#"StorePath: /nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10
URL: nar/1234567890abcdef.nar.xz
Compression: xz
FileHash: sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=
FileSize: 12345
NarHash: sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=
NarSize: 67890
References: abc123def456ghi789jkl012mno345pq-glibc-2.35
Deriver: xyz789abc123def456ghi012jkl345mno-hello-2.10.drv
Sig: cache.nixos.org-1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==
Sig: my-cache-1:AAcOFRwjKjE4P0ZNVFtiaXB3foWMk5qhqK+2vcTL0tng5+71/AMKERgfJi00O0JJUFdeZWxzeoGIj5adpKuyuQ==
"#;

    #[test]
    fn test_parse_narinfo() {
        let info = SAMPLE_NARINFO.parse::<NarInfo>().unwrap();

        assert_eq!(
            info.store_path,
            "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10"
        );
        assert_eq!(info.url, "nar/1234567890abcdef.nar.xz");
        assert_eq!(info.compression, Compression::Xz);
        assert_eq!(info.file_size, 12345);
        assert_eq!(info.nar_size, 67890);
        assert_eq!(info.references.len(), 1);
        assert_eq!(info.signatures.len(), 2);
    }

    #[test]
    fn test_serialize_roundtrip() {
        let info = SAMPLE_NARINFO.parse::<NarInfo>().unwrap();
        let serialized = info.to_string();
        let reparsed = serialized.parse::<NarInfo>().unwrap();

        assert_eq!(info.store_path, reparsed.store_path);
        assert_eq!(info.url, reparsed.url);
        assert_eq!(info.compression, reparsed.compression);
        assert_eq!(info.signatures.len(), reparsed.signatures.len());
    }

    #[test]
    fn test_has_trusted_signature() {
        let info = SAMPLE_NARINFO.parse::<NarInfo>().unwrap();

        let trusted = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.has_trusted_signature(&trusted));

        let untrusted = vec!["other-cache:publickey".to_string()];
        assert!(!info.has_trusted_signature(&untrusted));

        assert!(!info.has_trusted_signature(&[]));
    }

    /// A valid signature under `name` (the body is arbitrary 64-byte content).
    fn sig(name: &str) -> Signature {
        format!("{name}:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==")
            .parse()
            .unwrap()
    }

    /// Helper to create a base NarInfo for testing
    fn base_narinfo() -> NarInfo {
        NarInfo {
            store_path: "/nix/store/abc123-test".to_string(),
            url: "nar/test.nar.zst".to_string(),
            compression: Compression::Zstd,
            file_hash: NarHash::from_hex(&"ab".repeat(32)).unwrap(),
            file_size: 100,
            nar_hash: NarHash::from_hex(&"cd".repeat(32)).unwrap(),
            nar_size: 200,
            references: vec![],
            deriver: None,
            signatures: vec![],
            ca: None,
        }
    }

    #[test]
    fn test_is_trusted_ca_no_sigs_with_trusted_keys() {
        // CA path with no signatures, trusted keys configured → is_trusted returns true
        let info = NarInfo {
            ca: Some("fixed:sha256:abc".to_string()),
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_ca_no_sigs_no_trusted_keys() {
        // CA path with no signatures, no trusted keys → is_trusted returns true
        let info = NarInfo {
            ca: Some("fixed:sha256:abc".to_string()),
            ..base_narinfo()
        };
        assert!(info.is_trusted(&[]));
    }

    #[test]
    fn test_is_trusted_ca_with_trusted_sig() {
        // CA path with trusted signature → is_trusted returns true
        let info = NarInfo {
            ca: Some("fixed:sha256:abc".to_string()),
            signatures: vec![sig("cache.nixos.org-1")],
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_non_ca_with_trusted_sig() {
        // Non-CA path with trusted signature → is_trusted returns true
        let info = NarInfo {
            signatures: vec![sig("cache.nixos.org-1")],
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_non_ca_without_sig() {
        // Non-CA path without trusted signature → is_trusted returns false
        let info = base_narinfo();
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(!info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_non_ca_empty_trusted_keys() {
        // Non-CA path with empty trusted keys → is_trusted returns true (no trust restriction)
        let info = base_narinfo();
        assert!(info.is_trusted(&[]));
    }

    #[test]
    fn test_is_trusted_empty_ca_string() {
        // CA field with empty string Some("") → treated as non-CA (returns false without signatures)
        let info = NarInfo {
            ca: Some("".to_string()),
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(!info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_whitespace_ca_string() {
        // CA field with whitespace Some("  ") → treated as non-CA
        let info = NarInfo {
            ca: Some("  ".to_string()),
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(!info.is_trusted(&trusted_keys));
    }
}
