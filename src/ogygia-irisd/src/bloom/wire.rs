//! Shared wire format for bloom filter serialization.
//!
//! Wire layout:
//! ```text
//! [8 bytes]    SipHash of the BloomHeader (BE u64)
//! [12 bytes]   bincode-serialized BloomHeader
//! [N×8 bytes]  BE u64 words of the bit vector
//! ```
//!
//! The header hash provides naive versioning: `BloomHeader` derives
//! `Hash`, so if the struct definition changes the receiver's hash of
//! the deserialized header won't match the wire hash.

use std::hash::Hash;
use std::hash::Hasher;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use siphasher::sip::SipHasher;

/// Bloom filter wire format header.
#[derive(Debug, Hash, Serialize, Deserialize, PartialEq)]
pub struct BloomHeader {
    pub num_hashes: u32,
    pub num_bits: u64,
}

impl BloomHeader {
    /// Compute a deterministic hash via the `Hash` impl (used for wire versioning).
    fn sip_hash(&self) -> u64 {
        let mut hasher = SipHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// Serialize just the bloom wire-format header (hash + bincode header).
///
/// Returns the small header prefix (~20 bytes). Used by the streaming
/// serialization path where the body is yielded separately.
pub(super) fn serialize_header(header: &BloomHeader) -> Vec<u8> {
    let hash = header.sip_hash();
    let header_bytes = bincode::serialize(header).expect("BloomHeader serialization cannot fail");
    let mut buf = Vec::with_capacity(8 + header_bytes.len());
    buf.extend_from_slice(&hash.to_be_bytes());
    buf.extend_from_slice(&header_bytes);
    buf
}

/// Serialize a bloom header and bit-vector body into the wire format.
///
/// Consumes the word iterator directly — no intermediate collection.
/// Used only in tests; the production path streams via `serialize_header`
/// plus batched word iteration.
#[cfg(test)]
fn serialize(header: &BloomHeader, words: impl Iterator<Item = u64>) -> Vec<u8> {
    let hash = header.sip_hash();
    let header_bytes = bincode::serialize(header).expect("BloomHeader serialization cannot fail");
    let word_count = (header.num_bits as usize).div_ceil(64);

    let mut buf = Vec::with_capacity(8 + header_bytes.len() + word_count * 8);
    buf.extend_from_slice(&hash.to_be_bytes());
    buf.extend_from_slice(&header_bytes);
    for word in words {
        buf.extend_from_slice(&word.to_be_bytes());
    }
    buf
}

/// Deserialize a bloom from the wire format.
///
/// Validates the header hash (format versioning) and internal
/// consistency (body length matches `header.num_bits`). Returns the
/// parsed header and u64 words for the caller to validate against
/// local expectations.
pub fn deserialize(data: &[u8]) -> Result<(BloomHeader, Vec<u64>)> {
    anyhow::ensure!(data.len() >= 8, "bloom data too short for header hash");

    let wire_hash = u64::from_be_bytes(
        data[..8]
            .try_into()
            .context("bloom data too short for header hash")?,
    );

    let header: BloomHeader = bincode::deserialize(&data[8..])
        .map_err(|e| anyhow::anyhow!("failed to deserialize bloom header: {}", e))?;

    let computed_hash = header.sip_hash();
    anyhow::ensure!(
        computed_hash == wire_hash,
        "bloom header hash mismatch (incompatible wire format version): \
         wire {:#018x}, local {:#018x}",
        wire_hash,
        computed_hash,
    );

    let header_size =
        bincode::serialized_size(&header).context("failed to compute bloom header size")? as usize;
    let body = &data[8 + header_size..];
    anyhow::ensure!(
        body.len().is_multiple_of(8),
        "bloom body not aligned to u64"
    );

    let words: Vec<u64> = body
        .chunks_exact(8)
        .map(|c| u64::from_be_bytes(c.try_into().expect("chunks_exact guarantees 8 bytes")))
        .collect();

    let expected_words = (header.num_bits as usize).div_ceil(64);
    anyhow::ensure!(
        words.len() == expected_words,
        "bloom body length mismatch: {} words but header declares {} bits ({} words)",
        words.len(),
        header.num_bits,
        expected_words,
    );

    Ok((header, words))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_hash_detects_mismatch() {
        let header = BloomHeader {
            num_hashes: 7,
            num_bits: 128,
        };
        let data = serialize(&header, [0u64, 0].into_iter());

        // Corrupt the header hash.
        let mut bad = data.clone();
        bad[0] ^= 0xFF;
        let err = deserialize(&bad).unwrap_err();
        assert!(
            err.to_string().contains("header hash mismatch"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn roundtrip() {
        let header = BloomHeader {
            num_hashes: 7,
            num_bits: 192,
        };
        let words = vec![0xDEAD_BEEF_CAFE_BABEu64, 0x1234_5678_9ABC_DEF0, 0];
        let data = serialize(&header, words.iter().copied());
        let (parsed_header, parsed_words) = deserialize(&data).unwrap();
        assert_eq!(parsed_header, header);
        assert_eq!(parsed_words, words);
    }

    #[test]
    fn body_length_mismatch() {
        let header = BloomHeader {
            num_hashes: 7,
            num_bits: 64, // expects 1 word
        };

        // Craft a payload where the header declares 64 bits (1 word)
        // but the body contains 2 words.
        let hash = header.sip_hash();
        let header_bytes = bincode::serialize(&header).unwrap();
        let mut bad = Vec::new();
        bad.extend_from_slice(&hash.to_be_bytes());
        bad.extend_from_slice(&header_bytes);
        bad.extend_from_slice(&1u64.to_be_bytes());
        bad.extend_from_slice(&2u64.to_be_bytes());

        let err = deserialize(&bad).unwrap_err();
        assert!(
            err.to_string().contains("body length mismatch"),
            "unexpected error: {}",
            err
        );
    }
}
