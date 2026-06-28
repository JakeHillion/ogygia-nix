//! Store scanner for indexing /nix/store paths into the bloom filter.
//!
//! Queries the Nix SQLite database directly for all serveable (signed or
//! content-addressed) store paths and inserts their hashes into the bloom
//! filter. After the initial scan, stays alive to service rebuild requests
//! from the store watcher.

use std::sync::Arc;

use anyhow::Result;
use ogygia_nixutils::NixDb;
use tokio::sync::mpsc;

use crate::bloom::local::LocalBloom;

/// Store scanner for indexing existing paths
pub struct StoreScanner {
    bloom: Arc<LocalBloom>,
    nix_db: NixDb,
    rebuild_rx: mpsc::Receiver<()>,
}

impl StoreScanner {
    /// Create a new store scanner with a channel for rebuild requests.
    pub fn new(bloom: Arc<LocalBloom>, nix_db: NixDb, rebuild_rx: mpsc::Receiver<()>) -> Self {
        Self {
            bloom,
            nix_db,
            rebuild_rx,
        }
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

    /// Query the Nix database for all serveable paths and insert their hashes
    /// into the bloom filter.
    pub async fn scan(&self, label: &str) -> Result<ScanStats> {
        tracing::info!("{}: starting...", label);

        let hashes = self.nix_db.serveable_hashes().await?;

        let indexed = hashes.len();
        for hash in &hashes {
            self.bloom.insert(hash);
        }

        tracing::info!("{}: {} paths indexed", label, indexed);

        Ok(ScanStats { indexed })
    }
}

/// Statistics from a store scan
#[derive(Debug, Default)]
pub struct ScanStats {
    /// Number of paths indexed into bloom filter
    pub indexed: usize,
}
