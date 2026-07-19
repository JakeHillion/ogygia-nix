//! Operator-side configuration for `ogygia nebula`.
//!
//! Fleet-wide settings resolve relative to the operator's current working
//! directory (the flake root): certificates live in `<cwd>/nebula`. The CA
//! key path comes from the environment (`OGYGIA_NEBULA_CA_KEY`), keeping the
//! key out of the repo.

use std::env;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;

pub const ENV_CA_KEY: &str = "OGYGIA_NEBULA_CA_KEY";

pub const DEFAULT_NETWORK_NAME: &str = "ogygia";
pub const DEFAULT_CA_DURATION_SECS: u64 = 10 * 365 * 86400;

/// Resolved fleet configuration.
#[derive(Debug)]
pub struct Config {
    /// Directory containing ca.crt and per-host certificates. Absolute.
    pub cert_dir: PathBuf,
    /// Absolute path to the CA certificate.
    pub ca_cert_path: PathBuf,
}

impl Config {
    /// Load configuration relative to the operator's current working directory.
    pub fn load() -> Result<Self> {
        let flake_root = env::current_dir().context("failed to read current directory")?;
        let cert_dir = flake_root.join("nebula");
        let ca_cert_path = cert_dir.join("ca.crt");
        Ok(Self {
            cert_dir,
            ca_cert_path,
        })
    }

    pub fn ca_key_path(&self) -> Result<PathBuf> {
        env::var(ENV_CA_KEY).map(PathBuf::from).map_err(|_| {
            anyhow!(
                "{} is not set; point it at the CA private key on this machine",
                ENV_CA_KEY
            )
        })
    }

    pub fn cert_path(&self, spec_hash: &str) -> PathBuf {
        self.cert_dir.join(format!("{spec_hash}.crt"))
    }
}
