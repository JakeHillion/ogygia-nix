//! Local bloom filter for tracking store path hashes
//!
//! Uses a phased replacement scheme: `ArcSwap` for lock-free reads,
//! `RwLock` for serializing writes against rebuild swaps.
//!
//! Filter dimensions (bit count and hash count) are computed from the
//! configured false-positive rate and an estimated entry count. The
//! estimate is a compile-time constant today; future work will replace
//! it with a cluster-derived measurement.

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Result;
use arc_swap::ArcSwap;
use fastbloom::AtomicBloomFilter;

use super::BLOOM_SEED;

/// Estimated number of store paths per node.
///
/// Used together with the configured false-positive rate to size the
/// bloom filter at startup. This is a rough constant for now — future
/// work will derive an accurate count from cluster-wide store metadata.
const ESTIMATED_ENTRIES: usize = 400_000;

/// Compute optimal bloom filter dimensions for `n` elements at the
/// given false-positive rate.
///
/// Returns `(num_bits, num_hashes)`.
fn bloom_params(false_positive_rate: f64, n: usize) -> (usize, u32) {
    let n = n as f64;
    let ln2 = 2.0_f64.ln();
    let num_bits = (-(n * false_positive_rate.ln()) / (ln2 * ln2)).ceil() as usize;
    let num_hashes = ((num_bits as f64 / n) * ln2).round().max(1.0) as u32;
    (num_bits, num_hashes)
}

/// Mutable bloom state: the filter plus element/deletion counters.
///
/// The filter itself lives behind an `Arc` so it can be shared with
/// the lock-free `read_bloom` slot. Counters are only accessible
/// through the `write_bloom` lock — never exposed via `read_bloom`.
struct BloomState {
    filter: Arc<AtomicBloomFilter>,
    element_count: AtomicUsize,
    deletion_count: AtomicUsize,
}

impl BloomState {
    fn new(num_bits: usize, num_hashes: u32) -> Self {
        Self {
            filter: Arc::new(
                AtomicBloomFilter::with_num_bits(num_bits)
                    .seed(&BLOOM_SEED)
                    .hashes(num_hashes),
            ),
            element_count: AtomicUsize::new(0),
            deletion_count: AtomicUsize::new(0),
        }
    }
}

/// Local bloom filter with phased replacement for safe rebuilds.
///
/// Two slots hold the bloom state:
/// - `read_bloom` (`ArcSwap<AtomicBloomFilter>`): serves
///   `serialize()`. Only the filter — no counters. During a rebuild
///   this still points to the old filter so existing paths remain
///   visible.
/// - `write_bloom` (`RwLock<BloomState>`): receives `insert()` and
///   `mark_deletion()` calls via the read lock (cheap, concurrent).
///   A rebuild takes the write lock to swap in a fresh state,
///   serializing the swap against ongoing inserts. Counters live here
///   so they're always consistent with the filter they describe.
///
/// Normal operation: both slots share the same `Arc<AtomicBloomFilter>`.
pub struct LocalBloom {
    read_bloom: ArcSwap<AtomicBloomFilter>,
    write_bloom: RwLock<BloomState>,
    num_bits: usize,
    num_hashes: u32,
    rebuild_threshold: f64,
}

impl LocalBloom {
    /// Create a new local bloom filter sized for the given false-positive rate.
    pub fn new(false_positive_rate: f64, rebuild_threshold: f64) -> Self {
        let (num_bits, num_hashes) = bloom_params(false_positive_rate, ESTIMATED_ENTRIES);

        tracing::info!(
            "Bloom filter: {} bits, {} hashes (fpr={}, estimated_entries={})",
            num_bits,
            num_hashes,
            false_positive_rate,
            ESTIMATED_ENTRIES,
        );

        let state = BloomState::new(num_bits, num_hashes);
        let read_bloom = ArcSwap::from(Arc::clone(&state.filter));
        Self {
            read_bloom,
            write_bloom: RwLock::new(state),
            num_bits,
            num_hashes,
            rebuild_threshold,
        }
    }

    /// Insert a store path hash into the bloom filter.
    ///
    /// Takes the `write_bloom` read lock (concurrent with other inserts,
    /// blocked only during the brief swap in `start_rebuild`).
    pub fn insert(&self, hash: &str) {
        let state = self.write_bloom.read().expect("write_bloom poisoned");
        state.filter.insert(hash);
        state.element_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a deletion (bloom filters cannot remove elements).
    ///
    /// Takes the `write_bloom` read lock so the deletion count stays
    /// consistent with the bloom state it belongs to.
    pub fn mark_deletion(&self) {
        let state = self.write_bloom.read().expect("write_bloom poisoned");
        state.deletion_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether the false positive rate has degraded enough to warrant a rebuild.
    pub fn needs_rebuild(&self) -> bool {
        let state = self.write_bloom.read().expect("write_bloom poisoned");
        let elements = state.element_count.load(Ordering::Relaxed);
        let deletions = state.deletion_count.load(Ordering::Relaxed);
        if elements == 0 {
            return false;
        }
        (deletions as f64 / elements as f64) > self.rebuild_threshold
    }

    /// Serialize the bloom filter for the `/bloom` HTTP endpoint.
    ///
    /// Uses the shared wire format from `bloom::wire`.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let header = super::wire::BloomHeader {
            num_hashes: self.num_hashes,
            num_bits: self.num_bits as u64,
        };
        let filter = self.read_bloom.load();
        Ok(super::wire::serialize(&header, filter.iter()))
    }

    /// Begin a rebuild: swap the write slot to a fresh empty state.
    ///
    /// Takes the write lock so no insert or deletion can land on the old
    /// state after this returns. The fresh state has zeroed counters.
    pub fn start_rebuild(&self) {
        let fresh = BloomState::new(self.num_bits, self.num_hashes);
        let mut write = self.write_bloom.write().expect("write_bloom poisoned");
        *write = fresh;
    }

    /// Finish a rebuild: promote the current write filter to the read slot.
    ///
    /// Counters are already correct — every `insert()` during the rebuild
    /// scan incremented the new state's `element_count`.
    pub fn finish_rebuild(&self) {
        let filter = {
            let state = self.write_bloom.read().expect("write_bloom poisoned");
            Arc::clone(&state.filter)
        };
        self.read_bloom.store(filter);
    }

    /// Number of bits in the bloom filter (for peer bloom size validation).
    pub fn num_bits(&self) -> usize {
        self.num_bits
    }

    /// Number of hash functions (for peer bloom deserialization).
    pub fn num_hashes(&self) -> u32 {
        self.num_hashes
    }

    pub fn element_count(&self) -> usize {
        let state = self.write_bloom.read().expect("write_bloom poisoned");
        state.element_count.load(Ordering::Relaxed)
    }

    pub fn deletion_count(&self) -> usize {
        let state = self.write_bloom.read().expect("write_bloom poisoned");
        state.deletion_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_roundtrip() {
        let bloom = LocalBloom::new(0.01, 0.1);
        bloom.insert("abc123def456ghi789jkl012mno345pq");
        bloom.insert("xyz789abc123def456ghi012jkl345mno");

        let data = bloom.serialize().unwrap();
        let (num_bits, num_hashes) = bloom_params(0.01, ESTIMATED_ENTRIES);

        // Deserialize via the wire module and validate header.
        let (header, words) = crate::bloom::wire::deserialize(&data).unwrap();
        assert_eq!(header.num_hashes, num_hashes);
        assert_eq!(header.num_bits, num_bits as u64);
        assert_eq!(words.len(), num_bits.div_ceil(64));

        // Reconstruct a non-atomic BloomFilter and verify membership.
        let filter = fastbloom::BloomFilter::from_vec(words)
            .seed(&BLOOM_SEED)
            .hashes(num_hashes);

        assert!(filter.contains("abc123def456ghi789jkl012mno345pq"));
        assert!(filter.contains("xyz789abc123def456ghi012jkl345mno"));
        assert!(!filter.contains("000000000000000000000000000000aa"));
    }

    #[test]
    fn bloom_params_1_percent() {
        let (bits, hashes) = bloom_params(0.01, 400_000);
        // Optimal: m ≈ 3.83M bits, k = 7
        assert!(bits > 3_800_000 && bits < 3_850_000);
        assert_eq!(hashes, 7);
    }

    #[test]
    fn bloom_params_point1_percent() {
        let (bits, hashes) = bloom_params(0.001, 400_000);
        // Optimal: m ≈ 5.75M bits, k = 10
        assert!(bits > 5_700_000 && bits < 5_800_000);
        assert_eq!(hashes, 10);
    }
}
