//! Configuration file parsing and loading for Ogygia.
//!
//! This module provides shared configuration structures and parsing logic
//! used by both the CLI and daemon components.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::{CONFIG_OVERRIDE_ENV, CONFIG_RELATIVE_PATH, SYSTEM_STATE_DATA};

/// Minimum ZooKeeper connection timeout in seconds.
const MIN_TIMEOUT_SECONDS: u64 = 1;

/// Parsed ZooKeeper configuration section.
#[derive(Debug, Clone)]
pub struct ZookeeperConfig {
    /// List of ZooKeeper endpoints in "host:port" format.
    pub endpoints: Vec<String>,
    /// Normalized namespace path (starts with /, no trailing /).
    pub namespace: String,
    /// Connection timeout duration.
    pub timeout: Duration,
}

/// Parsed Ogygia configuration.
#[derive(Debug, Clone)]
pub struct OgygiaConfig {
    /// Path to the configuration file that was loaded.
    pub path: PathBuf,
    /// Optional domain suffix to trim from hostnames (normalized).
    pub domain: Option<String>,
    /// Optional ZooKeeper connection configuration.
    pub zookeeper: Option<ZookeeperConfig>,
}

/// Loads the Ogygia configuration from the filesystem.
///
/// Searches for configuration files in the following order:
/// 1. `$OGYGIA_CONFIG` environment variable
/// 2. System state paths with `config.toml`
///
/// # Returns
///
/// * `Ok(Some(OgygiaConfig))` - Configuration loaded successfully
/// * `Ok(None)` - No configuration file found
/// * `Err(_)` - Configuration file exists but couldn't be read or parsed
pub fn load_config() -> Result<Option<OgygiaConfig>> {
    let Some(path) = locate_config_file() else {
        return Ok(None);
    };

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read Ogygia config at {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse Ogygia config at {}", path.display()))?;

    let Some(ogygia_section) = raw.ogygia else {
        return Ok(Some(OgygiaConfig {
            path,
            domain: None,
            zookeeper: None,
        }));
    };

    let domain = ogygia_section.domain.and_then(|d| normalize_domain(&d));

    let zookeeper = match ogygia_section.zookeeper {
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

            Some(ZookeeperConfig {
                endpoints: raw_zk.endpoints,
                namespace: normalize_namespace(&raw_zk.namespace),
                timeout: Duration::from_secs(raw_zk.timeout_seconds.max(MIN_TIMEOUT_SECONDS)),
            })
        }
        None => None,
    };

    Ok(Some(OgygiaConfig {
        path,
        domain,
        zookeeper,
    }))
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
    /// ZooKeeper connection settings.
    #[serde(default)]
    zookeeper: Option<RawZookeeperConfig>,
}

/// Raw ZooKeeper configuration section from TOML.
#[derive(Debug, Deserialize)]
struct RawZookeeperConfig {
    /// List of ZooKeeper server endpoints (e.g., ["zk1:2181", "zk2:2181"]).
    #[serde(default)]
    endpoints: Vec<String>,
    /// ZooKeeper namespace path where host data is stored.
    #[serde(default = "default_namespace")]
    namespace: String,
    /// Connection timeout in seconds.
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

/// Default ZooKeeper connection timeout in seconds.
const fn default_timeout_seconds() -> u64 {
    10
}

/// Default ZooKeeper namespace for host data storage.
fn default_namespace() -> String {
    "/nixos/versions".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_namespace() {
        assert_eq!(normalize_namespace(""), "/nixos/versions");
        assert_eq!(normalize_namespace("/"), "/");
        assert_eq!(normalize_namespace("/foo"), "/foo");
        assert_eq!(normalize_namespace("foo"), "/foo");
        assert_eq!(normalize_namespace("/foo/"), "/foo");
        assert_eq!(normalize_namespace("/foo/bar/"), "/foo/bar");
    }

    #[test]
    fn test_normalize_domain() {
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("  "), None);
        assert_eq!(normalize_domain("."), None);
        assert_eq!(normalize_domain("..."), None);
        assert_eq!(normalize_domain("example.com"), Some("example.com".to_string()));
        assert_eq!(normalize_domain(".example.com."), Some("example.com".to_string()));
        assert_eq!(normalize_domain("  example.com  "), Some("example.com".to_string()));
    }
}
