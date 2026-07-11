//! Store watcher for monitoring /nix/store for new and removed paths
//!
//! Uses inotify (via the `notify` crate) to watch for store path changes
//! and updates the local bloom filter accordingly.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use notify::Config;
use notify::Event;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use ogygia_nixutils::NixDb;
use ogygia_nixutils::StoreHash;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bloom::local::LocalBloom;

/// Store watcher that monitors /nix/store for path changes
pub struct StoreWatcher {
    bloom: Arc<LocalBloom>,
    nix_db: NixDb,
    rebuild_tx: mpsc::Sender<()>,
}

impl StoreWatcher {
    /// Create a new store watcher with a channel for requesting rebuilds.
    pub fn new(bloom: Arc<LocalBloom>, nix_db: NixDb, rebuild_tx: mpsc::Sender<()>) -> Self {
        Self {
            bloom,
            nix_db,
            rebuild_tx,
        }
    }

    /// Start watching /nix/store for new and removed paths
    pub async fn start(self: Arc<Self>, token: CancellationToken) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<Event>(100);

        // Create the watcher in a blocking task since notify uses sync APIs
        let watcher_handle = tokio::task::spawn_blocking(move || -> Result<RecommendedWatcher> {
            let mut watcher = RecommendedWatcher::new(
                move |result: Result<Event, notify::Error>| {
                    if let Ok(event) = result {
                        match event.kind {
                            EventKind::Create(_) | EventKind::Remove(_) => {
                                let _ = tx.blocking_send(event);
                            }
                            _ => {}
                        }
                    }
                },
                Config::default(),
            )?;

            // Watch /nix/store non-recursively (we only care about top-level paths)
            watcher.watch(Path::new("/nix/store"), RecursiveMode::NonRecursive)?;

            Ok(watcher)
        });

        // Keep the watcher alive
        let _watcher = watcher_handle.await??;

        tracing::info!("Started watching /nix/store for path changes");

        // Process events until cancelled
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!("Store watcher shutting down");
                    return Ok(());
                }
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    for path in event.paths {
                        if let Some(store_path) = path.to_str()
                            && store_path.starts_with("/nix/store/")
                            && store_path.len() > 44
                        {
                            match event.kind {
                                EventKind::Create(_) => {
                                    self.process_create(store_path).await;
                                }
                                EventKind::Remove(_) => self.process_remove(store_path),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Process a newly created store path.
    ///
    /// Only indexes the path if it is serveable (signed or content-addressed).
    async fn process_create(&self, store_path: &str) {
        let Ok(hash) = StoreHash::from_store_path(store_path) else {
            return;
        };

        let serveable = match self.nix_db.is_path_serveable(store_path).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Failed to query path info for {}: {}", store_path, e);
                return;
            }
        };

        if serveable {
            self.bloom.insert(hash.as_str());
            tracing::debug!("Indexed new store path: {}", store_path);
        } else {
            tracing::debug!(
                "Skipping not-serveable store path (neither signed nor content-addressed): {}",
                store_path
            );
        }
    }

    /// Process a removed store path
    fn process_remove(&self, store_path: &str) {
        self.bloom.mark_deletion();
        tracing::debug!("Noted store path removal: {}", store_path);

        if self.bloom.needs_rebuild() {
            tracing::info!(
                "Bloom filter deletion threshold exceeded ({} deletions / {} elements), requesting rebuild",
                self.bloom.deletion_count(),
                self.bloom.element_count(),
            );
            let _ = self.rebuild_tx.try_send(());
        }
    }
}
