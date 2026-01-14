//! Configuration loading and types

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;

/// Root configuration
#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub bloom: BloomConfig,
    #[serde(default)]
    pub peers: PeersConfig,
    #[serde(default)]
    pub trust: TrustConfig,
}

/// HTTP server configuration
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// Listen addresses (e.g., ["127.0.0.1:35742", "172.20.0.1:35742"])
    pub listen: Vec<String>,
    /// Cache priority advertised in /nix-cache-info (lower = higher priority)
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_priority() -> u32 {
    30
}

/// Bloom filter configuration
#[derive(Debug, Deserialize, Clone)]
pub struct BloomConfig {
    /// Target false-positive rate (e.g. 0.01 = 1%)
    #[serde(default = "default_false_positive_rate")]
    pub false_positive_rate: f64,
    /// Deletion ratio at which to trigger a rebuild
    #[serde(default = "default_rebuild_threshold")]
    pub rebuild_threshold: f64,
    /// Peer bloom cache TTL in seconds
    #[serde(default = "default_peer_bloom_ttl_secs")]
    pub peer_bloom_ttl_secs: u64,
}

fn default_false_positive_rate() -> f64 {
    0.01
}

fn default_rebuild_threshold() -> f64 {
    0.005
}

fn default_peer_bloom_ttl_secs() -> u64 {
    300
}

impl Default for BloomConfig {
    fn default() -> Self {
        Self {
            false_positive_rate: default_false_positive_rate(),
            rebuild_threshold: default_rebuild_threshold(),
            peer_bloom_ttl_secs: default_peer_bloom_ttl_secs(),
        }
    }
}

/// Peer configuration — static list of peer URLs
#[derive(Debug, Deserialize, Clone, Default)]
pub struct PeersConfig {
    /// Peer HTTP URLs (e.g., ["http://10.100.0.1:35742"])
    #[serde(default)]
    pub urls: Vec<String>,
}

/// Trust configuration for peer content verification
#[derive(Debug, Deserialize, Clone, Default)]
pub struct TrustConfig {
    /// Public keys trusted when fetching content from peers
    /// Format: "name:base64-public-key" (e.g., "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=")
    #[serde(default)]
    pub trusted_keys: Vec<String>,
}

/// Load configuration from file or default locations
pub fn load_config(path: Option<&Path>) -> Result<Config> {
    let config_path = if let Some(p) = path {
        p.to_path_buf()
    } else if let Ok(p) = std::env::var("OGYGIA_IRISD_CONFIG") {
        PathBuf::from(p)
    } else {
        // Search default locations
        let candidates = [
            PathBuf::from("/etc/ogygia-irisd/config.toml"),
            PathBuf::from("config.toml"),
        ];
        candidates
            .into_iter()
            .find(|p| p.exists())
            .context("No configuration file found")?
    };

    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

    let config: Config = toml::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.listen, vec!["127.0.0.1:35742"]);
        assert!((config.bloom.false_positive_rate - 0.01).abs() < f64::EPSILON);
        assert!(config.peers.urls.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
false_positive_rate = 0.001
rebuild_threshold = 0.2
peer_bloom_ttl_secs = 600

[peers]
urls = ["http://10.100.0.1:35742", "http://10.100.0.2:35742"]

[trust]
trusted_keys = [
    "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=",
    "my-cache-1:abc123def456="
]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!((config.bloom.false_positive_rate - 0.001).abs() < f64::EPSILON);
        assert_eq!(config.peers.urls.len(), 2);
        assert_eq!(config.trust.trusted_keys.len(), 2);
    }

    #[test]
    fn test_trust_config_default() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.trust.trusted_keys.is_empty());
    }
}
