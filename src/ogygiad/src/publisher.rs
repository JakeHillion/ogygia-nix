//! ZooKeeper publisher for system state information.
//!
//! This module handles publishing NixOS build revisions to ZooKeeper,
//! including connection management, node creation, and update caching.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use ogygia_common::config::ZookeeperConfig;
use ogygia_common::{SYSTEM_STATE_DATA, join_zk_path};
use tracing::{debug, info};
use zookeeper::{CreateMode, WatchedEvent, Watcher, ZkError, ZooKeeper};

use crate::state::StateValues;

/// No-op watcher for ZooKeeper connections.
///
/// We don't need to react to ZooKeeper events, so this watcher does nothing.
struct NoopWatcher;

impl Watcher for NoopWatcher {
    fn handle(&self, _: WatchedEvent) {}
}

/// ZooKeeper publisher that maintains connection and caches written values.
pub struct Publisher {
    zk: ZooKeeper,
    namespace: String,
    hostname: String,
    cached_versions: HashMap<String, String>,
}

impl Publisher {
    /// Creates a new publisher and establishes ZooKeeper connection.
    ///
    /// # Arguments
    ///
    /// * `config` - ZooKeeper connection configuration
    /// * `hostname` - Fully qualified hostname for this node
    pub fn new(config: &ZookeeperConfig, hostname: String) -> Result<Self> {
        let connection_string = config.endpoints.join(",");

        let zk = ZooKeeper::connect(&connection_string, config.timeout, NoopWatcher)
            .with_context(|| {
                format!(
                    "failed to connect to ZooKeeper at {}. \
                     Check that the endpoints are reachable and the ZooKeeper service is running. \
                     Connection timeout: {:?}",
                    connection_string, config.timeout
                )
            })?;

        info!("Connected to ZooKeeper: {}", connection_string);

        let mut publisher = Self {
            zk,
            namespace: config.namespace.clone(),
            hostname: hostname.clone(),
            cached_versions: HashMap::new(),
        };

        // Ensure hostname directory exists
        publisher.ensure_hostname_directory()?;

        // Load existing versions into cache
        publisher.load_existing_versions()?;

        Ok(publisher)
    }

    /// Ensures the hostname directory exists in ZooKeeper.
    fn ensure_hostname_directory(&self) -> Result<()> {
        let hostname_path = join_zk_path(&self.namespace, &self.hostname);

        match self.zk.create(
            &hostname_path,
            Vec::new(),
            zookeeper::Acl::open_unsafe().clone(),
            CreateMode::Persistent,
        ) {
            Ok(_) => {
                info!("Created hostname directory: {}", hostname_path);
                Ok(())
            }
            Err(ZkError::NodeExists) => {
                debug!("Hostname directory already exists: {}", hostname_path);
                Ok(())
            }
            Err(ZkError::NoNode) => {
                // Parent path doesn't exist, create with makepath equivalent
                self.create_path_recursive(&hostname_path)
            }
            Err(e) => Err(e).with_context(|| format!("failed to create {}", hostname_path)),
        }
    }

    /// Creates a ZooKeeper path recursively (equivalent to makepath=True).
    fn create_path_recursive(&self, path: &str) -> Result<()> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let mut current = String::new();

        for part in parts {
            current.push('/');
            current.push_str(part);

            match self.zk.create(
                &current,
                Vec::new(),
                zookeeper::Acl::open_unsafe().clone(),
                CreateMode::Persistent,
            ) {
                Ok(_) | Err(ZkError::NodeExists) => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("failed to create path {}", current))
                }
            }
        }

        Ok(())
    }

    /// Loads existing versions from ZooKeeper into the cache.
    fn load_existing_versions(&mut self) -> Result<()> {
        let hostname_path = join_zk_path(&self.namespace, &self.hostname);

        let existing_states = match self.zk.get_children(&hostname_path, false) {
            Ok(children) => children,
            Err(ZkError::NoNode) => {
                debug!("No existing state nodes for {}", hostname_path);
                return Ok(());
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to list children of {}", hostname_path)
                })
            }
        };

        for state in existing_states {
            let znode_path = join_zk_path(&hostname_path, &state);
            match self.zk.get_data(&znode_path, false) {
                Ok((data, _)) => {
                    if let Ok(version) = String::from_utf8(data) {
                        self.cached_versions.insert(state.clone(), version.clone());
                        debug!("Cached existing {} = {}", state, version);
                    }
                }
                Err(e) => {
                    debug!("Could not read {}: {:?}", znode_path, e);
                }
            }
        }

        Ok(())
    }

    /// Updates or creates a ZooKeeper node with version info if changed.
    ///
    /// Returns `true` if an update was performed, `false` if cached value matched.
    fn update_zookeeper_node(&mut self, state: &str, version: &str) -> Result<bool> {
        // Check if we already have this version cached
        if self.cached_versions.get(state).map(|s| s.as_str()) == Some(version) {
            return Ok(false); // No change needed
        }

        let hostname_path = join_zk_path(&self.namespace, &self.hostname);
        let znode_path = join_zk_path(&hostname_path, state);

        info!("Updating {} = {}", znode_path, version);

        // Try to set existing node first (most common case)
        match self.zk.set_data(&znode_path, version.as_bytes().to_vec(), None) {
            Ok(_) => {}
            Err(ZkError::NoNode) => {
                // Node doesn't exist, create it
                self.zk
                    .create(
                        &znode_path,
                        version.as_bytes().to_vec(),
                        zookeeper::Acl::open_unsafe().clone(),
                        CreateMode::Persistent,
                    )
                    .with_context(|| format!("failed to create {}", znode_path))?;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to set data for {}", znode_path))
            }
        }

        // Cache the version we just wrote
        self.cached_versions
            .insert(state.to_string(), version.to_string());

        Ok(true) // Update was performed
    }

    /// Publishes all current system state versions to ZooKeeper.
    ///
    /// Only updates znodes whose values have changed since last publish.
    /// Returns the number of updates made.
    pub fn publish_all(&mut self, revisions: &StateValues) -> Result<usize> {
        let mut updates_made = 0;

        for (idx, state_data) in SYSTEM_STATE_DATA.iter().enumerate() {
            if let Some(ref version) = revisions[idx] {
                if self.update_zookeeper_node(state_data.znode_name, version)? {
                    updates_made += 1;
                }
            }
        }

        if updates_made > 0 {
            info!("Made {} ZooKeeper updates", updates_made);
        } else {
            debug!("No ZooKeeper updates needed");
        }

        Ok(updates_made)
    }

    /// Checks if the ZooKeeper connection is still valid.
    ///
    /// Note: The zookeeper crate doesn't expose a reliable way to check connection state,
    /// so we'll rely on error handling during operations instead.
    pub fn is_connected(&self) -> bool {
        // Assume connected unless we get an error
        true
    }

    /// Returns the connection timeout for reconnection attempts.
    pub fn connection_timeout() -> Duration {
        Duration::from_secs(10)
    }
}
