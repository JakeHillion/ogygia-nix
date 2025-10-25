use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    /// ZooKeeper configuration
    pub zookeeper: ZooKeeperConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ZooKeeperConfig {
    /// ZooKeeper server addresses (comma-separated host:port pairs)
    pub addresses: String,

    /// Enable version uploading to ZooKeeper
    #[serde(default)]
    pub enable_version_upload: bool,

    /// Hostname to use in ZooKeeper paths (typically FQDN)
    pub hostname: String,
}

impl Config {
    pub fn from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
