//! Helpers for extracting per-host Nebula state from the flake via `nix eval`.
//!
//! Reads `nixosConfigurations.<host>.config.ogygia.nebula.{enable,spec,specHash}`
//! without ever importing the host certificate (which may be missing on a fresh
//! checkout — that's exactly when rekey runs). The `nix eval` mechanics live in
//! [`ogygia_nixutils::Nix`]; this module only owns the `ogygia.nebula` schema.

use anyhow::Result;
use ogygia_nixutils::Nix;
use serde::Deserialize;

/// The spec recorded by the NixOS module. Mirrors `ogygia.nebula.spec`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct HostSpec {
    pub name: String,
    pub ipv4: String,
    pub subnet: String,
    #[serde(rename = "pubKey")]
    pub pub_key: String,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(rename = "caFingerprint")]
    pub ca_fingerprint: String,
    pub version: u32,
}

/// Per-host evaluation result. `spec` is `None` when the host has nebula
/// disabled or hasn't been bootstrapped yet.
#[derive(Debug)]
pub struct HostInfo {
    pub host: String,
    pub enabled: bool,
    pub spec_hash: Option<String>,
    pub spec: Option<HostSpec>,
    /// Validity period, in seconds, to sign this host's cert for.
    pub validity_secs: u64,
}

/// List the names of every host in `flake_ref#nixosConfigurations`.
pub async fn list_hosts(nix: &Nix, flake_ref: &str) -> Result<Vec<String>> {
    nix.eval_json(
        &format!("{flake_ref}#nixosConfigurations"),
        Some("builtins.attrNames"),
    )
    .await
}

/// Evaluate `ogygia.nebula.{enable,specHash,spec}` for a single host.
///
/// Selects only those three attributes so evaluation never touches
/// `certPath` — importing a missing cert is exactly what rekey exists to
/// resolve. The host name is quoted as a single attribute-path component,
/// so FQDN attribute names containing dots work.
pub async fn host_info(nix: &Nix, flake_ref: &str, host: &str) -> Result<HostInfo> {
    #[derive(Deserialize)]
    struct Raw {
        enable: bool,
        #[serde(rename = "specHash")]
        spec_hash: Option<String>,
        spec: Option<HostSpec>,
        // `or null` for flakes whose module predates validitySecs; drop the
        // fallback once every host has updated.
        #[serde(rename = "validitySecs")]
        validity_secs: Option<u64>,
    }

    let raw: Raw = nix
        .eval_json(
            &format!("{flake_ref}#nixosConfigurations.\"{host}\".config.ogygia.nebula"),
            Some("cfg: { inherit (cfg) enable specHash spec; validitySecs = cfg.validitySecs or null; }"),
        )
        .await?;

    Ok(HostInfo {
        host: host.to_string(),
        enabled: raw.enable,
        spec_hash: raw.spec_hash,
        spec: raw.spec,
        validity_secs: raw.validity_secs.unwrap_or(90 * 86400),
    })
}
