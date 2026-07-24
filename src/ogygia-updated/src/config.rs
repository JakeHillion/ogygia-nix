use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;

/// Where the NixOS module renders the daemon's configuration. Stable so
/// that `ogygia update` can read `repo.url` without asking the daemon.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/ogygia/updated.toml";

/// Daemon configuration, deserialized from TOML.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Directory the daemon owns; holds its clone and canary state.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    pub repo: RepoConfig,
    pub host: HostConfig,
    #[serde(default)]
    pub build: BuildConfig,
    #[serde(default)]
    pub activate: ActivateConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// Git remote to track.
    pub url: String,
    /// Branch whose tip the host follows.
    #[serde(default = "default_branch")]
    pub branch: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Attribute name of this host's nixosConfiguration in the flake.
    pub name: String,
    /// File holding the commit the running system was built from.
    #[serde(default = "default_current_revision_path")]
    pub current_revision_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    /// Flake attribute to build. Defaults to the host's toplevel.
    pub flake_attr: Option<String>,
    /// A cache-resident edition of this host's closure, typically with the
    /// expensive commit-specific parts removed, that the real build largely
    /// shares. Substituted with `--max-jobs 0` before the build to warm the
    /// store; if it cannot be substituted the cycle is skipped rather than
    /// built locally, so cache-dependent hosts never fall back to a full
    /// build.
    pub prefetch_attr: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateConfig {
    /// Reboot automatically when an update changes the kernel.
    #[serde(default)]
    pub allow_reboot: bool,
    /// Minutes to wait before an automatic reboot.
    #[serde(default = "default_reboot_delay_minutes")]
    pub reboot_delay_minutes: u32,
    /// System profile updated with the new generation.
    #[serde(default = "default_profile")]
    pub profile: PathBuf,
    /// Booted system compared against the new generation to detect
    /// kernel changes.
    #[serde(default = "default_booted_system")]
    pub booted_system: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    /// Seconds to wait after startup before the first update.
    #[serde(default = "default_initial_delay_seconds")]
    pub initial_delay_seconds: u64,
    /// Seconds between update cycles.
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: u64,
    /// Upper bound of the random extra delay added to each interval.
    #[serde(default = "default_jitter_seconds")]
    pub jitter_seconds: u64,
    /// Control socket manual update triggers are received on.
    #[serde(default = "default_socket_path")]
    pub socket_path: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing config file {}", path.display()))
    }

    /// Flake attribute to build, defaulting to this host's toplevel.
    pub fn flake_attr(&self) -> String {
        self.build.flake_attr.clone().unwrap_or_else(|| {
            format!(
                "nixosConfigurations.\"{}\".config.system.build.toplevel",
                self.host.name
            )
        })
    }

    /// File holding the commit the next boot's system was built from.
    pub fn next_boot_revision_path(&self) -> PathBuf {
        self.activate.profile.join("sw/share/ogygia/build-revision")
    }

    /// The daemon's private clone of the configuration repository.
    pub fn repo_path(&self) -> PathBuf {
        self.state_dir.join("repo")
    }

    /// File recording the current or most-recent canary.
    pub fn canary_state_path(&self) -> PathBuf {
        self.state_dir.join("canary.json")
    }
}

impl Default for ActivateConfig {
    fn default() -> Self {
        Self {
            allow_reboot: false,
            reboot_delay_minutes: default_reboot_delay_minutes(),
            profile: default_profile(),
            booted_system: default_booted_system(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            initial_delay_seconds: default_initial_delay_seconds(),
            interval_seconds: default_interval_seconds(),
            jitter_seconds: default_jitter_seconds(),
            socket_path: default_socket_path(),
        }
    }
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/lib/ogygia-updated")
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_current_revision_path() -> PathBuf {
    PathBuf::from("/run/current-system/sw/share/ogygia/build-revision")
}

fn default_reboot_delay_minutes() -> u32 {
    15
}

fn default_profile() -> PathBuf {
    PathBuf::from("/nix/var/nix/profiles/system")
}

fn default_booted_system() -> PathBuf {
    PathBuf::from("/run/booted-system")
}

fn default_socket_path() -> PathBuf {
    PathBuf::from(crate::control::DEFAULT_SOCKET_PATH)
}

fn default_initial_delay_seconds() -> u64 {
    900
}

fn default_interval_seconds() -> u64 {
    3600
}

fn default_jitter_seconds() -> u64 {
    1800
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_uses_defaults() {
        let config: Config = toml::from_str(
            r#"
            [repo]
            url = "https://git.example.com/nixos.git"

            [host]
            name = "host.example.com"
            "#,
        )
        .unwrap();

        assert_eq!(config.repo.branch, "main");
        assert_eq!(
            config.repo_path(),
            PathBuf::from("/var/lib/ogygia-updated/repo")
        );
        assert_eq!(
            config.canary_state_path(),
            PathBuf::from("/var/lib/ogygia-updated/canary.json")
        );
        assert_eq!(
            config.flake_attr(),
            "nixosConfigurations.\"host.example.com\".config.system.build.toplevel"
        );
        assert_eq!(
            config.next_boot_revision_path(),
            PathBuf::from("/nix/var/nix/profiles/system/sw/share/ogygia/build-revision")
        );
        assert!(!config.activate.allow_reboot);
        assert_eq!(config.daemon.interval_seconds, 3600);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let result: Result<Config, _> = toml::from_str(
            r#"
            [repo]
            url = "https://git.example.com/nixos.git"
            urll = "typo"

            [host]
            name = "host.example.com"
            "#,
        );
        assert!(result.is_err());
    }
}
