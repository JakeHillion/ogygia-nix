//! Ogygia daemon - NixOS system state publisher to ZooKeeper.
//!
//! This daemon watches NixOS system state paths for changes and publishes
//! build revision information to ZooKeeper for fleet-wide visibility.

mod publisher;
mod state;
mod watcher;

use std::env;
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result};
use hostname::get as get_hostname;
use ogygia_common::HOSTNAME_OVERRIDE_ENV;
use tokio::time::sleep;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::publisher::Publisher;
use crate::state::collect_all_revisions;
use crate::watcher::StateWatcher;

/// Detects the current hostname using multiple fallback strategies.
///
/// Tries the following methods in order:
/// 1. `OGYGIA_HOSTNAME` environment variable (user override)
/// 2. `hostname -f` command (fully qualified)
/// 3. `HOSTNAME` environment variable
/// 4. `hostname` command (short name)
/// 5. `gethostname()` syscall
/// 6. Fallback to "unknown-host" if all else fails
fn detect_hostname() -> String {
    hostname_from_env(HOSTNAME_OVERRIDE_ENV)
        .or_else(|| hostname_from_command("hostname", &["-f"]))
        .or_else(|| hostname_from_env("HOSTNAME"))
        .or_else(|| hostname_from_command("hostname", &[]))
        .or_else(hostname_from_syscall)
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// Attempts to read hostname from an environment variable.
fn hostname_from_env(var: &str) -> Option<String> {
    env::var(var)
        .ok()
        .and_then(|value| normalize_hostname(&value))
}

/// Attempts to get hostname by running a command.
fn hostname_from_command(program: &str, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    normalize_hostname(value.as_ref())
}

/// Attempts to get hostname via the `gethostname()` syscall.
fn hostname_from_syscall() -> Option<String> {
    let os_str = get_hostname().ok()?;
    let owned = os_str.into_string().ok()?;
    normalize_hostname(&owned)
}

/// Normalizes a hostname string by trimming whitespace.
fn normalize_hostname(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing with environment filter
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("ogygiad starting...");

    // Load configuration
    let config = ogygia_common::config::load_config()
        .context("failed to load configuration")?
        .context(
            "No configuration found. Ensure ogygia is enabled in your NixOS configuration.",
        )?;

    let zk_config = config
        .zookeeper
        .context("ZooKeeper configuration is required for ogygiad")?;

    // Detect hostname
    let hostname = detect_hostname();
    info!("Detected hostname: {}", hostname);

    // Initialize publisher
    let mut publisher = Publisher::new(&zk_config, hostname)
        .context("failed to initialize ZooKeeper publisher")?;

    // Perform initial publish
    info!("Performing initial state publish");
    let revisions = collect_all_revisions();
    if let Err(e) = publisher.publish_all(&revisions) {
        error!("Initial publish failed: {:?}", e);
        // Continue anyway - we'll retry on next change
    }

    info!("Starting inotify watch for system changes");

    // Spawn inotify watcher in a dedicated thread
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = StateWatcher::new().context("failed to initialize filesystem watcher")?;
    std::thread::spawn(move || {
        watcher.run_blocking(tx);
    });

    // Main event loop
    while let Some((path, mask)) = rx.recv().await {
        info!("File system change detected: {} (mask: 0x{:x})", path, mask);

        // Collect current state and publish
        let revisions = collect_all_revisions();
        match publisher.publish_all(&revisions) {
            Ok(_) => {}
            Err(e) => {
                error!("Failed to publish updates: {:?}", e);
                if !publisher.is_connected() {
                    warn!("ZooKeeper connection lost, attempting reconnect...");
                    sleep(Publisher::connection_timeout()).await;
                    // Try to recreate publisher
                    match Publisher::new(&zk_config, detect_hostname()) {
                        Ok(new_publisher) => {
                            publisher = new_publisher;
                            info!("Reconnected to ZooKeeper");
                            // Retry publish
                            let revisions = collect_all_revisions();
                            if let Err(e) = publisher.publish_all(&revisions) {
                                error!("Retry publish failed: {:?}", e);
                            }
                        }
                        Err(e) => {
                            error!("Failed to reconnect to ZooKeeper: {:?}", e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
