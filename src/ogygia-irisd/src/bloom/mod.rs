//! Bloom filter based store path indexing and peer lookup

/// Fixed seed so all nodes produce compatible hashes.
const BLOOM_SEED: u128 = 0x0009_791a_1215_b100_5eed_2025;

pub mod burst;
pub mod local;
pub mod peers;
pub mod wire;
