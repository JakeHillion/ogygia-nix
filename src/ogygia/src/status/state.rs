//! State gathering for NixOS build revision tracking.
//!
//! This module handles all data collection from:
//! - Local filesystem (reading system state paths and build revision files)
//! - etcd (fetching fleet-wide host state)
//! - ZooKeeper (fetching fleet-wide host state)
//! - Configuration files (loading TOML config)
//! - Hostname detection (environment, commands, syscalls)
//!
//! All functions return raw data without formatting or truncation.
//! Display/rendering logic is handled by the `display` module.

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use etcd_client::Client as EtcdClient;
use etcd_client::ConnectOptions;
use etcd_client::GetOptions;
use hostname::get as get_hostname;
use serde::Deserialize;
use zookeeper::WatchedEvent;
use zookeeper::Watcher;
use zookeeper::ZkError;
use zookeeper::ZooKeeper;

/// Number of system states tracked per host (current, booted, next boot).
pub const STATE_COUNT: usize = 3;

/// Minimum ZooKeeper connection timeout in seconds.
const MIN_TIMEOUT_SECONDS: u64 = 1;

/// Relative path from system closure to the build revision file.
const REVISION_RELATIVE_PATH: &str = "sw/share/ogygia/build-revision";

/// Relative path from system closure to the configuration file.
const CONFIG_RELATIVE_PATH: &str = "sw/share/ogygia/config.toml";

/// Environment variable to override the configuration file path.
const HOSTNAME_OVERRIDE_ENV: &str = "OGYGIA_HOSTNAME";

/// Environment variable to override hostname detection.
const CONFIG_OVERRIDE_ENV: &str = "OGYGIA_CONFIG";

/// Metadata about a NixOS system state (without display information).
#[derive(Clone, Copy)]
pub struct SystemStateData {
    /// Absolute path on the local filesystem where this system state is stored.
    pub base_path: &'static str,
    /// Name of the key in etcd/ZooKeeper under the host's namespace that stores this state.
    pub key_name: &'static str,
}

/// The three system states we track for each NixOS host.
pub const SYSTEM_STATE_DATA: [SystemStateData; STATE_COUNT] = [
    SystemStateData {
        base_path: "/run/current-system",
        key_name: "current",
    },
    SystemStateData {
        base_path: "/run/booted-system",
        key_name: "booted",
    },
    SystemStateData {
        base_path: "/nix/var/nix/profiles/system",
        key_name: "nextboot",
    },
];

/// Array of optional revision strings for the three tracked states.
pub type StateValues = [Option<String>; STATE_COUNT];

/// Raw data representing one host's system state.
///
/// Contains unformatted data - full revision strings, complete hostnames, etc.
/// Display formatting (truncation, domain trimming) is handled elsewhere.
#[derive(Clone, Debug)]
pub struct HostState {
    /// Full hostname (FQDN or short name, as stored in etcd/ZooKeeper or detected locally).
    pub host: String,
    /// Build revisions for current, booted, and next boot states (full strings, not truncated).
    pub values: StateValues,
    /// True if this row represents the local host.
    pub is_local: bool,
}

/// Parsed and validated CLI configuration.
#[derive(Debug)]
pub struct CliConfig {
    /// Path to the configuration file that was loaded.
    pub path: PathBuf,
    /// Optional domain suffix to trim from hostnames (normalized).
    pub domain_suffix: Option<String>,
    /// Optional etcd connection configuration.
    pub etcd: Option<EtcdCliConfig>,
    /// Optional ZooKeeper connection configuration.
    pub zookeeper: Option<ZookeeperCliConfig>,
}

/// Processed etcd configuration ready for connection.
#[derive(Debug)]
pub struct EtcdCliConfig {
    /// List of etcd endpoints in "http://host:port" format.
    pub endpoints: Vec<String>,
    /// Normalized namespace path (starts with /, no trailing /).
    pub namespace: String,
    /// Connection timeout duration.
    pub timeout: Duration,
}

/// Processed ZooKeeper configuration ready for connection.
#[derive(Debug)]
pub struct ZookeeperCliConfig {
    /// List of ZooKeeper endpoints in "host:port" format.
    pub endpoints: Vec<String>,
    /// Normalized namespace path (starts with /, no trailing /).
    pub namespace: String,
    /// Connection timeout duration.
    pub timeout: Duration,
}

/// Creates an empty state values array with all entries set to `None`.
pub fn empty_state_values() -> StateValues {
    [(); STATE_COUNT].map(|_| None)
}

/// Collects system state for the local host by reading from filesystem paths.
///
/// Reads the build revision files from the three standard NixOS system state
/// directories and returns raw data (full revision strings, not truncated).
pub fn collect_local_state(hostname: String) -> HostState {
    let mut values = empty_state_values();
    for (idx, state) in SYSTEM_STATE_DATA.iter().enumerate() {
        values[idx] = read_revision(Path::new(state.base_path));
    }

    HostState {
        host: hostname,
        values,
        is_local: true,
    }
}

/// Reads a build revision from a system state directory.
///
/// Returns the full revision string (not truncated). Returns `None` if the file
/// doesn't exist, or `Some(error_string)` if reading fails for other reasons.
fn read_revision(base_path: &Path) -> Option<String> {
    let revision_path = base_path.join(REVISION_RELATIVE_PATH);

    match fs::read_to_string(&revision_path) {
        Ok(contents) => Some(contents.trim().to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Some("permission denied".to_string())
        }
        Err(error) => Some(format!("error: {}", error)),
    }
}

/// Fetches host state data from ZooKeeper.
///
/// Connects to ZooKeeper, lists all hosts under the configured namespace,
/// and reads their build revisions for all three system states.
///
/// Returns raw data with full hostnames (no domain trimming) and full revision
/// strings (no truncation).
///
/// # Arguments
///
/// * `config` - ZooKeeper connection and namespace configuration
/// * `exclude_host` - Optional hostname matcher to exclude (typically the local host)
///
/// # Returns
///
/// A vector of host state rows, sorted alphabetically by hostname.
/// Missing znodes are represented as `None`.
pub fn fetch_zookeeper_state(
    config: &ZookeeperCliConfig,
    exclude_host: Option<&HostMatcher>,
) -> Result<Vec<HostState>> {
    let connection_string = config.endpoints.join(",");
    let zk =
        ZooKeeper::connect(&connection_string, config.timeout, NoopWatcher).with_context(|| {
            format!(
                "failed to connect to ZooKeeper at {}. \
                 Check that the endpoints are reachable and the ZooKeeper service is running. \
                 Connection timeout: {:?}",
                connection_string, config.timeout
            )
        })?;

    let hosts = zk.get_children(&config.namespace, false).with_context(|| {
        format!(
            "failed to list hosts under {}. \
                 The namespace may not exist yet, or you may not have read permissions. \
                 This is normal if no publisher daemon has written data yet",
            config.namespace
        )
    })?;

    let mut rows = Vec::with_capacity(hosts.len());
    for host in hosts {
        if exclude_host
            .map(|matcher| matcher.matches(&host))
            .unwrap_or(false)
        {
            continue;
        }

        let mut values = empty_state_values();
        let host_path = join_zk_path(&config.namespace, &host);

        for (idx, state) in SYSTEM_STATE_DATA.iter().enumerate() {
            let path = join_zk_path(&host_path, state.key_name);
            match zk.get_data(&path, false) {
                Ok((data, _)) => {
                    if let Some(value) = parse_zk_revision_bytes(&data) {
                        values[idx] = Some(value);
                    }
                }
                Err(ZkError::NoNode) => { /* missing state is acceptable */ }
                Err(error) => {
                    return Err(anyhow!(error)).with_context(|| format!("failed to read {path}"));
                }
            }
        }

        rows.push(HostState {
            host,
            values,
            is_local: false,
        });
    }

    rows.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(rows)
}

/// Fetches host state data from etcd.
///
/// Connects to etcd, lists all hosts under the configured namespace,
/// and reads their build revisions for all three system states.
///
/// Returns raw data with full hostnames (no domain trimming) and full revision
/// strings (no truncation).
///
/// # Arguments
///
/// * `config` - etcd connection and namespace configuration
/// * `exclude_host` - Optional hostname matcher to exclude (typically the local host)
///
/// # Returns
///
/// A vector of host state rows, sorted alphabetically by hostname.
/// Missing keys are represented as `None`.
pub async fn fetch_etcd_state(
    config: &EtcdCliConfig,
    exclude_host: Option<&HostMatcher>,
) -> Result<Vec<HostState>> {
    let opts = ConnectOptions::new().with_timeout(config.timeout);
    let mut client = EtcdClient::connect(&config.endpoints, Some(opts))
        .await
        .with_context(|| {
            format!(
                "failed to connect to etcd at {:?}. \
                 Check that the endpoints are reachable and the etcd service is running. \
                 Connection timeout: {:?}",
                config.endpoints, config.timeout
            )
        })?;

    // Get all keys under the namespace to find hosts
    let range_end = get_etcd_range_end(&config.namespace);
    let opts = GetOptions::new().with_range(range_end);
    let response = client
        .get(config.namespace.as_str(), Some(opts))
        .await
        .with_context(|| {
            format!(
                "failed to list hosts under {}. \
                 The namespace may not exist yet, or you may not have read permissions. \
                 This is normal if no publisher daemon has written data yet",
                config.namespace
            )
        })?;

    // Extract unique hostnames from keys
    let mut hosts: Vec<String> = Vec::new();
    for kv in response.kvs() {
        let key = kv.key_str().unwrap_or("");
        if let Some(relative_path) = key.strip_prefix(&config.namespace) {
            let relative_path = relative_path.trim_start_matches('/');
            let host = relative_path.split('/').next().map(String::from);
            if let Some(host) = host {
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
            }
        }
    }

    let mut rows = Vec::with_capacity(hosts.len());
    for host in hosts {
        if exclude_host
            .map(|matcher| matcher.matches(&host))
            .unwrap_or(false)
        {
            continue;
        }

        let mut values = empty_state_values();
        let host_prefix = join_etcd_path(&config.namespace, &host);

        for (idx, state) in SYSTEM_STATE_DATA.iter().enumerate() {
            let key = join_etcd_path(&host_prefix, state.key_name);
            match client.get(key.as_str(), None).await {
                Ok(response) => {
                    if let Some(kv) = response.kvs().first() {
                        if let Some(value) = parse_etcd_value(kv.value()) {
                            values[idx] = Some(value);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("etcd key {} not found: {}", key, e);
                    /* missing key is acceptable */
                }
            }
        }

        rows.push(HostState {
            host,
            values,
            is_local: false,
        });
    }

    rows.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(rows)
}

/// Computes the range end for an etcd prefix query.
///
/// This is used to get all keys with a given prefix.
fn get_etcd_range_end(prefix: &str) -> Vec<u8> {
    let mut end = prefix.as_bytes().to_vec();
    for i in (0..end.len()).rev() {
        if end[i] < 0xff {
            end[i] += 1;
            end.truncate(i + 1);
            return end;
        }
    }
    // If all bytes are 0xff, return an empty vec (means "all keys")
    vec![]
}

/// Joins two etcd key path components, handling slashes correctly.
///
/// etcd keys are forward-slash separated strings (not platform-specific
/// filesystem paths), so we need custom logic to handle edge cases like root paths
/// and ensure no double slashes.
pub fn join_etcd_path(prefix: &str, child: &str) -> String {
    if prefix == "/" {
        format!("/{}", child.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            child.trim_start_matches('/')
        )
    }
}

/// Parses build revision bytes from etcd value data.
///
/// Returns the full revision string (not truncated).
/// Returns `None` if the data is not valid UTF-8, is empty, or contains only whitespace.
fn parse_etcd_value(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Parses build revision bytes from ZooKeeper znode data.
///
/// Returns the full revision string (not truncated).
/// Returns `None` if the data is not valid UTF-8, is empty, or contains only whitespace.
fn parse_zk_revision_bytes(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Joins two ZooKeeper path components, handling slashes correctly.
///
/// ZooKeeper paths are always forward-slash separated strings (not platform-specific
/// filesystem paths), so we need custom logic to handle edge cases like root paths
/// and ensure no double slashes.
pub fn join_zk_path(prefix: &str, child: &str) -> String {
    if prefix == "/" {
        format!("/{}", child.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            child.trim_start_matches('/')
        )
    }
}

/// Loads the Ogygia CLI configuration from the filesystem.
///
/// Searches for configuration files in the following order:
/// 1. `$OGYGIA_CONFIG` environment variable
/// 2. System state paths with `config.toml`
///
/// # Returns
///
/// * `Ok(Some(CliConfig))` - Configuration loaded successfully
/// * `Ok(None)` - No configuration file found (will use local-only mode)
/// * `Err(_)` - Configuration file exists but couldn't be read or parsed
pub fn load_cli_config() -> Result<Option<CliConfig>> {
    let Some(path) = locate_config_file() else {
        return Ok(None);
    };

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read Ogygia config at {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse Ogygia config at {}", path.display()))?;

    let Some(ogygia_cfg) = raw.ogygia else {
        return Ok(Some(CliConfig {
            path,
            domain_suffix: None,
            etcd: None,
            zookeeper: None,
        }));
    };

    let domain_suffix = ogygia_cfg.domain.and_then(|d| normalize_domain(&d));

    // Parse etcd configuration (preferred)
    let etcd = match ogygia_cfg.etcd {
        Some(raw_etcd) => {
            if raw_etcd.endpoints.is_empty() {
                return Err(anyhow!(
                    "etcd config {} does not define any endpoints. \
                     Add endpoints in the format [\"http://host1:2379\", \"http://host2:2379\"]",
                    path.display()
                ));
            }

            // Validate endpoint format
            for endpoint in &raw_etcd.endpoints {
                if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                    return Err(anyhow!(
                        "Invalid etcd endpoint '{}' in {}. \
                         Endpoints must be in 'http://host:port' or 'https://host:port' format \
                         (e.g., 'http://etcd1.example.com:2379')",
                        endpoint,
                        path.display()
                    ));
                }
            }

            let base_namespace = normalize_namespace(&raw_etcd.namespace);
            // Append /nixos/versions to the base namespace (e.g., /ogygia -> /ogygia/nixos/versions)
            let namespace = if base_namespace == "/" {
                "/nixos/versions".to_string()
            } else {
                format!("{}/nixos/versions", base_namespace)
            };

            Some(EtcdCliConfig {
                endpoints: raw_etcd.endpoints,
                namespace,
                timeout: Duration::from_secs(raw_etcd.timeout_seconds.max(MIN_TIMEOUT_SECONDS)),
            })
        }
        None => None,
    };

    // Parse ZooKeeper configuration
    let zookeeper = match ogygia_cfg.zookeeper {
        Some(raw_zk) => {
            if raw_zk.endpoints.is_empty() {
                return Err(anyhow!(
                    "ZooKeeper config {} does not define any endpoints. \
                     Add endpoints in the format [\"host1:2181\", \"host2:2181\"]",
                    path.display()
                ));
            }

            // Validate endpoint format
            for endpoint in &raw_zk.endpoints {
                if !endpoint.contains(':') {
                    return Err(anyhow!(
                        "Invalid ZooKeeper endpoint '{}' in {}. \
                         Endpoints must be in 'host:port' format (e.g., 'zk1.example.com:2181')",
                        endpoint,
                        path.display()
                    ));
                }
            }

            Some(ZookeeperCliConfig {
                endpoints: raw_zk.endpoints,
                namespace: normalize_namespace(&raw_zk.namespace),
                timeout: Duration::from_secs(raw_zk.timeout_seconds.max(MIN_TIMEOUT_SECONDS)),
            })
        }
        None => None,
    };

    Ok(Some(CliConfig {
        path,
        domain_suffix,
        etcd,
        zookeeper,
    }))
}

/// Normalizes a ZooKeeper namespace path.
///
/// Ensures the path:
/// - Starts with `/`
/// - Does not end with `/` (unless it's the root)
/// - Defaults to `/nixos/versions` if empty
pub fn normalize_namespace(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "/nixos/versions".into();
    }

    let prefixed = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    if prefixed.len() == 1 {
        "/".into()
    } else {
        prefixed.trim_end_matches('/').to_string()
    }
}

/// Normalizes a domain suffix by trimming whitespace and dots.
///
/// Returns `None` if the domain is empty after normalization.
pub fn normalize_domain(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Searches for a configuration file in standard locations.
///
/// Checks the environment variable first, then falls back to searching
/// system state paths for the configuration file.
fn locate_config_file() -> Option<PathBuf> {
    if let Ok(path) = env::var(CONFIG_OVERRIDE_ENV) {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    for state in &SYSTEM_STATE_DATA {
        let candidate = Path::new(state.base_path).join(CONFIG_RELATIVE_PATH);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

/// Raw configuration structure as parsed from TOML.
#[derive(Debug, Deserialize)]
struct RawConfig {
    ogygia: Option<RawOgygiaConfig>,
}

/// Raw Ogygia configuration section from TOML.
#[derive(Debug, Deserialize)]
struct RawOgygiaConfig {
    /// Domain suffix to trim from hostnames for display (e.g., "example.com").
    #[serde(default)]
    domain: Option<String>,
    /// etcd connection settings.
    #[serde(default)]
    etcd: Option<RawEtcdConfig>,
    /// ZooKeeper connection settings.
    #[serde(default)]
    zookeeper: Option<RawZookeeperConfig>,
}

/// Raw etcd configuration section from TOML.
#[derive(Debug, Deserialize)]
struct RawEtcdConfig {
    /// List of etcd server endpoints (e.g., ["http://etcd1:2379", "http://etcd2:2379"]).
    #[serde(default)]
    endpoints: Vec<String>,
    /// etcd namespace prefix where host data is stored.
    #[serde(default = "default_etcd_namespace")]
    namespace: String,
    /// Connection timeout in seconds.
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

/// Raw ZooKeeper configuration section from TOML.
#[derive(Debug, Deserialize)]
struct RawZookeeperConfig {
    /// List of ZooKeeper server endpoints (e.g., ["zk1:2181", "zk2:2181"]).
    #[serde(default)]
    endpoints: Vec<String>,
    /// ZooKeeper namespace path where host data is stored.
    #[serde(default = "default_zookeeper_namespace")]
    namespace: String,
    /// Connection timeout in seconds.
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

/// Default connection timeout in seconds.
const fn default_timeout_seconds() -> u64 {
    10
}

/// Default etcd namespace for host data storage.
fn default_etcd_namespace() -> String {
    "/ogygia".to_string()
}

/// Default ZooKeeper namespace for host data storage.
fn default_zookeeper_namespace() -> String {
    "/nixos/versions".to_string()
}

/// No-op watcher for ZooKeeper connections.
///
/// We don't need to react to ZooKeeper events since we only perform
/// one-shot reads, so this watcher does nothing.
struct NoopWatcher;

impl Watcher for NoopWatcher {
    fn handle(&self, _: WatchedEvent) {}
}

/// Detects the current hostname using multiple fallback strategies.
///
/// Tries the following methods in order:
/// 1. `OGYGIA_HOSTNAME` environment variable (user override)
/// 2. `hostname -f` command (fully qualified)
/// 3. `HOSTNAME` environment variable
/// 4. `hostname` command (short name)
/// 5. `gethostname()` syscall
/// 6. Fallback to "unknown-host" if all else fails
pub fn detect_hostname() -> String {
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
///
/// Returns `None` if the command fails or produces empty output.
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
///
/// Returns `None` if the result is empty.
fn normalize_hostname(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Helper for matching hostnames in case-insensitive and FQDN-agnostic way.
///
/// This matcher handles various hostname formats:
/// - Fully qualified domain names (FQDN): `host.example.com`
/// - Short hostnames: `host`
/// - Mixed case variations: `Host`, `HOST`, etc.
///
/// When comparing hostnames, it normalizes both to lowercase and tries to match
/// either the full name or just the first component (short name).
pub struct HostMatcher {
    /// The normalized full hostname in lowercase.
    canonical_lower: String,
    /// The short hostname (first component before '.') in lowercase.
    short_lower: String,
}

impl HostMatcher {
    /// Creates a new hostname matcher from a hostname string.
    ///
    /// Extracts both the full hostname and the short name (first component)
    /// in normalized lowercase form for flexible matching.
    pub fn new(host: &str) -> Self {
        let canonical_lower = host.trim().to_ascii_lowercase();
        let short_lower = canonical_lower
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(canonical_lower.as_str())
            .to_string();
        Self {
            canonical_lower,
            short_lower,
        }
    }

    /// Checks if the given hostname matches this matcher.
    ///
    /// Returns `true` if any of these conditions are met:
    /// - Full names match (case-insensitive)
    /// - Short names match (case-insensitive)
    /// - One's full name matches the other's short name
    pub fn matches(&self, other: &str) -> bool {
        let other_lower = other.trim().to_ascii_lowercase();
        if other_lower == self.canonical_lower || other_lower == self.short_lower {
            return true;
        }

        let other_short = other_lower
            .split('.')
            .next()
            .unwrap_or(other_lower.as_str())
            .to_string();

        other_short == self.canonical_lower || other_short == self.short_lower
    }
}
