//! Store-path metadata, mirroring the fields of `nix path-info --json`.

use std::collections::HashMap;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;

use crate::types::NarHash;

/// Information about a store path.
///
/// These are exactly the fields reported by `nix path-info --json` that
/// `ogygia-irisd` needs to build a narinfo. [`NixDb::find_path_info`] produces
/// the same value directly from the Nix database;
/// [`parse_path_info_json`] produces it from the command's output. The two are
/// asserted equal by the equivalence tests.
///
/// [`NixDb::find_path_info`]: crate::db::NixDb::find_path_info
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathInfo {
    /// Full store path.
    pub path: String,
    /// NAR content hash.
    pub nar_hash: NarHash,
    /// NAR size in bytes.
    pub nar_size: u64,
    /// References (other store paths this path depends on), sorted by path.
    pub references: Vec<String>,
    /// Deriver path, if known.
    pub deriver: Option<String>,
    /// Signatures over the NAR (`<key-name>:<base64>`).
    pub signatures: Vec<String>,
    /// Content-addressing descriptor (e.g. `fixed:sha256:…`), if content-addressed.
    pub ca: Option<String>,
}

impl PathInfo {
    /// Whether this path is suitable for serving to peers.
    ///
    /// A path is serveable if it is content-addressed (self-verifying) or has
    /// at least one signature.
    pub fn is_serveable(&self) -> bool {
        self.ca.is_some() || !self.signatures.is_empty()
    }
}

/// Raw JSON object from `nix path-info --json`.
///
/// The store path itself is the map key, not a field, so it is supplied
/// separately when constructing the [`PathInfo`].
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

/// Parse the output of `nix path-info --json` into [`PathInfo`] values.
///
/// `nix path-info --json` returns a map keyed by store path, where some values
/// may be `null` (path not in the store); those entries are dropped. This is
/// the reference implementation the database queries are checked against.
pub fn parse_path_info_json(json: &str) -> Result<Vec<PathInfo>> {
    let infos: HashMap<String, Option<PathInfoJson>> =
        serde_json::from_str(json).context("failed to parse path-info JSON")?;

    let mut result = Vec::new();
    for (path, info) in infos {
        // A null value means the path is not in the store; drop it.
        let Some(info) = info else { continue };
        let nar_hash = NarHash::from_sri(&info.nar_hash)
            .with_context(|| format!("invalid NarHash for {path}"))?;
        result.push(PathInfo {
            path,
            nar_hash,
            nar_size: info.nar_size,
            references: info.references,
            deriver: info.deriver,
            signatures: info.signatures,
            ca: info.ca,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_path() {
        let json = r#"{
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.10": {
                "narHash": "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=",
                "narSize": 12345,
                "references": [
                    "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-glibc-2.35"
                ],
                "deriver": "/nix/store/cccccccccccccccccccccccccccccccc-hello-2.10.drv",
                "signatures": ["cache.nixos.org-1:sig"],
                "ca": null
            }
        }"#;

        let mut infos = parse_path_info_json(json).unwrap();
        assert_eq!(infos.len(), 1);
        let info = infos.pop().unwrap();
        assert_eq!(
            info.path,
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.10"
        );
        assert_eq!(
            info.nar_hash,
            NarHash::from_sri("sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=").unwrap()
        );
        assert_eq!(info.nar_size, 12345);
        assert_eq!(info.references.len(), 1);
        assert_eq!(
            info.deriver.as_deref(),
            Some("/nix/store/cccccccccccccccccccccccccccccccc-hello-2.10.drv")
        );
        assert_eq!(info.signatures, vec!["cache.nixos.org-1:sig"]);
        assert_eq!(info.ca, None);
    }

    #[test]
    fn parse_drops_null_values_and_defaults_missing_fields() {
        // A null value (path not valid) is dropped; references/signatures
        // default to empty; deriver and ca are absent (None).
        let json = r#"{
            "/nix/store/dddddddddddddddddddddddddddddddd-src": {
                "narHash": "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=",
                "narSize": 7,
                "ca": "fixed:sha256:1abcdef",
                "deriver": null
            },
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-missing": null
        }"#;

        let infos = parse_path_info_json(json).unwrap();
        assert_eq!(infos.len(), 1);
        let info = &infos[0];
        assert!(info.references.is_empty());
        assert!(info.signatures.is_empty());
        assert_eq!(info.deriver, None);
        assert_eq!(info.ca.as_deref(), Some("fixed:sha256:1abcdef"));
        assert!(info.is_serveable());
    }
}
