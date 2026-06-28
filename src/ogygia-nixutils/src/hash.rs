//! Hash format conversions used when reading the Nix database.

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Convert a Nix database hash (`<algo>:<hex>`, e.g. `sha256:4b4c…`) to SRI
/// format (`<algo>-<base64>`, e.g. `sha256-S0ym…`).
///
/// The `ValidPaths.hash` column stores the NAR hash as `<algo>:<hex>`, whereas
/// narinfo consumers (and `nix path-info --json`) expect the SRI encoding.
pub fn hex_hash_to_sri(db_hash: &str) -> Result<String> {
    let (algo, hex_str) = db_hash
        .split_once(':')
        .context("invalid hash format in Nix database: missing ':'")?;
    let bytes = hex::decode(hex_str)
        .with_context(|| format!("invalid hex in Nix database hash: {hex_str}"))?;
    let b64 = BASE64.encode(&bytes);
    Ok(format!("{algo}-{b64}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_hash_to_sri_sha256() {
        let db_hash = "sha256:4b4ca6bc50c269e51f6525b5bdeab4814d765a857cd2a6525dc832965c02cd96";
        let sri = hex_hash_to_sri(db_hash).unwrap();
        assert_eq!(sri, "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=");
    }

    #[test]
    fn hex_hash_to_sri_missing_colon() {
        assert!(hex_hash_to_sri("sha256deadbeef").is_err());
    }

    #[test]
    fn hex_hash_to_sri_invalid_hex() {
        assert!(hex_hash_to_sri("sha256:xyz").is_err());
    }
}
