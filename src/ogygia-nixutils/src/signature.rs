//! A signature over a store path's NAR.
//!
//! Nix writes signatures as `<key-name>:<base64>`, where the base64 payload is
//! a 64-byte Ed25519 signature. Previously these were passed around as bare
//! strings, so every consumer that needed the key name re-ran the same
//! `split_once(':')`. [`Signature`] parses the form once, holds the name and the
//! raw signature bytes, and exposes the name directly — mirroring how
//! [`NarHash`](crate::NarHash) turns a hash's encodings into rendering methods.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Length of an Ed25519 signature in bytes.
const SIG_LEN: usize = 64;

/// A signature over a store path's NAR, in Nix's `<key-name>:<base64>` form.
///
/// The body is the raw 64-byte Ed25519 signature; base64 is just its textual
/// encoding, decoded on parsing ([`FromStr`](std::str::FromStr)) and re-rendered
/// on [`Display`](std::fmt::Display). Holding the decoded bytes means an invalid or
/// wrong-length signature is rejected at its ingestion point rather than
/// surviving as an opaque string, and the key name — the only part callers match
/// against trusted or signing keys — is a direct accessor.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature {
    name: String,
    body: [u8; SIG_LEN],
}

impl Signature {
    /// The signing key's name (the part before the `:`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw 64-byte Ed25519 signature.
    pub fn body(&self) -> &[u8; SIG_LEN] {
        &self.body
    }
}

impl std::str::FromStr for Signature {
    type Err = anyhow::Error;

    /// Parse the `<key-name>:<base64>` form, decoding and length-checking the
    /// Ed25519 signature body.
    fn from_str(s: &str) -> Result<Self> {
        let (name, body) = s
            .split_once(':')
            .with_context(|| format!("malformed signature, expected `<name>:<base64>`: {s}"))?;
        let body: [u8; SIG_LEN] = BASE64
            .decode(body)
            .with_context(|| format!("invalid base64 in signature: {s}"))?
            .try_into()
            .map_err(|v: Vec<u8>| {
                anyhow!("signature is {} bytes, expected {SIG_LEN}: {s}", v.len())
            })?;
        Ok(Self {
            name: name.to_owned(),
            body,
        })
    }
}

impl std::fmt::Display for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.name, BASE64.encode(self.body))
    }
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signature({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `name:base64` for 64 arbitrary bytes — a well-formed signature.
    fn sample() -> String {
        format!("my-cache-1:{}", BASE64.encode([7u8; SIG_LEN]))
    }

    #[test]
    fn parses_name_and_body() {
        let sig: Signature = sample().parse().unwrap();
        assert_eq!(sig.name(), "my-cache-1");
        assert_eq!(sig.body(), &[7u8; SIG_LEN]);
    }

    #[test]
    fn display_round_trips() {
        let text = sample();
        assert_eq!(text.parse::<Signature>().unwrap().to_string(), text);
    }

    #[test]
    fn rejects_missing_colon() {
        assert!("no-colon-here".parse::<Signature>().is_err());
    }

    #[test]
    fn rejects_bad_base64() {
        assert!("name:not valid base64!!".parse::<Signature>().is_err());
    }

    #[test]
    fn rejects_wrong_length() {
        // Valid base64 for 3 bytes, not 64.
        assert!("name:YWJj".parse::<Signature>().is_err());
    }
}
