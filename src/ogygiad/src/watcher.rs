//! Filesystem watcher for system state changes.
//!
//! Uses inotify to monitor NixOS system state paths for changes.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use inotify::{Inotify, WatchMask};
use ogygia_common::SYSTEM_STATE_DATA;
use tracing::{debug, info};

/// Watch descriptor type from inotify.
type WatchDescriptor = i32;

/// Filesystem watcher for system state paths.
pub struct StateWatcher {
    inotify: Inotify,
    watch_descriptors: HashMap<WatchDescriptor, String>,
}

impl StateWatcher {
    /// Creates a new state watcher and sets up inotify watches.
    ///
    /// Watches all existing system state paths for:
    /// - CREATE, DELETE, MOVED_TO, MOVED_FROM (symlink changes)
    /// - ATTRIB (attribute changes on symlinks)
    pub fn new() -> Result<Self> {
        let mut inotify = Inotify::init().context("failed to initialize inotify")?;
        let mut watch_descriptors = HashMap::new();

        // Watch mask matching Python implementation
        let watch_mask = WatchMask::CREATE
            | WatchMask::DELETE
            | WatchMask::MOVED_TO
            | WatchMask::MOVED_FROM
            | WatchMask::ATTRIB
            | WatchMask::DONT_FOLLOW;

        for state in &SYSTEM_STATE_DATA {
            let path = Path::new(state.base_path);
            if path.exists() {
                let wd = inotify
                    .watches()
                    .add(path, watch_mask)
                    .with_context(|| format!("failed to watch {}", state.base_path))?;

                watch_descriptors.insert(wd.get_watch_descriptor_id(), state.base_path.to_string());
                info!("Watching {}", state.base_path);
            } else {
                debug!("Skipping non-existent path: {}", state.base_path);
            }
        }

        if watch_descriptors.is_empty() {
            anyhow::bail!("No system state paths exist to watch");
        }

        Ok(Self {
            inotify,
            watch_descriptors,
        })
    }

    /// Runs the watcher event loop, sending events through the provided channel.
    ///
    /// This is a blocking function that should be run in a dedicated thread.
    pub fn run_blocking(mut self, tx: tokio::sync::mpsc::UnboundedSender<(String, u32)>) {
        let mut buffer = [0u8; 4096];
        loop {
            match self.inotify.read_events_blocking(&mut buffer) {
                Ok(events) => {
                    for event in events {
                        let wd = event.wd.get_watch_descriptor_id();
                        if let Some(path) = self.watch_descriptors.get(&wd) {
                            let mask = event.mask.bits();
                            if tx.send((path.clone(), mask)).is_err() {
                                // Channel closed, exit thread
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Inotify error: {:?}", e);
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    }
}
