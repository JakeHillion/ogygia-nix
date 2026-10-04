//! Store path computation, as specified in the Nix manual's "Store Path"
//! and "Nix Archive" sections. Nothing here writes to a store.

use std::io::Write;

use anyhow::Result;
use anyhow::bail;
use sha2::Digest;
use sha2::Sha256;

pub const STORE_DIR: &str = "/nix/store";

const BASE32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Nix's base-32 encoding (least significant bits first, reversed).
pub fn base32_encode(bytes: &[u8]) -> String {
    let len = (bytes.len() * 8).div_ceil(5);
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let mut c = (bytes[i] as u16) >> j;
        if i + 1 < bytes.len() {
            c |= (bytes[i + 1] as u16) << (8 - j);
        }
        out.push(BASE32_CHARS[(c & 0x1f) as usize] as char);
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let len = s.len() * 5 / 8;
    let mut out = vec![0u8; len];
    for (n, ch) in s.bytes().rev().enumerate() {
        let digit = BASE32_CHARS.iter().position(|&c| c == ch)? as u16;
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        if i < len {
            out[i] |= (digit << j) as u8;
        }
        let carry = digit >> (8 - j);
        if i + 1 < len {
            out[i + 1] |= carry as u8;
        } else if carry != 0 {
            return None;
        }
    }
    Some(out)
}

/// XOR-fold a hash down to `size` bytes.
fn compress(hash: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for (i, b) in hash.iter().enumerate() {
        out[i % size] ^= b;
    }
    out
}

/// `makeStorePath`: the store path for a fingerprint `ty:sha256:<hex>:<dir>:<name>`.
pub fn make_store_path(ty: &str, inner_sha256: &[u8], name: &str) -> String {
    let fingerprint = format!(
        "{ty}:sha256:{}:{STORE_DIR}:{name}",
        hex::encode(inner_sha256)
    );
    let h = Sha256::digest(fingerprint.as_bytes());
    format!("{STORE_DIR}/{}-{name}", base32_encode(&compress(&h, 20)))
}

/// Type prefix with references appended, as used for `source` and `text`.
fn type_with_refs(ty: &str, refs: &[&str], self_ref: bool) -> String {
    let mut s = ty.to_owned();
    let mut refs: Vec<&str> = refs.to_vec();
    refs.sort();
    for r in refs {
        s.push(':');
        s.push_str(r);
    }
    if self_ref {
        s.push_str(":self");
    }
    s
}

/// The path of a NAR-hashed (recursive SHA-256) source with no references,
/// such as a flake input or a copied path.
pub fn source_path(nar_sha256: &[u8], name: &str) -> String {
    make_store_path("source", nar_sha256, name)
}

/// The path `builtins.toFile` writes `contents` to.
pub fn text_path(contents: &[u8], name: &str, refs: &[&str]) -> String {
    let h = Sha256::digest(contents);
    make_store_path(&type_with_refs("text", refs, false), &h, name)
}

/// The path of a fixed-output derivation's output.
pub fn fixed_output_path(recursive: bool, algo: &str, hash: &[u8], name: &str) -> String {
    if recursive && algo == "sha256" {
        return make_store_path("source", hash, name);
    }
    let inner = format!(
        "fixed:out:{}{algo}:{}:",
        if recursive { "r:" } else { "" },
        hex::encode(hash)
    );
    let h = Sha256::digest(inner.as_bytes());
    make_store_path("output:out", &h, name)
}

/// Check a store path name for characters Nix rejects.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("store path name is empty");
    }
    if name.len() > 211 {
        bail!("store path name '{name}' is longer than 211 characters");
    }
    if name == "." || name == ".." {
        bail!("store path name '{name}' is not valid");
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "+-._?=".contains(*c)))
    {
        bail!("store path name '{name}' contains illegal character '{c}'");
    }
    Ok(())
}

/// `parseStorePath`: the canonical form of `path` if it names a store
/// object rather than a path inside one.
pub fn parse_store_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let path = crate::path::canon_path(path);
    let base = path.strip_prefix(STORE_DIR)?.strip_prefix('/')?;
    let (hash, name) = (base.as_bytes().get(..32)?, base.get(33..)?);
    if base.contains('/') || !hash.iter().all(|c| BASE32_CHARS.contains(c)) {
        return None;
    }
    check_name(name).ok()?;
    Some(path)
}

/// Write one NAR string: its length, the bytes, and padding to 8 bytes.
pub fn nar_str(w: &mut impl Write, s: &[u8]) -> std::io::Result<()> {
    w.write_all(&(s.len() as u64).to_le_bytes())?;
    w.write_all(s)?;
    let pad = (8 - s.len() % 8) % 8;
    w.write_all(&[0u8; 8][..pad])
}

/// Digest size in bytes of a hash algorithm Nix supports.
pub fn hash_size(algo: &str) -> Option<usize> {
    Some(match algo {
        "md5" => 16,
        "sha1" => 20,
        "sha256" => 32,
        "sha512" => 64,
        _ => return None,
    })
}

/// Parse a hash in any format Nix accepts: SRI (`sha256-<base64>`), or
/// base16, Nix base-32 or base64 of a digest of `algo`. Returns the
/// algorithm and the digest.
pub fn parse_hash(s: &str, algo: Option<&str>) -> Result<(String, Vec<u8>)> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    if let Some((a, rest)) = s.split_once('-')
        && let Some(size) = hash_size(a)
    {
        if let Some(expected) = algo
            && expected != a
        {
            bail!("hash '{s}' should have type '{expected}'");
        }
        let bytes = b64
            .decode(rest)
            .map_err(|e| anyhow::anyhow!("invalid SRI hash '{s}': {e}"))?;
        if bytes.len() != size {
            bail!("invalid SRI hash '{s}': wrong length");
        }
        return Ok((a.to_owned(), bytes));
    }
    let (algo, s) = match s.split_once(':') {
        Some((a, rest)) if hash_size(a).is_some() => (a, rest),
        _ => match algo {
            Some(a) => (a, s),
            None => bail!("hash '{s}' does not include a type"),
        },
    };
    let Some(size) = hash_size(algo) else {
        bail!("unknown hash algorithm '{algo}'");
    };
    let bytes = if s.len() == size * 2 {
        hex::decode(s).ok()
    } else if s.len() == (size * 8).div_ceil(5) {
        base32_decode(s)
    } else if s.len() == size.div_ceil(3) * 4 {
        b64.decode(s).ok()
    } else {
        None
    };
    match bytes {
        Some(b) if b.len() == size => Ok((algo.to_owned(), b)),
        _ => bail!("invalid hash '{s}' for algorithm '{algo}'"),
    }
}

/// Render a digest in one of Nix's hash formats.
pub fn format_hash(algo: &str, bytes: &[u8], format: &str) -> Result<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(match format {
        "base16" => hex::encode(bytes),
        "nix32" | "base32" => base32_encode(bytes),
        "base64" => b64.encode(bytes),
        "sri" => format!("{algo}-{}", b64.encode(bytes)),
        _ => bail!("unknown hash format '{format}'"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trip() {
        let bytes: Vec<u8> = (0..32).collect();
        let s = base32_encode(&bytes);
        assert_eq!(s.len(), 52);
        assert_eq!(base32_decode(&s).unwrap(), bytes);
    }

    #[test]
    fn text_path_matches_nix() {
        // `builtins.toFile "x" "y"` in Nix 2.34.
        assert_eq!(
            text_path(b"y", "x", &[]),
            "/nix/store/lfngsssysp6h1v4ccqg23c52s9sjl779-x"
        );
    }

    #[test]
    fn flake_input_path_from_nar_hash() {
        // nixpkgs as locked by the NixOS configuration this crate targets.
        let (_, h) =
            parse_hash("sha256-62XMQD4WLdMAfdT0/8gJmPH2dKeTw908ryNloAplTr8=", None).unwrap();
        assert_eq!(
            source_path(&h, "source"),
            "/nix/store/0c7qm6wkgdg9dd3ws34vvg9xacdwa0dq-source"
        );
    }
}
