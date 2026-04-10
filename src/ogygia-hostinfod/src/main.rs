use std::collections::HashMap;
use std::env;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use etcd_client::Client;
use etcd_client::PutOptions;
use hostname::get as get_hostname;
use inotify::EventMask;
use inotify::Inotify;
use inotify::WatchMask;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::warn;

const HOSTNAME_OVERRIDE_ENV: &str = "OGYGIA_HOSTNAME";
const REVISION_RELATIVE_PATH: &str = "sw/share/ogygia/build-revision";

/// System states to track and their corresponding etcd key suffixes.
const SYSTEM_STATES: [(&str, &str); 3] = [
    ("/run/current-system", "current"),
    ("/run/booted-system", "booted"),
    ("/nix/var/nix/profiles/system", "nextboot"),
];

/// inotify watch flags matching the reference Python implementation:
/// CREATE | DELETE | MOVED_TO | MOVED_FROM | ATTRIB | DONT_FOLLOW
const WATCH_MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::DELETE)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::ATTRIB)
    .union(WatchMask::DONT_FOLLOW);

#[derive(Parser, Debug)]
#[command(
    name = "ogygia-hostinfod",
    version,
    about = "Ogygia host info etcd publisher"
)]
struct Cli {
    /// etcd endpoints (comma-separated or multiple args)
    #[arg(
        long,
        env = "OGYGIA_ETCD_ENDPOINTS",
        value_delimiter = ',',
        required = true
    )]
    endpoints: Vec<String>,

    /// etcd key prefix
    #[arg(
        long,
        env = "OGYGIA_ETCD_PREFIX",
        default_value = "/ogygia/nixos/versions"
    )]
    prefix: String,
}

fn detect_hostname() -> String {
    hostname_from_env(HOSTNAME_OVERRIDE_ENV)
        .or_else(|| hostname_from_command("hostname", &["-f"]))
        .or_else(|| hostname_from_env("HOSTNAME"))
        .or_else(|| hostname_from_command("hostname", &[]))
        .or_else(hostname_from_syscall)
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn hostname_from_env(var: &str) -> Option<String> {
    env::var(var)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
}

fn hostname_from_command(program: &str, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
}

fn hostname_from_syscall() -> Option<String> {
    get_hostname()
        .ok()
        .and_then(|s| s.into_string().ok())
        .filter(|s| !s.trim().is_empty())
}

fn read_revision(base_path: &str) -> Option<String> {
    let revision_path = Path::new(base_path).join(REVISION_RELATIVE_PATH);

    match std::fs::read_to_string(&revision_path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!("Failed to read {}: {}", revision_path.display(), e);
            None
        }
    }
}

async fn update_etcd_key(
    client: &mut Client,
    key: &str,
    revision: &str,
    cached: &mut HashMap<String, String>,
) -> Result<bool> {
    if cached.get(key) == Some(&revision.to_string()) {
        return Ok(false);
    }

    info!("Updating etcd key {} = {}", key, revision);
    client
        .put(key, revision, Some(PutOptions::default()))
        .await?;
    cached.insert(key.to_string(), revision.to_string());
    Ok(true)
}

async fn update_all_states(
    client: &mut Client,
    prefix: &str,
    hostname: &str,
    cached: &mut HashMap<String, String>,
) -> Result<u32> {
    info!("Starting update_all_states...");
    let mut updates = 0;

    for (base_path, state_name) in SYSTEM_STATES.iter() {
        info!("Processing state: {} at path: {}", state_name, base_path);
        match read_revision(base_path) {
            Some(revision) => {
                info!("Read revision for {}: {}", state_name, revision);
                let key = format!("{}/{}/{}", prefix, hostname, state_name);
                if update_etcd_key(client, &key, &revision, cached).await? {
                    updates += 1;
                }
            }
            None => {
                info!("No revision found for {} at {}", state_name, base_path);
            }
        }
    }

    if updates > 0 {
        info!("Made {} etcd updates", updates);
    } else {
        info!("No etcd updates needed");
    }

    info!("Finished update_all_states");

    Ok(updates)
}

/// Check if an inotify event indicates a relevant change to one of our watched paths
fn is_relevant_event(event: &inotify::Event<&std::ffi::OsStr>) -> bool {
    let mask = event.mask;

    // Check if any of the relevant flags are set
    let is_relevant = mask.intersects(
        EventMask::CREATE
            .union(EventMask::DELETE)
            .union(EventMask::MOVED_TO)
            .union(EventMask::MOVED_FROM)
            .union(EventMask::ATTRIB),
    );

    if !is_relevant {
        return false;
    }

    // Check if the event is on a file we care about
    if let Some(name) = event.name {
        let name_bytes = name.as_bytes();
        // Check if it's the "system" link in /nix/var/nix/profiles/
        // or related to current-system/booted-system
        if name_bytes == b"system"
            || name_bytes == b"current-system"
            || name_bytes == b"booted-system"
        {
            return true;
        }
    }

    // Events without a name are on the watched directory itself (e.g., ATTRIB on the symlink)
    if event.name.is_none() {
        // Check if the event mask includes ISDIR or is ATTRIB (symlink change)
        return mask.contains(EventMask::ATTRIB)
            || mask.contains(EventMask::CREATE)
            || mask.contains(EventMask::DELETE);
    }

    false
}

/// Spawn a blocking thread that sets up inotify and watches for changes
fn spawn_inotify_thread(tx: mpsc::Sender<()>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Initialize inotify
        let mut inotify = match Inotify::init() {
            Ok(i) => i,
            Err(e) => {
                warn!("Failed to initialize inotify: {}", e);
                return;
            }
        };

        // Add watches for each system state
        let mut watch_count = 0;
        for (base_path, state_name) in SYSTEM_STATES.iter() {
            let path = Path::new(base_path);
            let parent = if path.file_name().is_some() {
                path.parent().unwrap_or(Path::new("/"))
            } else {
                path
            };

            if parent.exists() {
                match inotify.watches().add(parent, WATCH_MASK) {
                    Ok(wd) => {
                        info!(
                            "Watching {} for {} (watch descriptor: {:?})",
                            parent.display(),
                            state_name,
                            wd
                        );
                        watch_count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to watch {}: {}", parent.display(), e);
                    }
                }
            } else {
                warn!("Parent path does not exist: {}", parent.display());
            }
        }

        if watch_count == 0 {
            warn!("No directories could be watched, exiting watcher thread");
            return;
        }

        info!("Starting inotify watch loop in blocking thread");

        // Buffer for reading events (must be large enough for multiple events)
        let mut buffer = [0u8; 4096];

        loop {
            match inotify.read_events_blocking(&mut buffer) {
                Ok(events) => {
                    let mut relevant_change = false;
                    for event in events {
                        debug!(
                            "inotify event: {:?}, mask: {:?}, name: {:?}",
                            event.wd, event.mask, event.name
                        );

                        if is_relevant_event(&event) {
                            relevant_change = true;
                            if let Some(name) = event.name {
                                info!("Detected change in: {:?}", name);
                            } else {
                                info!(
                                    "Detected change in watched directory (mask: {:?})",
                                    event.mask
                                );
                            }
                        }
                    }

                    if relevant_change {
                        // Use blocking_send since we're in a sync thread
                        if tx.blocking_send(()).is_err() {
                            // Channel closed, exit the thread
                            info!("Channel closed, exiting inotify watcher thread");
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("Error reading inotify events: {}", e);
                    // Small delay before retrying to avoid busy-looping on persistent errors
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    })
}

async fn run(prefix: String, mut client: Client) -> Result<()> {
    let hostname = detect_hostname();
    info!("Hostname: {}", hostname);
    info!("etcd prefix: {}", prefix);

    let mut cached: HashMap<String, String> = HashMap::new();

    // Initial update
    update_all_states(&mut client, &prefix, &hostname, &mut cached).await?;

    // Create async channel for inotify thread communication
    let (tx, mut rx) = mpsc::channel::<()>(100);

    // Spawn blocking thread for inotify
    let inotify_thread = spawn_inotify_thread(tx);

    // Watch for changes and update etcd
    loop {
        tokio::select! {
            // Wait for inotify signals
            maybe_signal = rx.recv() => {
                if maybe_signal.is_none() {
                    info!("Channel closed, exiting...");
                    break;
                }
                info!("Received inotify signal, updating etcd...");

                match update_all_states(&mut client, &prefix, &hostname, &mut cached).await {
                    Ok(updates) => {
                        info!("Finished updating etcd after inotify signal, {} updates made", updates);
                    }
                    Err(e) => {
                        warn!("Failed to update etcd: {}", e);
                    }
                }
            }

            // Handle shutdown signal
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal, exiting...");
                break;
            }
        }
    }

    // Clean up
    drop(rx);
    let _ = inotify_thread.join();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    info!("Connecting to etcd at {:?}", cli.endpoints);

    let client = Client::connect(&cli.endpoints, None)
        .await
        .with_context(|| format!("Failed to connect to etcd at {:?}", cli.endpoints))?;

    info!("Connected to etcd");

    run(cli.prefix, client).await
}
