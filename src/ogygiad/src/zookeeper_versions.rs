use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use zookeeper_client::{Client, CreateMode, CreateOptions, SetDataOptions};

/// Mapping of system paths to their state names
const STATE_MAPPING: &[(&str, &str)] = &[
    ("/run/current-system", "current"),
    ("/run/booted-system", "booted"),
    ("/nix/var/nix/profiles/system", "nextboot"),
];

/// ZooKeeper version uploader
pub struct ZooKeeperVersions {
    zk_client: Client,
    hostname: String,
    cached_versions: Arc<Mutex<HashMap<String, String>>>,
}

impl ZooKeeperVersions {
    /// Create a new ZooKeeper version uploader
    pub async fn new(zk_addresses: &str, hostname: String) -> Result<Self> {
        info!("Connecting to ZooKeeper at: {}", zk_addresses);

        let zk_client = Client::connect(zk_addresses)
            .await
            .context("Failed to connect to ZooKeeper")?;

        info!("Connected to ZooKeeper");

        Ok(Self {
            zk_client,
            hostname,
            cached_versions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Read version from a build-revision file
    fn read_version_file(path: &Path) -> Option<String> {
        let version_file = path
            .join("sw")
            .join("share")
            .join("ogygia")
            .join("build-revision");

        match std::fs::read_to_string(&version_file) {
            Ok(content) => {
                let version = content.trim().to_string();
                debug!("Read version from {:?}: {}", version_file, version);
                Some(version)
            }
            Err(e) => {
                debug!("Could not read {:?}: {}", version_file, e);
                None
            }
        }
    }

    /// Update a ZooKeeper node with version info if changed
    async fn update_zookeeper_node(&self, state: &str, version: &str) -> Result<bool> {
        let mut cached = self.cached_versions.lock().await;

        // Check if version has changed
        if cached.get(state) == Some(&version.to_string()) {
            debug!("Version unchanged for {}: {}", state, version);
            return Ok(false);
        }

        let znode_path = format!("/nixos/versions/{}/{}", self.hostname, state);
        info!("Updating {} = {}", znode_path, version);

        let version_bytes = version.as_bytes().to_vec();

        // Try to set existing node first (most common case)
        match self
            .zk_client
            .set_data(&znode_path, SetDataOptions::default().with_data(version_bytes.clone()))
            .await
        {
            Ok(_) => {
                // Successfully updated existing node
                cached.insert(state.to_string(), version.to_string());
                Ok(true)
            }
            Err(e) => {
                // Node might not exist, try creating it
                debug!("Failed to set {}: {}. Trying to create it.", znode_path, e);

                match self
                    .zk_client
                    .create(
                        &znode_path,
                        CreateOptions::default()
                            .with_data(version_bytes)
                            .with_create_mode(CreateMode::Persistent),
                    )
                    .await
                {
                    Ok(_) => {
                        info!("Created new node: {}", znode_path);
                        cached.insert(state.to_string(), version.to_string());
                        Ok(true)
                    }
                    Err(e) => {
                        warn!("Failed to create {}: {}", znode_path, e);
                        Err(e.into())
                    }
                }
            }
        }
    }

    /// Load existing versions from ZooKeeper into cache
    async fn load_existing_versions(&self) -> Result<()> {
        let hostname_path = format!("/nixos/versions/{}", self.hostname);

        match self.zk_client.get_children(&hostname_path).await {
            Ok(children) => {
                let mut cached = self.cached_versions.lock().await;
                for state in children {
                    let znode_path = format!("{}/{}", hostname_path, state);
                    match self.zk_client.get_data(&znode_path).await {
                        Ok(data) => {
                            if let Ok(version) = String::from_utf8(data.0) {
                                debug!("Cached existing {} = {}", state, version);
                                cached.insert(state, version);
                            }
                        }
                        Err(e) => {
                            debug!("Could not read {}: {}", znode_path, e);
                        }
                    }
                }
            }
            Err(e) => {
                debug!("Could not list states for {}: {}", hostname_path, e);
            }
        }

        Ok(())
    }

    /// Update all version information in ZooKeeper if changed
    async fn update_all_versions(&self) -> Result<()> {
        let mut updates_made = 0;

        for (path, state) in STATE_MAPPING {
            if let Some(version) = Self::read_version_file(Path::new(path)) {
                match self.update_zookeeper_node(state, &version).await {
                    Ok(true) => updates_made += 1,
                    Ok(false) => {}
                    Err(e) => {
                        warn!("Failed to update {} for {}: {}", state, path, e);
                    }
                }
            }
        }

        if updates_made > 0 {
            info!("Made {} ZooKeeper updates", updates_made);
        } else {
            debug!("No ZooKeeper updates needed");
        }

        Ok(())
    }

    /// Ensure the hostname directory exists in ZooKeeper
    async fn ensure_hostname_directory(&self) -> Result<()> {
        let hostname_path = format!("/nixos/versions/{}", self.hostname);

        // Try to create the entire path
        match self
            .zk_client
            .create(
                &hostname_path,
                CreateOptions::default()
                    .with_data(vec![])
                    .with_create_mode(CreateMode::Persistent)
                    .create_parents(),
            )
            .await
        {
            Ok(_) => {
                info!("Created hostname directory: {}", hostname_path);
                Ok(())
            }
            Err(e) => {
                // NodeExists is fine
                debug!("Hostname directory already exists or creation failed: {}", e);
                Ok(())
            }
        }
    }

    /// Run the version uploader
    pub async fn run(self) -> Result<()> {
        // Ensure hostname directory exists
        self.ensure_hostname_directory().await?;

        // Load existing versions into cache
        self.load_existing_versions().await?;

        // Set up file watching
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);

        let watcher_result: Result<RecommendedWatcher> = {
            let tx = tx.clone();
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                match res {
                    Ok(event) => {
                        if let Err(e) = tx.blocking_send(event) {
                            warn!("Failed to send event: {}", e);
                        }
                    }
                    Err(e) => warn!("Watch error: {}", e),
                }
            })
            .context("Failed to create file watcher")
        };

        let mut watcher = watcher_result?;

        // Watch all state paths
        for (path, _) in STATE_MAPPING {
            let path = Path::new(path);
            if path.exists() {
                watcher
                    .watch(path, RecursiveMode::NonRecursive)
                    .with_context(|| format!("Failed to watch {:?}", path))?;
                info!("Watching {:?}", path);
            } else {
                warn!("Path does not exist, skipping watch: {:?}", path);
            }
        }

        // Initial update after inotify setup
        self.update_all_versions().await?;

        info!("Starting file watch for system changes");

        // Watch for changes
        while let Some(event) = rx.recv().await {
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                    info!("File system change detected: {:?}", event);

                    // Small delay to allow filesystem operations to complete
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    if let Err(e) = self.update_all_versions().await {
                        warn!("Failed to update versions: {}", e);
                    }
                }
                _ => {
                    debug!("Ignoring event: {:?}", event);
                }
            }
        }

        Ok(())
    }
}
