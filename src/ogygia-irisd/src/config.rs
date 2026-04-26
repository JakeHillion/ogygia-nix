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
    #[serde(default)]
    pub cache: CacheConfig,
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
    /// Maximum age for peer bloom cache in seconds (defaults to 2× TTL)
    #[serde(default)]
    pub peer_bloom_max_age_secs: Option<u64>,
    /// CUSUM reference value for burst detection
    #[serde(default = "default_burst_k")]
    pub burst_k: f64,
    /// CUSUM decision threshold for burst detection (0.0 = disabled)
    #[serde(default = "default_burst_h")]
    pub burst_h: f64,
    /// Max seconds to defer rebuilds during burst (0 = disabled)
    #[serde(default = "default_burst_max_cooldown_secs")]
    pub burst_max_cooldown_secs: u64,
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

fn default_burst_k() -> f64 {
    1.0
}

fn default_burst_h() -> f64 {
    0.0
}

fn default_burst_max_cooldown_secs() -> u64 {
    0
}

impl BloomConfig {
    /// Returns the maximum age for peer bloom cache in seconds.
    /// Defaults to 2× the TTL if not explicitly configured.
    pub fn max_age_secs(&self) -> u64 {
        self.peer_bloom_max_age_secs
            .unwrap_or(self.peer_bloom_ttl_secs * 2)
    }
}

impl Default for BloomConfig {
    fn default() -> Self {
        Self {
            false_positive_rate: default_false_positive_rate(),
            rebuild_threshold: default_rebuild_threshold(),
            peer_bloom_ttl_secs: default_peer_bloom_ttl_secs(),
            peer_bloom_max_age_secs: None,
            burst_k: default_burst_k(),
            burst_h: default_burst_h(),
            burst_max_cooldown_secs: default_burst_max_cooldown_secs(),
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

/// NAR disk cache configuration
#[derive(Debug, Deserialize, Clone)]
pub struct CacheConfig {
    /// Directory for cached zstd-compressed NAR files
    #[serde(default = "default_cache_dir")]
    pub dir: PathBuf,
    /// Seconds of idle time before a cached NAR is evicted (0 = no TTI)
    #[serde(default = "default_time_to_idle_secs")]
    pub time_to_idle_secs: u64,
    /// Maximum total size in bytes of all cached NAR files (0 = unlimited)
    #[serde(default = "default_max_size_bytes")]
    pub max_size_bytes: u64,
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("/var/cache/ogygia-irisd/nar")
}

fn default_time_to_idle_secs() -> u64 {
    3600
}

fn default_max_size_bytes() -> u64 {
    10 * 1024 * 1024 * 1024 // 10 GiB
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            dir: default_cache_dir(),
            time_to_idle_secs: default_time_to_idle_secs(),
            max_size_bytes: default_max_size_bytes(),
        }
    }
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

    // Validate bloom configuration: max_age must be >= ttl
    if config.bloom.max_age_secs() < config.bloom.peer_bloom_ttl_secs {
        anyhow::bail!(
            "bloom.peer_bloom_max_age_secs ({}) must be >= bloom.peer_bloom_ttl_secs ({})",
            config.bloom.max_age_secs(),
            config.bloom.peer_bloom_ttl_secs
        );
    }

    // Validate burst configuration: if burst detection is enabled, cooldown must be > 0
    if config.bloom.burst_h > 0.0 && config.bloom.burst_max_cooldown_secs == 0 {
        anyhow::bail!(
            "bloom.burst_max_cooldown_secs must be > 0 when bloom.burst_h is enabled (got burst_h={}, burst_max_cooldown_secs={})",
            config.bloom.burst_h,
            config.bloom.burst_max_cooldown_secs
        );
    }

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
    fn test_parse_cache_config() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[cache]
dir = "/tmp/nar-cache"
time_to_idle_secs = 7200
max_size_bytes = 5368709120
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.cache.dir, PathBuf::from("/tmp/nar-cache"));
        assert_eq!(config.cache.time_to_idle_secs, 7200);
        assert_eq!(config.cache.max_size_bytes, 5_368_709_120);
    }

    #[test]
    fn test_cache_config_defaults() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.cache.dir,
            PathBuf::from("/var/cache/ogygia-irisd/nar")
        );
        assert_eq!(config.cache.time_to_idle_secs, 3600);
        assert_eq!(config.cache.max_size_bytes, 10 * 1024 * 1024 * 1024);
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

    #[test]
    fn test_parse_config_max_age_default() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
peer_bloom_ttl_secs = 300
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.bloom.peer_bloom_ttl_secs, 300);
        assert_eq!(config.bloom.max_age_secs(), 600);
    }

    #[test]
    fn test_parse_config_max_age_explicit() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
peer_bloom_ttl_secs = 300
peer_bloom_max_age_secs = 900
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.bloom.peer_bloom_ttl_secs, 300);
        assert_eq!(config.bloom.max_age_secs(), 900);
    }

    #[test]
    fn test_config_max_age_validation() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
peer_bloom_ttl_secs = 300
peer_bloom_max_age_secs = 200
"#;
        // Write to a temp file and test load_config validation
        let temp_dir = std::env::temp_dir();
        let config_path = temp_dir.join("test_invalid_max_age.toml");
        std::fs::write(&config_path, toml).unwrap();

        let result = load_config(Some(&config_path));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("peer_bloom_max_age_secs"));
        assert!(err_msg.contains("peer_bloom_ttl_secs"));

        // Clean up
        let _ = std::fs::remove_file(&config_path);
    }

    #[test]
    fn test_burst_config_defaults() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!((config.bloom.burst_k - 1.0).abs() < f64::EPSILON);
        assert!((config.bloom.burst_h - 0.0).abs() < f64::EPSILON);
        assert_eq!(config.bloom.burst_max_cooldown_secs, 0);
    }

    #[test]
    fn test_burst_config_explicit() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
burst_k = 2.0
burst_h = 5.0
burst_max_cooldown_secs = 60
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!((config.bloom.burst_k - 2.0).abs() < f64::EPSILON);
        assert!((config.bloom.burst_h - 5.0).abs() < f64::EPSILON);
        assert_eq!(config.bloom.burst_max_cooldown_secs, 60);
    }

    #[test]
    fn test_burst_config_validation_rejects_zero_cooldown() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
burst_h = 5.0
burst_max_cooldown_secs = 0
"#;
        let temp_dir = std::env::temp_dir();
        let config_path = temp_dir.join("test_invalid_burst.toml");
        std::fs::write(&config_path, toml).unwrap();

        let result = load_config(Some(&config_path));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("burst_max_cooldown_secs"));
        assert!(err_msg.contains("burst_h"));

        let _ = std::fs::remove_file(&config_path);
    }

    #[test]
    fn test_burst_config_validation_accepts_disabled() {
        let toml = r#"
[server]
listen = ["127.0.0.1:35742"]

[bloom]
burst_h = 0.0
burst_max_cooldown_secs = 0
"#;
        let temp_dir = std::env::temp_dir();
        let config_path = temp_dir.join("test_disabled_burst.toml");
        std::fs::write(&config_path, toml).unwrap();

        let result = load_config(Some(&config_path));
        assert!(result.is_ok());

        let _ = std::fs::remove_file(&config_path);
    }
}
