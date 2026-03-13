//! Nix store path types and narinfo conversion.

use crate::nix::narinfo::Compression;
use crate::nix::narinfo::NarInfo;

/// Information about a store path.
#[derive(Debug, Clone)]
pub struct PathInfo {
    /// Full store path
    pub path: String,
    /// NAR hash in SRI format (e.g. `sha256-<base64>`)
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

impl PathInfo {
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

#[cfg(test)]
mod tests {
    use super::*;

    impl PathInfo {
        fn is_serveable(&self) -> bool {
            self.ca.is_some() || !self.signatures.is_empty()
        }
    }

    #[test]
    fn test_path_info_to_narinfo() {
        let path_info = PathInfo {
            path: "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10".to_string(),
            nar_hash: "sha256-ASNFZ4mrze8=".to_string(),
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
        assert_eq!(narinfo.nar_hash, "sha256-ASNFZ4mrze8=");
        assert_eq!(narinfo.nar_size, 12345);
        assert_eq!(narinfo.references.len(), 1);
        assert_eq!(narinfo.signatures.len(), 1);
    }

    #[test]
    fn test_is_serveable() {
        let base = PathInfo {
            path: "/nix/store/abc-test".to_string(),
            nar_hash: "sha256-AAAA".to_string(),
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
