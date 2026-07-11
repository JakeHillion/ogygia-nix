//! Strongly-typed Nix hashes.
//!
//! Store-path metadata carries hashes that were previously passed around as
//! bare strings. This module gives each a type that owns its validation and
//! encoding, so a value can't be built in the wrong format and the encodings
//! become explicit methods rather than ad-hoc string surgery.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// A SHA-256 NAR content hash, stored as its 32 raw digest bytes.
///
/// A NAR hash reaches this crate in three textual encodings — the Nix database
/// column form `sha256:<hex>`, the SRI form `sha256-<base64>` used by
/// `nix path-info --json` and narinfo files, and the bare hex embedded in NAR
/// URLs. Holding the raw digest turns those encodings into rendering methods
/// ([`to_sri`](Self::to_sri), [`to_hex`](Self::to_hex)) rather than
/// prefix-sniffing string surgery, and lets two hashes be compared by value
/// regardless of how each was written.
///
/// Only SHA-256 is represented; other algorithms are rejected when parsing.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NarHash([u8; 32]);

impl NarHash {
    /// Construct from a raw 32-byte SHA-256 digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parse the Nix database column form `sha256:<hex>`.
    pub fn from_db_str(s: &str) -> Result<Self> {
        Self::from_hex(expect_sha256(s, ':')?)
    }

    /// Parse the SRI form `sha256-<base64>` (narinfo / `nix path-info --json`).
    pub fn from_sri(s: &str) -> Result<Self> {
        let bytes: [u8; 32] = BASE64
            .decode(expect_sha256(s, '-')?)
            .with_context(|| format!("invalid base64 in NAR hash: {s}"))?
            .try_into()
            .map_err(|v: Vec<u8>| anyhow!("NAR hash is {} bytes, expected 32: {s}", v.len()))?;
        Ok(Self(bytes))
    }

    /// Parse a bare lowercase-hex SHA-256 digest, as embedded in NAR URLs.
    pub fn from_hex(hex_str: &str) -> Result<Self> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(hex_str, &mut bytes)
            .with_context(|| format!("invalid sha256 hex NAR hash: {hex_str}"))?;
        Ok(Self(bytes))
    }

    /// Render as SRI `sha256-<base64>` — the narinfo `NarHash` form.
    pub fn to_sri(&self) -> String {
        format!("sha256-{}", BASE64.encode(self.0))
    }

    /// Render as bare lowercase hex — the form used in NAR URLs.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The raw 32-byte digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// SRI is the canonical textual form.
impl std::fmt::Display for NarHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_sri())
    }
}

impl std::fmt::Debug for NarHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NarHash({})", self.to_sri())
    }
}

/// Split `sha256<sep><payload>`, rejecting any other algorithm or a missing
/// separator. The separator differs by encoding (`:` for the database, `-` for
/// SRI); the payload never contains it, so the split is unambiguous.
fn expect_sha256(s: &str, sep: char) -> Result<&str> {
    let (algo, payload) = s
        .split_once(sep)
        .with_context(|| format!("malformed hash, expected `sha256{sep}…`: {s}"))?;
    if algo != "sha256" {
        bail!("unsupported hash algorithm `{algo}` (only sha256 is supported): {s}");
    }
    Ok(payload)
}

/// The 32-character nixbase32 hash that prefixes a `/nix/store/<hash>-<name>`
/// store path.
///
/// Validated on construction — 32 characters drawn from Nix's base32 alphabet —
/// so a value of this type is always a well-formed store-path hash and can't be
/// confused with an arbitrary string when used as a database lookup key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct StoreHash(String);

impl StoreHash {
    /// Length of a store-path hash in nixbase32 characters.
    const LEN: usize = 32;

    /// The hash as its 32-character nixbase32 string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Extract the store-path hash from a full `/nix/store/<hash>-<name>` path.
    ///
    /// The hash is the 32 nixbase32 characters immediately after the
    /// `/nix/store/` prefix. This is the one validated way to turn a store path
    /// into a [`StoreHash`], replacing the ad-hoc `strip_prefix`/slice/parse
    /// dance repeated across call sites.
    pub fn from_store_path(path: &str) -> Result<Self> {
        let rest = path
            .strip_prefix("/nix/store/")
            .with_context(|| format!("not a /nix/store path: {path}"))?;
        let hash = rest
            .get(..Self::LEN)
            .with_context(|| format!("store path too short to contain a hash: {path}"))?;
        hash.parse()
    }
}

impl std::str::FromStr for StoreHash {
    type Err = anyhow::Error;

    /// Parse and validate a 32-character nixbase32 store-path hash.
    fn from_str(s: &str) -> Result<Self> {
        if s.len() != Self::LEN {
            bail!(
                "store-path hash must be {} characters, got {}: {s}",
                Self::LEN,
                s.len(),
            );
        }
        if let Some(c) = s.chars().find(|&c| !is_nixbase32(c)) {
            bail!("invalid nixbase32 character {c:?} in store-path hash: {s}");
        }
        Ok(Self(s.to_owned()))
    }
}

impl std::fmt::Display for StoreHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Nix's base32 alphabet: digits plus lowercase consonants, omitting `e o u t`.
fn is_nixbase32(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='d' | 'f'..='n' | 'p'..='s' | 'v'..='z')
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real SHA-256 digest in all three of its encodings.
    const DB: &str = "sha256:4b4ca6bc50c269e51f6525b5bdeab4814d765a857cd2a6525dc832965c02cd96";
    const SRI: &str = "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=";
    const HEX: &str = "4b4ca6bc50c269e51f6525b5bdeab4814d765a857cd2a6525dc832965c02cd96";

    #[test]
    fn nar_hash_encodings_agree() {
        let from_db = NarHash::from_db_str(DB).unwrap();
        let from_sri = NarHash::from_sri(SRI).unwrap();
        let from_hex = NarHash::from_hex(HEX).unwrap();
        assert_eq!(from_db, from_sri);
        assert_eq!(from_db, from_hex);
    }

    #[test]
    fn nar_hash_renders_each_encoding() {
        let hash = NarHash::from_db_str(DB).unwrap();
        assert_eq!(hash.to_sri(), SRI);
        assert_eq!(hash.to_hex(), HEX);
        assert_eq!(hash.to_string(), SRI);
    }

    #[test]
    fn nar_hash_from_db_str_rejects_bad_input() {
        assert!(NarHash::from_db_str("sha256deadbeef").is_err()); // missing ':'
        assert!(NarHash::from_db_str("sha256:xyz").is_err()); // not hex
        assert!(NarHash::from_db_str("md5:abcd").is_err()); // wrong algorithm
        // Right shape, wrong length (16 hex chars = 8 bytes).
        assert!(NarHash::from_db_str("sha256:0123456789abcdef").is_err());
    }

    #[test]
    fn nar_hash_from_sri_rejects_wrong_length() {
        // Valid base64 for "abc" — 3 bytes, not 32.
        assert!(NarHash::from_sri("sha256-YWJj").is_err());
    }

    #[test]
    fn nar_hash_from_hex_rejects_wrong_length() {
        assert!(NarHash::from_hex("abcd").is_err());
    }

    #[test]
    fn nar_hash_from_bytes_roundtrips() {
        let hash = NarHash::from_db_str(DB).unwrap();
        assert_eq!(NarHash::from_bytes(*hash.as_bytes()), hash);
    }

    #[test]
    fn store_hash_accepts_valid() {
        let s = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let hash: StoreHash = s.parse().unwrap();
        assert_eq!(hash.as_str(), s);
        assert_eq!(hash.to_string(), s);
    }

    #[test]
    fn store_hash_rejects_wrong_length() {
        assert!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse::<StoreHash>()
                .is_err()
        ); // 31
        assert!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse::<StoreHash>()
                .is_err()
        ); // 33
    }

    #[test]
    fn store_hash_from_store_path_extracts_hash() {
        let hash =
            StoreHash::from_store_path("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.10")
                .unwrap();
        assert_eq!(hash.as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn store_hash_from_store_path_rejects_bad_input() {
        // Missing prefix.
        assert!(StoreHash::from_store_path("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x").is_err());
        // Too short to hold a 32-character hash.
        assert!(StoreHash::from_store_path("/nix/store/aaa").is_err());
        // Right length, but the hash contains a non-nixbase32 character.
        assert!(
            StoreHash::from_store_path("/nix/store/eaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x").is_err()
        );
    }

    #[test]
    fn store_hash_rejects_chars_outside_nixbase32() {
        // e, o, u, t and uppercase are not in Nix's base32 alphabet.
        for bad in ['e', 'o', 'u', 't', 'A'] {
            let s = format!("{bad}{}", "a".repeat(31));
            assert!(s.parse::<StoreHash>().is_err(), "should reject {bad:?}");
        }
    }
}
