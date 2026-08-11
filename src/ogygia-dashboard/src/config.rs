use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub git: GitConfig,
    pub etcd: EtcdConfig,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default)]
    pub hostname_strip_suffix: Option<String>,
    /// Nebula certificate-expiry alerts. Requires the binary to be built with
    /// the `nebula` feature (on by default).
    #[serde(default)]
    pub nebula: NebulaConfig,
}

/// Nebula certificate-expiry alerts. Off unless `enable = true`; thresholds are
/// fixed fractions of each cert's validity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NebulaConfig {
    #[serde(default)]
    pub enable: bool,
}

fn default_title() -> String {
    "Status Page".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(flatten)]
    pub bind: Bind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bind {
    Tcp { port: u16 },
    Unix { socket: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
    /// HTTPS URL of the repository, used for web links and the Gitea API,
    /// and for anonymous git access when `ssh` is not configured.
    pub remote_url: String,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub archive: Option<ArchiveConfig>,
}

/// SSH transport for all git operations, allowing private repositories
/// and pushing. Requires a key with write access when `archive` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshConfig {
    /// SSH URL for the same repository as `remote_url`.
    pub url: String,
    /// Private key, e.g. a Gitea deploy key.
    pub key_path: PathBuf,
    /// Expected host key of the server in known_hosts format
    /// ("ssh-ed25519 AAAA..."). Any host key is accepted when unset.
    #[serde(default)]
    pub host_key: Option<String>,
}

/// Archival of deployed commits to a persistent branch, keeping them
/// reachable after the branch they were deployed from is force-pushed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    #[serde(default = "default_archive_branch")]
    pub branch: String,
}

fn default_archive_branch() -> String {
    "ogygia/deployed-commits-archive".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    #[serde(default = "default_etcd_prefix")]
    pub prefix: String,
}

fn default_etcd_prefix() -> String {
    "/ogygia/nixos/versions".to_string()
}

impl Config {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;

        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.as_ref().display()))?;

        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.git.remote_url.is_empty() {
            return Err(anyhow::anyhow!("git.remote_url cannot be empty"));
        }

        if self.etcd.endpoints.is_empty() {
            return Err(anyhow::anyhow!("etcd.endpoints cannot be empty"));
        }

        for endpoint in &self.etcd.endpoints {
            if endpoint.is_empty() {
                return Err(anyhow::anyhow!("etcd endpoint cannot be empty"));
            }
        }

        if let Some(ssh) = &self.git.ssh
            && ssh.url.is_empty()
        {
            return Err(anyhow::anyhow!("git.ssh.url cannot be empty"));
        }

        if let Some(archive) = &self.git.archive {
            if self.git.ssh.is_none() {
                return Err(anyhow::anyhow!("git.archive requires git.ssh for pushing"));
            }
            if archive.branch.is_empty() {
                return Err(anyhow::anyhow!("git.archive.branch cannot be empty"));
            }
        }

        Ok(())
    }

    /// Override server config with CLI values if provided
    /// CLI args take precedence: socket > port > config file
    pub fn override_server_config(&mut self, port: Option<u16>, socket: Option<String>) {
        match (socket, port) {
            (Some(socket), _) => {
                // Socket takes precedence over port
                self.server.bind = Bind::Unix { socket };
            }
            (None, Some(port)) => {
                // Port specified, override existing config
                self.server.bind = Bind::Tcp { port };
            }
            (None, None) => {
                // No CLI overrides, keep existing config
            }
        }
    }

    /// Get the full git repository URL
    pub fn git_repo_url(&self) -> &str {
        &self.git.remote_url
    }

    /// Get the repository web URL (removes .git suffix if present)
    pub fn repo_web_url(&self) -> String {
        if self.git.remote_url.ends_with(".git") {
            self.git.remote_url[..self.git.remote_url.len() - 4].to_string()
        } else {
            self.git.remote_url.to_string()
        }
    }

    /// Get the pull requests web URL
    pub fn pulls_web_url(&self) -> String {
        format!("{}/pulls", self.repo_web_url())
    }

    /// Get the API URL for pull requests (assumes Gitea/Forgejo API structure)
    pub fn pulls_api_url(&self) -> String {
        let web_url = self.repo_web_url();
        // Extract server, username, and repo from the URL
        if let Some(parts) = self.parse_repo_url(&web_url) {
            format!(
                "https://{}/api/v1/repos/{}/{}/pulls?state=open&limit=1",
                parts.0, parts.1, parts.2
            )
        } else {
            // Fallback if parsing fails
            format!("{web_url}/api/v1/pulls?state=open&limit=1")
        }
    }

    /// Get commit web URL
    pub fn commit_web_url(&self, commit_hash: &str) -> String {
        format!("{}/commit/{}", self.repo_web_url(), commit_hash)
    }

    /// Parse repository URL to extract server, username, and repository name
    /// Returns (server, username, repository) or None if parsing fails
    fn parse_repo_url(&self, url: &str) -> Option<(String, String, String)> {
        let url = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    fn test_config() -> Config {
        Config {
            server: ServerConfig {
                bind: Bind::Tcp { port: 8080 },
            },
            git: GitConfig {
                remote_url: "https://git.example.com/user/repo.git".to_string(),
                ssh: None,
                archive: None,
            },
            etcd: EtcdConfig {
                endpoints: vec!["http://etcd1:2379".to_string()],
                prefix: default_etcd_prefix(),
            },
            title: default_title(),
            hostname_strip_suffix: None,
            nebula: NebulaConfig::default(),
        }
    }

    #[test]
    fn test_load_from_toml_tcp() {
        let toml_content = r#"
[server]
port = 9090

[git]
remote_url = "https://git.example.com/testuser/testrepo.git"

[etcd]
endpoints = ["http://etcd1:2379", "http://etcd2:2379"]
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_file(temp_file.path()).unwrap();

        assert!(matches!(config.server.bind, Bind::Tcp { port: 9090 }));
        assert_eq!(
            config.git.remote_url,
            "https://git.example.com/testuser/testrepo.git"
        );
        assert_eq!(
            config.etcd.endpoints,
            vec!["http://etcd1:2379", "http://etcd2:2379"]
        );
        assert_eq!(config.etcd.prefix, "/ogygia/nixos/versions");
        assert_eq!(config.title, "Status Page");
        assert!(config.hostname_strip_suffix.is_none());
        assert!(config.git.ssh.is_none());
        assert!(config.git.archive.is_none());
    }

    #[test]
    fn test_load_ssh_and_archive_config() {
        let toml_content = r#"
[server]
port = 9090

[git]
remote_url = "https://git.example.com/testuser/testrepo.git"

[git.ssh]
url = "git@git.example.com:testuser/testrepo.git"
key_path = "/run/credentials/key"

[git.archive]

[etcd]
endpoints = ["http://etcd1:2379"]
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_file(temp_file.path()).unwrap();

        let ssh = config.git.ssh.unwrap();
        assert_eq!(ssh.url, "git@git.example.com:testuser/testrepo.git");
        assert_eq!(ssh.key_path, PathBuf::from("/run/credentials/key"));
        assert!(ssh.host_key.is_none());

        let archive = config.git.archive.unwrap();
        assert_eq!(archive.branch, "ogygia/deployed-commits-archive");
    }

    #[test]
    fn test_validation_archive_requires_ssh() {
        let mut config = test_config();
        config.git.archive = Some(ArchiveConfig {
            branch: default_archive_branch(),
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_load_from_toml_unix() {
        let toml_content = r#"
title = "My Dashboard"
hostname_strip_suffix = ".example.com"

[server]
socket = "/tmp/test.sock"

[git]
remote_url = "https://git.example.com/testuser/testrepo.git"

[etcd]
endpoints = ["http://etcd1:2379", "http://etcd2:2379"]
prefix = "/custom/prefix"
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(toml_content.as_bytes()).unwrap();

        let config = Config::load_from_file(temp_file.path()).unwrap();

        assert!(matches!(config.server.bind, Bind::Unix { socket } if socket == "/tmp/test.sock"));
        assert_eq!(config.title, "My Dashboard");
        assert_eq!(
            config.hostname_strip_suffix,
            Some(".example.com".to_string())
        );
    }

    #[test]
    fn test_override_server_config() {
        // Test socket override (takes precedence)
        let mut config = test_config();
        config.override_server_config(Some(3000), Some("/tmp/override.sock".to_string()));
        assert!(
            matches!(config.server.bind, Bind::Unix { socket } if socket == "/tmp/override.sock")
        );

        // Test port override only
        let mut config = test_config();
        config.override_server_config(Some(3000), None);
        assert!(matches!(config.server.bind, Bind::Tcp { port: 3000 }));

        // Test no override
        let mut config = test_config();
        config.override_server_config(None, None);
        assert!(matches!(config.server.bind, Bind::Tcp { port: 8080 }));
    }

    #[test]
    fn test_url_generation() {
        let config = test_config();

        assert_eq!(
            config.git_repo_url(),
            "https://git.example.com/user/repo.git"
        );
        assert_eq!(config.repo_web_url(), "https://git.example.com/user/repo");
        assert_eq!(
            config.pulls_web_url(),
            "https://git.example.com/user/repo/pulls"
        );
        assert_eq!(
            config.commit_web_url("abc123"),
            "https://git.example.com/user/repo/commit/abc123"
        );
        assert_eq!(
            config.pulls_api_url(),
            "https://git.example.com/api/v1/repos/user/repo/pulls?state=open&limit=1"
        );
    }

    #[test]
    fn test_validation_empty_remote_url() {
        let mut config = test_config();
        config.git.remote_url = "".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validation_empty_etcd_endpoints() {
        let mut config = test_config();
        config.etcd.endpoints = vec![];
        assert!(config.validate().is_err());
    }
}
