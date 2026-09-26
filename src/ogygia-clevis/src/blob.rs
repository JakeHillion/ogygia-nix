//! Reads which Tang pins an existing Clevis blob is bound to. Everything
//! needed is in the JWE headers, so no key material is touched.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

use crate::config::Pin;

/// Every Tang pin the blob is bound to, one per signing key advertised in
/// it, in the order they appear.
pub fn bound_pins(jwe: &str) -> Result<Vec<Pin>> {
    let mut pins = Vec::new();
    collect(&protected_header(jwe)?, &mut pins)?;
    Ok(pins)
}

fn protected_header(jwe: &str) -> Result<Value> {
    let (header, _) = jwe.trim().split_once('.').context("not a compact JWE")?;
    let raw = URL_SAFE_NO_PAD
        .decode(header)
        .context("decoding JWE protected header")?;
    serde_json::from_slice(&raw).context("parsing JWE protected header")
}

fn collect(header: &Value, pins: &mut Vec<Pin>) -> Result<()> {
    let clevis = header
        .get("clevis")
        .context("JWE header has no clevis section")?;
    match clevis.get("pin").and_then(Value::as_str) {
        Some("tang") => {
            let tang = clevis.get("tang").context("tang pin has no tang section")?;
            let url = tang
                .get("url")
                .and_then(Value::as_str)
                .context("tang pin has no url")?;
            let adv = tang.get("adv").context("tang pin has no advertisement")?;
            for key in verify_keys(adv)? {
                pins.push(Pin {
                    url: url.to_owned(),
                    thp: thumbprint(key)?,
                });
            }
        }
        Some("sss") => {
            let inner = clevis
                .get("sss")
                .and_then(|sss| sss.get("jwe"))
                .and_then(Value::as_array)
                .context("sss pin has no inner JWEs")?;
            for jwe in inner {
                let jwe = jwe.as_str().context("inner JWE is not a string")?;
                collect(&protected_header(jwe)?, pins)?;
            }
        }
        Some(pin) => bail!("unsupported clevis pin {pin:?}"),
        None => bail!("JWE header has no clevis pin"),
    }
    Ok(())
}

/// The keys in a JWK set that may verify signatures.
pub fn verify_keys(jwks: &Value) -> Result<impl Iterator<Item = &Value>> {
    let keys = jwks
        .get("keys")
        .and_then(Value::as_array)
        .context("JWK set has no keys")?;
    Ok(keys.iter().filter(|key| {
        key.get("key_ops")
            .and_then(Value::as_array)
            .is_some_and(|ops| ops.iter().any(|op| op == "verify"))
    }))
}

/// RFC 7638 SHA-256 thumbprint of an EC JWK, as `tang-show-keys` prints it.
pub fn thumbprint(jwk: &Value) -> Result<String> {
    ensure!(
        jwk.get("kty").and_then(Value::as_str) == Some("EC"),
        "only EC keys are supported"
    );
    let member = |name: &str| {
        jwk.get(name)
            .and_then(Value::as_str)
            .with_context(|| format!("JWK has no {name}"))
    };
    // RFC 7638 hashes the required members in lexicographic order with no
    // whitespace, which is exactly what this literal serializes to.
    let canonical = serde_json::json!({
        "crv": member("crv")?,
        "kty": "EC",
        "x": member("x")?,
        "y": member("y")?,
    });
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `echo -n supersecret | clevis encrypt sss` against two local tang
    /// servers, with thumbprints as reported by `tang-show-keys`.
    const SSS: &str = include_str!("../tests/fixtures/sss.jwe");

    fn expected() -> Vec<Pin> {
        vec![
            Pin {
                url: "http://127.0.0.1:17654".into(),
                thp: "4vKNtY1V4oA0LVx3xB8JaJcEaNHBoEra8fWASu4rQXs".into(),
            },
            Pin {
                url: "http://127.0.0.1:17655".into(),
                thp: "aaaQybA3Yh0iFMi7D9B3coUsVck2Aw489J4m93K8Iag".into(),
            },
        ]
    }

    #[test]
    fn reads_pins_from_sss_blob() {
        assert_eq!(bound_pins(SSS).unwrap(), expected());
    }

    #[test]
    fn reads_pin_from_plain_tang_blob() {
        let header = protected_header(SSS).unwrap();
        let inner = header["clevis"]["sss"]["jwe"][0].as_str().unwrap();
        assert_eq!(bound_pins(inner).unwrap(), expected()[..1]);
    }

    #[test]
    fn rejects_non_clevis_input() {
        assert!(bound_pins("not a jwe").is_err());
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"dir"}"#);
        assert!(bound_pins(&format!("{header}.a.b.c.d")).is_err());
    }

    #[test]
    fn thumbprint_matches_tang() {
        let key = serde_json::json!({
            "alg": "ES512",
            "crv": "P-521",
            "key_ops": ["verify"],
            "kty": "EC",
            "x": "AB3iWXDf8AgnWPi1DxmmHRYoKLJ5ITesSnxZW5XaYb8F5jCrbhaTsqYUBWhxC6tZkyR5y5g_bK_5EtDLJ3cjOMuJ",
            "y": "AJfNqCf7Po7mPy2GPiJk6bOlMit_h7XsB0w_52tS_8Apt2BUazkkpyVUt5K1E9RyRmMaIsEzVuJi8BwcMR7f8ZPa",
        });
        assert_eq!(
            thumbprint(&key).unwrap(),
            "4vKNtY1V4oA0LVx3xB8JaJcEaNHBoEra8fWASu4rQXs"
        );
    }
}
