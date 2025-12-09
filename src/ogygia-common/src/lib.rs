//! Shared configuration and utilities for Ogygia components.
//!
//! This library provides common functionality used by both the Ogygia CLI
//! and the ogygiad daemon, including configuration file parsing and system
//! state path definitions.

pub mod config;

/// Relative path from system closure to the build revision file.
pub const REVISION_RELATIVE_PATH: &str = "sw/share/ogygia/build-revision";

/// Relative path from system closure to the configuration file.
pub const CONFIG_RELATIVE_PATH: &str = "sw/share/ogygia/config.toml";

/// Environment variable to override the configuration file path.
pub const CONFIG_OVERRIDE_ENV: &str = "OGYGIA_CONFIG";

/// Environment variable to override hostname detection.
pub const HOSTNAME_OVERRIDE_ENV: &str = "OGYGIA_HOSTNAME";

/// Number of system states tracked per host (current, booted, next boot).
pub const STATE_COUNT: usize = 3;

/// Metadata about a NixOS system state (without display information).
#[derive(Clone, Copy, Debug)]
pub struct SystemStateData {
    /// Absolute path on the local filesystem where this system state is stored.
    pub base_path: &'static str,
    /// Name of the ZooKeeper znode under the host's namespace that stores this state.
    pub znode_name: &'static str,
}

/// The three system states we track for each NixOS host.
pub const SYSTEM_STATE_DATA: [SystemStateData; STATE_COUNT] = [
    SystemStateData {
        base_path: "/run/current-system",
        znode_name: "current",
    },
    SystemStateData {
        base_path: "/run/booted-system",
        znode_name: "booted",
    },
    SystemStateData {
        base_path: "/nix/var/nix/profiles/system",
        znode_name: "nextboot",
    },
];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_join_zk_path() {
        assert_eq!(join_zk_path("/", "child"), "/child");
        assert_eq!(join_zk_path("/parent", "child"), "/parent/child");
        assert_eq!(join_zk_path("/parent/", "/child"), "/parent/child");
        assert_eq!(join_zk_path("/parent", "/child/"), "/parent/child/");
    }
}
