//! Peer bloom filters fetched lazily over HTTP
//!
//! Each peer exposes `GET /bloom` returning its serialized bloom filter.
//! `PeerBlooms` caches these with a configurable TTL and fetches them lazily
//! on first request. Expired blooms are discarded even if a refresh fails.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use fastbloom::BloomFilter;
use rand::seq::SliceRandom;
use tokio::sync::RwLock;
use tokio::sync::RwLockReadGuard;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::BLOOM_SEED;

enum Freshness {
    Fresh,
    Stale,
    Expired,
}

#[derive(Default)]
struct PeerBloomEntry {
    bloom: Option<BloomFilter>,
    fetched_at: Option<Instant>,      // last *successful* fetch
    last_attempt_at: Option<Instant>, // last fetch attempt (success or failure)
}

impl PeerBloomEntry {
    fn recently_attempted(&self, min_interval: Duration) -> bool {
        self.last_attempt_at
            .map(|t| t.elapsed() < min_interval)
            .unwrap_or(false)
    }

    fn freshness(&self, ttl: Duration, max_age: Duration) -> Freshness {
        match self.fetched_at {
            None => Freshness::Expired,
            Some(t) => {
                let elapsed = t.elapsed();
                if elapsed < ttl {
                    Freshness::Fresh
                } else if elapsed < max_age {
                    Freshness::Stale
                } else {
                    Freshness::Expired
                }
            }
        }
    }
}

/// Cache of peer bloom filters, fetched lazily and refreshed on TTL expiry.
pub struct PeerBlooms {
    peers: RwLock<HashMap<String, Arc<RwLock<PeerBloomEntry>>>>,
    ttl: Duration,
    max_age: Duration,
    expected_bloom_bits: usize,
    num_hashes: u32,
}

impl PeerBlooms {
    pub fn new(
        ttl: Duration,
        max_age: Duration,
        expected_bloom_bits: usize,
        num_hashes: u32,
    ) -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            ttl,
            max_age,
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
        let max_age = self.max_age;
        let expected_bits = self.expected_bloom_bits;
        let num_hashes = self.num_hashes;
        let futs = entries.into_iter().map(|(url, entry)| {
            let client = client.clone();
            async move {
                let _guard = ensure_peer_fresh(
                    &entry,
                    &url,
                    &client,
                    ttl,
                    max_age,
                    expected_bits,
                    num_hashes,
                )
                .await;
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
        let max_age = self.max_age;
        let expected_bits = self.expected_bloom_bits;
        let num_hashes = self.num_hashes;

        for (url, entry) in entries {
            let tx = tx.clone();
            let client = client.clone();
            let hash = hash.to_string();
            tokio::spawn(async move {
                let guard = ensure_peer_fresh(
                    &entry,
                    &url,
                    &client,
                    ttl,
                    max_age,
                    expected_bits,
                    num_hashes,
                )
                .await;

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
        let max_age = self.max_age;
        let ttl = self.ttl;
        peers.retain(|url, entry| match entry.try_read() {
            Ok(guard) => {
                let within_max_age = match guard.fetched_at {
                    Some(t) => t.elapsed() < max_age,
                    None => false,
                };
                let recently_attempted = guard.recently_attempted(ttl);
                if !within_max_age && !recently_attempted {
                    tracing::debug!("evicting expired bloom for {}", url);
                }
                within_max_age || recently_attempted
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
        let max_age = self.max_age;
        let ttl = self.ttl;

        let futs: Vec<_> = peers
            .values()
            .map(|entry| async move {
                let guard = entry.read().await;
                let fetched_deadline = guard.fetched_at.map(|t| t + max_age);
                let attempt_deadline = guard.last_attempt_at.map(|t| t + ttl);
                match (fetched_deadline, attempt_deadline) {
                    (Some(f), Some(a)) => Some(f.max(a)),
                    (Some(f), None) => Some(f),
                    (None, Some(a)) => Some(a),
                    (None, None) => None,
                }
            })
            .collect();

        futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .min()
    }

    pub fn max_age(&self) -> Duration {
        self.max_age
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
/// Uses a three-state freshness model:
/// - **Fresh**: return read guard immediately (fast path).
/// - **Stale** (between TTL and max_age): spawn a background refresh
///   and return the stale data immediately (stale-while-revalidate).
/// - **Expired** (beyond max_age): block on a fetch, discarding stale data
///   first (same as the original two-state behavior).
async fn ensure_peer_fresh<'a>(
    entry: &'a Arc<RwLock<PeerBloomEntry>>,
    url: &str,
    client: &reqwest::Client,
    ttl: Duration,
    max_age: Duration,
    expected_bits: usize,
    num_hashes: u32,
) -> RwLockReadGuard<'a, PeerBloomEntry> {
    // Fast path: check freshness under read lock.
    {
        let guard = entry.read().await;
        match guard.freshness(ttl, max_age) {
            Freshness::Fresh => return guard,
            Freshness::Stale if guard.recently_attempted(ttl) => return guard,
            Freshness::Stale | Freshness::Expired => {}
        }
    }

    // Slow path: write lock with double-check.
    let mut guard = entry.write().await;
    match guard.freshness(ttl, max_age) {
        Freshness::Fresh => return guard.downgrade(),
        Freshness::Stale if guard.recently_attempted(ttl) => return guard.downgrade(),
        Freshness::Stale => {
            // Stale-while-revalidate: serve stale data, spawn background refresh.
            guard.last_attempt_at = Some(Instant::now());
            let read_guard = guard.downgrade();

            let entry_clone = Arc::clone(entry);
            let client = client.clone();
            let url = url.to_string();
            tokio::spawn(async move {
                let result = fetch_bloom(&url, &client, expected_bits, num_hashes).await;
                let mut guard = entry_clone.write().await;
                match result {
                    Ok(bloom) => {
                        guard.bloom = Some(bloom);
                        guard.fetched_at = Some(Instant::now());
                    }
                    Err(e) => {
                        tracing::warn!("background bloom fetch failed for {}: {}", url, e);
                    }
                }
            });

            return read_guard;
        }
        Freshness::Expired if guard.recently_attempted(ttl) => {
            guard.bloom = None;
            return guard.downgrade();
        }
        Freshness::Expired => {}
    }

    // Expired path: discard stale data, block on fetch.
    guard.last_attempt_at = Some(Instant::now());
    guard.bloom = None;

    match fetch_bloom(url, client, expected_bits, num_hashes).await {
        Ok(bloom) => {
            guard.bloom = Some(bloom);
            guard.fetched_at = Some(Instant::now());
        }
        Err(e) => {
            tracing::warn!("failed to fetch bloom from {}: {}", url, e);
        }
    }

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

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let local = LocalBloom::new(0.01, 0.1);
        local.insert("abc123def456ghi789jkl012mno345pq");
        local.insert("xyz789abc123def456ghi012jkl345mno");
        local.finish_rebuild();

        let data = local.serialize().unwrap().unwrap();
        let bloom = deserialize_bloom(&data, local.num_bits(), local.num_hashes()).unwrap();

        assert!(bloom.contains("abc123def456ghi789jkl012mno345pq"));
        assert!(bloom.contains("xyz789abc123def456ghi012jkl345mno"));
        assert!(!bloom.contains("000000000000000000000000000000aa"));
    }

    #[tokio::test(start_paused = true)]
    async fn test_freshness_fresh() {
        let entry = PeerBloomEntry {
            bloom: None,
            fetched_at: Some(Instant::now()),
            last_attempt_at: None,
        };
        let ttl = Duration::from_secs(5);
        let max_age = Duration::from_secs(10);
        assert!(matches!(entry.freshness(ttl, max_age), Freshness::Fresh));
    }

    #[tokio::test(start_paused = true)]
    async fn test_freshness_stale() {
        let entry = PeerBloomEntry {
            bloom: None,
            fetched_at: Some(Instant::now()),
            last_attempt_at: None,
        };
        let ttl = Duration::from_secs(5);
        let max_age = Duration::from_secs(10);
        tokio::time::advance(Duration::from_secs(7)).await;
        assert!(matches!(entry.freshness(ttl, max_age), Freshness::Stale));
    }

    #[tokio::test(start_paused = true)]
    async fn test_freshness_expired() {
        let entry = PeerBloomEntry {
            bloom: None,
            fetched_at: Some(Instant::now()),
            last_attempt_at: None,
        };
        let ttl = Duration::from_secs(5);
        let max_age = Duration::from_secs(10);
        tokio::time::advance(Duration::from_secs(15)).await;
        assert!(matches!(entry.freshness(ttl, max_age), Freshness::Expired));
    }

    #[test]
    fn test_freshness_no_fetched_at() {
        let entry = PeerBloomEntry {
            bloom: None,
            fetched_at: None,
            last_attempt_at: None,
        };
        let ttl = Duration::from_secs(5);
        let max_age = Duration::from_secs(10);
        assert!(matches!(entry.freshness(ttl, max_age), Freshness::Expired));
    }

    #[tokio::test(start_paused = true)]
    async fn test_evict_keeps_stale_removes_expired() {
        let peer_blooms = PeerBlooms::new(Duration::from_secs(5), Duration::from_secs(10), 1024, 2);

        // Insert entry A at time 0 (will become expired after 15s)
        {
            let mut peers = peer_blooms.peers.write().await;
            let entry = PeerBloomEntry {
                fetched_at: Some(Instant::now()),
                ..Default::default()
            };
            peers.insert(
                "http://expired-peer:35742".to_string(),
                Arc::new(RwLock::new(entry)),
            );
        }

        // Advance 7s, then insert entry B (will be stale at 15s total)
        tokio::time::advance(Duration::from_secs(7)).await;
        {
            let mut peers = peer_blooms.peers.write().await;
            let entry = PeerBloomEntry {
                fetched_at: Some(Instant::now()),
                ..Default::default()
            };
            peers.insert(
                "http://stale-peer:35742".to_string(),
                Arc::new(RwLock::new(entry)),
            );
        }

        // Advance 8 more seconds.
        // Entry A: elapsed = 15s > max_age=10s → Expired
        // Entry B: elapsed = 8s, > ttl=5s but < max_age=10s → Stale
        tokio::time::advance(Duration::from_secs(8)).await;

        peer_blooms.evict_expired().await;

        let peers = peer_blooms.peers.read().await;
        assert!(
            !peers.contains_key("http://expired-peer:35742"),
            "expired entry should have been evicted"
        );
        assert!(
            peers.contains_key("http://stale-peer:35742"),
            "stale entry should have been kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_next_eviction_time_uses_max_age() {
        let peer_blooms = PeerBlooms::new(Duration::from_secs(5), Duration::from_secs(10), 1024, 2);
        {
            let mut peers = peer_blooms.peers.write().await;
            let entry = PeerBloomEntry {
                fetched_at: Some(Instant::now()),
                ..Default::default()
            };
            peers.insert(
                "http://test-peer:35742".to_string(),
                Arc::new(RwLock::new(entry)),
            );
        }

        let eviction_time = peer_blooms.next_eviction_time().await;
        // fetched_at + max_age, not fetched_at + ttl
        assert_eq!(
            eviction_time,
            Some(Instant::now() + Duration::from_secs(10))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_freshness_no_stale_window_when_max_age_equals_ttl() {
        let entry = PeerBloomEntry {
            bloom: None,
            fetched_at: Some(Instant::now()),
            last_attempt_at: None,
        };
        let ttl = Duration::from_secs(5);
        let max_age = Duration::from_secs(5); // same as ttl — no stale window
        tokio::time::advance(Duration::from_secs(6)).await;
        // Should be Expired directly — no Stale state possible
        assert!(matches!(entry.freshness(ttl, max_age), Freshness::Expired));
    }
}
