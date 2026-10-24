//! Where the daemon's settings live: the `defaults` domain on a Mac, or a
//! JSON file when a test runs the daemon without root.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

use crate::config::ConfigMode;
use crate::config::LocalConfig;
use crate::settings::Settings;

pub trait Preferences: Send + Sync {
    fn load(&self) -> Settings;
    /// Whether a configuration profile forces any of the settings.
    fn managed(&self) -> bool;
    fn store(&self, settings: &Settings) -> Result<()>;
    /// Whether to follow the fleet's Nebula config, and the local one.
    fn load_config(&self) -> (ConfigMode, LocalConfig);
    /// Whether a configuration profile forces the config mode or the local
    /// config.
    fn config_mode_managed(&self) -> bool;
    fn store_config(&self, mode: ConfigMode, local: &LocalConfig) -> Result<()>;
}

/// Preferences kept in one JSON file, standing in for the `defaults` domain
/// when a test runs the daemon. `forced` lists the keys a configuration
/// profile would force, by their `defaults` names.
pub struct FilePreferences {
    pub path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    repository_url: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    config_mode: ConfigMode,
    #[serde(default)]
    local_config: LocalConfig,
    #[serde(default)]
    forced: BTreeSet<String>,
}

impl FilePreferences {
    fn read(&self) -> File {
        std::fs::read(&self.path)
            .ok()
            .and_then(|json| serde_json::from_slice(&json).ok())
            .unwrap_or_default()
    }

    fn write(&self, file: &File) -> Result<()> {
        std::fs::write(&self.path, serde_json::to_vec_pretty(file)?)
            .with_context(|| format!("could not write {}", self.path.display()))
    }

    fn forced(&self, keys: &[&str]) -> bool {
        let file = self.read();
        keys.iter().any(|k| file.forced.contains(*k))
    }
}

impl Preferences for FilePreferences {
    fn load(&self) -> Settings {
        let file = self.read();
        Settings {
            repository_url: file.repository_url,
            branch: file.branch,
            device: file.device,
        }
    }

    fn managed(&self) -> bool {
        use crate::settings::*;
        self.forced(&[KEY_REPOSITORY_URL, KEY_BRANCH, KEY_DEVICE])
    }

    fn store(&self, settings: &Settings) -> Result<()> {
        let mut file = self.read();
        file.repository_url = settings.repository_url.clone();
        file.branch = settings.branch.clone();
        file.device = settings.device.clone();
        self.write(&file)
    }

    fn load_config(&self) -> (ConfigMode, LocalConfig) {
        let file = self.read();
        (file.config_mode, file.local_config)
    }

    fn config_mode_managed(&self) -> bool {
        use crate::settings::*;
        self.forced(&[KEY_CONFIG_MODE, KEY_LOCAL_CONFIG])
    }

    fn store_config(&self, mode: ConfigMode, local: &LocalConfig) -> Result<()> {
        let mut file = self.read();
        file.config_mode = mode;
        file.local_config = local.clone();
        self.write(&file)
    }
}
