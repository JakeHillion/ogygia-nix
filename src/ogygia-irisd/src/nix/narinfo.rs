//! Narinfo parsing and serialization

use std::collections::HashMap;

use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Convert a NarHash from SRI base64 format (`sha256-BASE64==`) to bare hex.
/// Hashes without the SRI prefix are returned unchanged.
pub fn nar_hash_to_hex(hash: &str) -> String {
    if let Some(b64) = hash.strip_prefix("sha256-") {
        let bytes = STANDARD.decode(b64).expect("invalid base64 in NarHash");
        hex::encode(bytes)
    } else {
        hash.to_string()
    }
}

/// Convert a bare hex NarHash back to SRI base64 format (`sha256-BASE64==`).
/// Hashes already in SRI format are returned unchanged.
pub fn nar_hash_to_sri(hash: &str) -> String {
    if hash.starts_with("sha256-") {
        return hash.to_string();
    }
    let bytes = hex::decode(hash).expect("invalid hex in NarHash");
    format!("sha256-{}", STANDARD.encode(bytes))
}

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
    pub file_hash: String,
    /// Size of compressed file
    pub file_size: u64,
    /// Hash of uncompressed NAR
    pub nar_hash: String,
    /// Size of uncompressed NAR
    pub nar_size: u64,
    /// Store path references
    pub references: Vec<String>,
    /// Deriver store path (optional)
    pub deriver: Option<String>,
    /// Signatures
    pub signatures: Vec<String>,
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
    /// Trusted keys are in format "name:base64-public-key".
    /// Signatures are in format "name:base64-signature".
    /// Returns true if any signature's name prefix matches a trusted key's name prefix.
    pub fn has_trusted_signature(&self, trusted_keys: &[String]) -> bool {
        let trusted_names: Vec<&str> = trusted_keys
            .iter()
            .filter_map(|key| key.split_once(':').map(|(name, _)| name))
            .collect();

        self.signatures.iter().any(|sig| {
            sig.split_once(':')
                .is_some_and(|(name, _)| trusted_names.contains(&name))
        })
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

    /// Parse a narinfo file
    pub fn parse(content: &str) -> Result<Self> {
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
                    signatures.push(value.to_string());
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

        let file_hash = fields
            .get("FileHash")
            .ok_or_else(|| anyhow!("Missing FileHash"))?
            .to_string();

        let file_size = fields
            .get("FileSize")
            .ok_or_else(|| anyhow!("Missing FileSize"))?
            .parse()?;

        let nar_hash = fields
            .get("NarHash")
            .ok_or_else(|| anyhow!("Missing NarHash"))?
            .to_string();

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

    /// Serialize to narinfo format
    pub fn serialize(&self) -> String {
        let mut lines = Vec::new();

        lines.push(format!("StorePath: {}", self.store_path));
        lines.push(format!("URL: {}", self.url));
        lines.push(format!("Compression: {}", self.compression));
        lines.push(format!("FileHash: {}", self.file_hash));
        lines.push(format!("FileSize: {}", self.file_size));
        lines.push(format!("NarHash: {}", self.nar_hash));
        lines.push(format!("NarSize: {}", self.nar_size));

        if !self.references.is_empty() {
            lines.push(format!("References: {}", self.references.join(" ")));
        }

        if let Some(ref deriver) = self.deriver {
            lines.push(format!("Deriver: {}", deriver));
        }

        for sig in &self.signatures {
            lines.push(format!("Sig: {}", sig));
        }

        if let Some(ref ca) = self.ca {
            lines.push(format!("CA: {}", ca));
        }

        lines.join("\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_NARINFO: &str = r#"StorePath: /nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10
URL: nar/1234567890abcdef.nar.xz
Compression: xz
FileHash: sha256:abcdef1234567890
FileSize: 12345
NarHash: sha256:fedcba0987654321
NarSize: 67890
References: abc123def456ghi789jkl012mno345pq-glibc-2.35
Deriver: xyz789abc123def456ghi012jkl345mno-hello-2.10.drv
Sig: cache.nixos.org-1:abcdefghijklmnop
Sig: my-cache-1:qrstuvwxyz123456
"#;

    #[test]
    fn test_parse_narinfo() {
        let info = NarInfo::parse(SAMPLE_NARINFO).unwrap();

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
        let info = NarInfo::parse(SAMPLE_NARINFO).unwrap();
        let serialized = info.serialize();
        let reparsed = NarInfo::parse(&serialized).unwrap();

        assert_eq!(info.store_path, reparsed.store_path);
        assert_eq!(info.url, reparsed.url);
        assert_eq!(info.compression, reparsed.compression);
        assert_eq!(info.signatures.len(), reparsed.signatures.len());
    }

    #[test]
    fn test_has_trusted_signature() {
        let info = NarInfo::parse(SAMPLE_NARINFO).unwrap();

        let trusted = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.has_trusted_signature(&trusted));

        let untrusted = vec!["other-cache:publickey".to_string()];
        assert!(!info.has_trusted_signature(&untrusted));

        assert!(!info.has_trusted_signature(&[]));
    }

    /// Helper to create a base NarInfo for testing
    fn base_narinfo() -> NarInfo {
        NarInfo {
            store_path: "/nix/store/abc123-test".to_string(),
            url: "nar/test.nar.zst".to_string(),
            compression: Compression::Zstd,
            file_hash: "sha256:abc".to_string(),
            file_size: 100,
            nar_hash: "sha256:def".to_string(),
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
            signatures: vec!["cache.nixos.org-1:sig".to_string()],
            ..base_narinfo()
        };
        let trusted_keys = vec!["cache.nixos.org-1:publickey".to_string()];
        assert!(info.is_trusted(&trusted_keys));
    }

    #[test]
    fn test_is_trusted_non_ca_with_trusted_sig() {
        // Non-CA path with trusted signature → is_trusted returns true
        let info = NarInfo {
            signatures: vec!["cache.nixos.org-1:sig".to_string()],
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
