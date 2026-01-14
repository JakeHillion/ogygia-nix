//! Store scanner for indexing existing /nix/store paths
//!
//! Scans the local /nix/store directory and inserts each store path hash
//! into the local bloom filter. Only paths that are serveable (signed or
//! content-addressed) are indexed. After the initial scan, stays alive
//! to service rebuild requests from the store watcher.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use tokio::fs;
use tokio::sync::mpsc;
use tokio::time;

use crate::bloom::local::LocalBloom;
use crate::nix::store::PathInfo;

/// Store scanner for indexing existing paths
pub struct StoreScanner {
    bloom: Arc<LocalBloom>,
    rebuild_rx: mpsc::Receiver<()>,
}

impl StoreScanner {
    /// Create a new store scanner with a channel for rebuild requests.
    pub fn new(bloom: Arc<LocalBloom>, rebuild_rx: mpsc::Receiver<()>) -> Self {
        Self { bloom, rebuild_rx }
    }

    /// Loop waiting for rebuild requests from the store watcher.
    pub async fn run_rebuild_loop(mut self) -> Result<()> {
        while self.rebuild_rx.recv().await.is_some() {
            if !self.bloom.needs_rebuild() {
                tracing::debug!("Rebuild requested but threshold no longer exceeded, skipping");
                continue;
            }

            tracing::info!(
                "Starting bloom filter rebuild ({} deletions / {} elements)",
                self.bloom.deletion_count(),
                self.bloom.element_count(),
            );

            self.bloom.start_rebuild();
            self.scan("Rebuild scan").await?;
            self.bloom.finish_rebuild();

            tracing::info!(
                "Bloom filter rebuild complete: {} paths indexed",
                self.bloom.element_count(),
            );
        }

        Ok(())
    }

    /// Scan /nix/store and insert serveable path hashes into the bloom filter.
    ///
    /// Paths are queried in batches via `nix path-info --json` and only
    /// indexed if they are signed or content-addressed.
    pub async fn scan(&self, label: &str) -> Result<ScanStats> {
        let store_dir = Path::new("/nix/store");

        tracing::info!("{}: starting...", label);

        let total_paths = Arc::new(AtomicUsize::new(0));
        let indexed = Arc::new(AtomicUsize::new(0));
        let skipped = Arc::new(AtomicUsize::new(0));

        // Progress logger
        let progress_total = Arc::clone(&total_paths);
        let progress_indexed = Arc::clone(&indexed);
        let progress_skipped = Arc::clone(&skipped);
        let log_label = label.to_owned();
        let progress_handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(10));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                tracing::info!(
                    "{}: {} indexed / {} skipped / {} found",
                    log_label,
                    progress_indexed.load(Ordering::Relaxed),
                    progress_skipped.load(Ordering::Relaxed),
                    progress_total.load(Ordering::Relaxed),
                );
            }
        });

        let mut read_dir = fs::read_dir(store_dir).await?;
        let mut batch: Vec<PathBuf> = Vec::with_capacity(BATCH_SIZE);

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = match entry.file_name().to_str().map(String::from) {
                Some(n) => n,
                None => continue,
            };

            // Store path filenames are at least 33 chars (32 hash + '-' + name)
            if name.len() < 33 {
                continue;
            }

            // First character must be alphanumeric
            if !name
                .chars()
                .next()
                .map(|c| c.is_alphanumeric())
                .unwrap_or(false)
            {
                continue;
            }

            total_paths.fetch_add(1, Ordering::Relaxed);
            batch.push(store_dir.join(&name));

            if batch.len() >= BATCH_SIZE {
                let (n_indexed, n_skipped) = self.index_batch(&batch).await;
                indexed.fetch_add(n_indexed, Ordering::Relaxed);
                skipped.fetch_add(n_skipped, Ordering::Relaxed);
                batch.clear();
            }
        }

        // Flush remaining paths
        if !batch.is_empty() {
            let (n_indexed, n_skipped) = self.index_batch(&batch).await;
            indexed.fetch_add(n_indexed, Ordering::Relaxed);
            skipped.fetch_add(n_skipped, Ordering::Relaxed);
        }

        progress_handle.abort();

        let stats = ScanStats {
            total_paths: total_paths.load(Ordering::Relaxed),
            indexed: indexed.load(Ordering::Relaxed),
            skipped: skipped.load(Ordering::Relaxed),
        };

        tracing::info!(
            "{}: {} indexed, {} skipped (unsigned) out of {} paths",
            label,
            stats.indexed,
            stats.skipped,
            stats.total_paths,
        );

        Ok(stats)
    }

    /// Query a batch of paths and insert serveable ones into the bloom filter.
    ///
    /// Returns `(indexed, skipped)` counts.
    async fn index_batch(&self, paths: &[PathBuf]) -> (usize, usize) {
        let infos = match PathInfo::from_store_paths(paths).await {
            Ok(infos) => infos,
            Err(e) => {
                tracing::warn!("Failed to query path info for batch: {}", e);
                return (0, paths.len());
            }
        };

        let mut indexed = 0;
        let mut skipped = 0;

        for info in &infos {
            let hash = info
                .path
                .strip_prefix("/nix/store/")
                .and_then(|s| s.get(..32));

            let Some(hash) = hash else {
                skipped += 1;
                continue;
            };

            if info.is_serveable() {
                self.bloom.insert(hash);
                indexed += 1;
            } else {
                skipped += 1;
            }
        }

        // Paths that nix path-info didn't return info for
        skipped += paths.len() - infos.len();

        (indexed, skipped)
    }
}

/// Number of paths to query in a single `nix path-info` batch.
const BATCH_SIZE: usize = 500;

/// Statistics from a store scan
#[derive(Debug, Default)]
pub struct ScanStats {
    /// Total number of paths found
    pub total_paths: usize,
    /// Number of paths indexed into bloom filter
    pub indexed: usize,
    /// Number of paths skipped (unsigned and not content-addressed)
    pub skipped: usize,
}
