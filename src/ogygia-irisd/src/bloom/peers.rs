//! Peer bloom filters fetched lazily over HTTP
//!
//! Each peer exposes `GET /bloom` returning its serialized bloom filter.
//! `PeerBlooms` caches these with a configurable TTL and fetches them lazily
//! on first request. Expired blooms are discarded even if a refresh fails.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use fastbloom::BloomFilter;
use rand::seq::SliceRandom;
use tokio::sync::RwLock;
use tokio::sync::RwLockReadGuard;
use tokio::sync::mpsc;

use super::BLOOM_SEED;

#[derive(Default)]
struct PeerBloomEntry {
    bloom: Option<BloomFilter>,
    fetched_at: Option<Instant>,
}

impl PeerBloomEntry {
    fn is_fresh(&self, ttl: Duration) -> bool {
        match self.fetched_at {
            Some(t) => t.elapsed() < ttl,
            None => false,
        }
    }
}

/// Cache of peer bloom filters, fetched lazily and refreshed on TTL expiry.
pub struct PeerBlooms {
    peers: RwLock<HashMap<String, Arc<RwLock<PeerBloomEntry>>>>,
    ttl: Duration,
    expected_bloom_bits: usize,
    num_hashes: u32,
}

impl PeerBlooms {
    pub fn new(ttl: Duration, expected_bloom_bits: usize, num_hashes: u32) -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            ttl,
            expected_bloom_bits,
            num_hashes,
        }
    }

    /// Ensure all configured peers have fresh bloom filters.
    ///
    /// Missing or expired blooms are fetched in random order, in parallel.
    /// Fetch failures log a warning but are not fatal. Expired blooms are
    /// discarded even if the refresh fails.
    pub async fn ensure_fresh(&self, peer_urls: &[String], client: &reqwest::Client) {
        let mut entries = self.get_or_create_entries(peer_urls).await;
        entries.shuffle(&mut rand::rng());

        let ttl = self.ttl;
        let expected_bits = self.expected_bloom_bits;
        let num_hashes = self.num_hashes;
        let futs = entries.into_iter().map(|(url, entry)| {
            let client = client.clone();
            async move {
                let _guard =
                    ensure_peer_fresh(&entry, &url, &client, ttl, expected_bits, num_hashes).await;
            }
        });
        futures::future::join_all(futs).await;
    }

    /// Return peer URLs whose bloom filter contains `hash`.
    ///
    /// Uses `source_hash` + `contains_hash` so the hash string is only
    /// hashed once across all peer blooms.
    pub async fn lookup(&self, hash: &str) -> Vec<String> {
        let entries: Vec<_> = {
            let peers = self.peers.read().await;
            peers
                .iter()
                .map(|(url, entry)| (url.clone(), Arc::clone(entry)))
                .collect()
        };

        if entries.is_empty() {
            return Vec::new();
        }

        // All blooms use the same seed, so source_hash is identical.
        // Compute it from any bloom and reuse for the rest.
        let mut result = Vec::new();
        let mut precomputed: Option<u64> = None;
        for (url, entry) in &entries {
            let guard = entry.read().await;
            if let Some(ref bloom) = guard.bloom {
                let h = precomputed.get_or_insert_with(|| bloom.source_hash(hash));
                if bloom.contains_hash(*h) {
                    result.push(url.clone());
                }
            }
        }
        result
    }

    /// Stream peer URLs whose bloom filter matches `hash`.
    ///
    /// Each peer's bloom is refreshed if stale (same double-check pattern
    /// as `ensure_fresh`), and matching URLs are sent through the channel
    /// as soon as they're ready — without waiting for all peers.
    pub async fn lookup_stream(
        &self,
        peer_urls: &[String],
        hash: &str,
        client: &reqwest::Client,
    ) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let entries = self.get_or_create_entries(peer_urls).await;
        let ttl = self.ttl;
        let expected_bits = self.expected_bloom_bits;
        let num_hashes = self.num_hashes;

        for (url, entry) in entries {
            let tx = tx.clone();
            let client = client.clone();
            let hash = hash.to_string();
            tokio::spawn(async move {
                let guard =
                    ensure_peer_fresh(&entry, &url, &client, ttl, expected_bits, num_hashes).await;

                if guard.bloom.as_ref().is_some_and(|b| b.contains(&hash)) {
                    let _ = tx.send(url);
                }
            });
        }

        // Drop original sender so channel closes when all tasks complete.
        drop(tx);
        rx
    }

    /// Remove expired entries, freeing their memory.
    pub async fn evict_expired(&self) {
        let mut peers = self.peers.write().await;
        let ttl = self.ttl;
        peers.retain(|url, entry| match entry.try_read() {
            Ok(guard) => {
                let fresh = guard.is_fresh(ttl);
                if !fresh {
                    tracing::debug!("evicting expired bloom for {}", url);
                }
                fresh
            }
            // Currently being written (refresh in progress), keep it.
            Err(_) => true,
        });
    }

    /// Earliest time a cached bloom expires, or `None` if no entries exist.
    ///
    /// The returned `Instant` may be in the past if an entry has already
    /// expired. All entry locks are taken in parallel via `join_all`.
    pub async fn next_eviction_time(&self) -> Option<Instant> {
        let peers = self.peers.read().await;
        let ttl = self.ttl;

        let futs: Vec<_> = peers
            .values()
            .map(|entry| async move {
                let guard = entry.read().await;
                guard.fetched_at.map(|t| t + ttl)
            })
            .collect();

        futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .min()
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Get or create per-peer entries for all configured URLs.
    async fn get_or_create_entries(
        &self,
        peer_urls: &[String],
    ) -> Vec<(String, Arc<RwLock<PeerBloomEntry>>)> {
        let mut entries = Vec::with_capacity(peer_urls.len());
        let mut missing = Vec::new();

        {
            let peers = self.peers.read().await;
            for url in peer_urls {
                if let Some(entry) = peers.get(url) {
                    entries.push((url.clone(), Arc::clone(entry)));
                } else {
                    missing.push(url.clone());
                }
            }
        }

        if !missing.is_empty() {
            let mut peers = self.peers.write().await;
            for url in missing {
                let entry = peers
                    .entry(url.clone())
                    .or_insert_with(|| Arc::new(RwLock::new(PeerBloomEntry::default())));
                entries.push((url, Arc::clone(entry)));
            }
        }

        entries
    }
}

/// Fetch and deserialize a bloom filter from a peer.
async fn fetch_bloom(
    peer_url: &str,
    client: &reqwest::Client,
    expected_bloom_bits: usize,
    num_hashes: u32,
) -> Result<BloomFilter> {
    let url = format!("{}/bloom", peer_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to fetch bloom from {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("peer {} returned {}", url, response.status());
    }

    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("failed to read bloom body from {}", url))?;

    deserialize_bloom(&bytes, expected_bloom_bits, num_hashes)
        .with_context(|| format!("failed to deserialize bloom from {}", url))
}

/// Refresh a single peer's bloom filter if it is stale, returning
/// a read guard to the entry.
///
/// Checks freshness under a read lock first; if stale, acquires a write
/// lock with a double-check to avoid redundant fetches when multiple
/// tasks race to refresh the same peer. The write guard is downgraded
/// to a read guard before returning.
async fn ensure_peer_fresh<'a>(
    entry: &'a RwLock<PeerBloomEntry>,
    url: &str,
    client: &reqwest::Client,
    ttl: Duration,
    expected_bits: usize,
    num_hashes: u32,
) -> RwLockReadGuard<'a, PeerBloomEntry> {
    // Fast path: check freshness under read lock.
    {
        let guard = entry.read().await;
        if guard.is_fresh(ttl) {
            return guard;
        }
    }

    // Slow path: write lock with double-check.
    let mut guard = entry.write().await;
    if guard.is_fresh(ttl) {
        return guard.downgrade();
    }

    // Discard expired bloom before attempting refresh.
    guard.bloom = None;

    match fetch_bloom(url, client, expected_bits, num_hashes).await {
        Ok(bloom) => {
            guard.bloom = Some(bloom);
        }
        Err(e) => {
            tracing::warn!("failed to fetch bloom from {}: {}", url, e);
        }
    }

    // Record the attempt time regardless of success/failure
    // so we don't retry unreachable peers on every request.
    guard.fetched_at = Some(Instant::now());

    guard.downgrade()
}

/// Deserialize a bloom filter from the wire format produced by
/// `LocalBloom::serialize()`.
///
/// Format integrity (header hash, body alignment, body length) is
/// validated by `wire::deserialize`. This function validates bloom-
/// specific policy: do the declared parameters match our expectations?
fn deserialize_bloom(
    data: &[u8],
    expected_bloom_bits: usize,
    num_hashes: u32,
) -> Result<BloomFilter> {
    let (header, words) = super::wire::deserialize(data)?;

    anyhow::ensure!(
        header.num_hashes == num_hashes,
        "bloom num_hashes mismatch: peer has {} but we expect {}",
        header.num_hashes,
        num_hashes,
    );
    anyhow::ensure!(
        header.num_bits == expected_bloom_bits as u64,
        "bloom num_bits mismatch: peer has {} but we expect {}",
        header.num_bits,
        expected_bloom_bits,
    );

    Ok(BloomFilter::from_vec(words)
        .seed(&BLOOM_SEED)
        .hashes(header.num_hashes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloom::local::LocalBloom;

    #[tokio::test]
    async fn test_serialize_deserialize_roundtrip() {
        use bytes::Bytes;
        use futures::TryStreamExt;

        let local = LocalBloom::new(0.01, 0.1);
        local.insert("abc123def456ghi789jkl012mno345pq");
        local.insert("xyz789abc123def456ghi012jkl345mno");

        let chunks: Vec<Bytes> = local.serialize_stream().try_collect().await.unwrap();
        let data: Vec<u8> = chunks.iter().flat_map(|b| b.iter().copied()).collect();
        let bloom = deserialize_bloom(&data, local.num_bits(), local.num_hashes()).unwrap();

        assert!(bloom.contains("abc123def456ghi789jkl012mno345pq"));
        assert!(bloom.contains("xyz789abc123def456ghi012jkl345mno"));
        assert!(!bloom.contains("000000000000000000000000000000aa"));
    }
}
