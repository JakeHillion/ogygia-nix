//! Helpers for extracting per-host Nebula state from the flake via `nix eval`.
//!
//! Reads `<output>.<host>.config.ogygia.nebula.{enable,spec,specHash}` for
//! every host in `nixosConfigurations` and every device in
//! `ogygiaConfigurations`, without ever importing the host certificate (which may be missing on a fresh
//! checkout — that's exactly when rekey runs). The `nix eval` mechanics live in
//! [`ogygia_nixutils::Nix`]; this module only owns the `ogygia.nebula` schema.

use anyhow::Result;
use anyhow::anyhow;
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

/// The flake output a host is defined in. Both evaluate the same
/// `ogygia.nebula` options under `config`, so only the output name differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// A NixOS host, which runs Nebula through services.nebula.
    Nixos,
    /// A device with no system configuration, defined with
    /// `mkOgygiaConfiguration`, which runs Nebula itself.
    Device,
}

impl Output {
    fn attr(self) -> &'static str {
        match self {
            Output::Nixos => "nixosConfigurations",
            Output::Device => "ogygiaConfigurations",
        }
    }
}

/// A host to evaluate: its attribute name and the output it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub output: Output,
    pub name: String,
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

/// List every host in the flake: NixOS hosts first, then devices. A flake
/// need not define either output.
pub async fn list_hosts(nix: &Nix, flake_ref: &str) -> Result<Vec<Host>> {
    #[derive(Deserialize)]
    struct Raw {
        nixos: Vec<String>,
        devices: Vec<String>,
    }

    // `#.` selects the flake's outputs themselves, so one evaluation can
    // tolerate a missing output.
    let raw: Raw = nix
        .eval_json(
            &format!("{flake_ref}#."),
            Some(
                "o: { nixos = builtins.attrNames (o.nixosConfigurations or { }); \
                 devices = builtins.attrNames (o.ogygiaConfigurations or { }); }",
            ),
        )
        .await?;

    let nixos = raw.nixos.into_iter().map(|name| Host {
        output: Output::Nixos,
        name,
    });
    let devices = raw.devices.into_iter().map(|name| Host {
        output: Output::Device,
        name,
    });
    Ok(nixos.chain(devices).collect())
}

/// Find `name` among the flake's hosts and devices.
pub async fn find_host(nix: &Nix, flake_ref: &str, name: &str) -> Result<Host> {
    let mut matches = list_hosts(nix, flake_ref)
        .await?
        .into_iter()
        .filter(|h| h.name == name);
    match (matches.next(), matches.next()) {
        (Some(host), None) => Ok(host),
        (Some(_), Some(_)) => Err(anyhow!(
            "{name} is defined in both nixosConfigurations and ogygiaConfigurations"
        )),
        (None, _) => Err(anyhow!(
            "{name} is in neither nixosConfigurations nor ogygiaConfigurations"
        )),
    }
}

/// Evaluate `ogygia.nebula.{enable,specHash,spec}` for a single host.
///
/// Selects only those three attributes so evaluation never touches
/// `certPath` — importing a missing cert is exactly what rekey exists to
/// resolve. The host name is quoted as a single attribute-path component,
/// so FQDN attribute names containing dots work.
pub async fn host_info(nix: &Nix, flake_ref: &str, host: &Host) -> Result<HostInfo> {
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
            &format!(
                "{flake_ref}#{}.\"{}\".config.ogygia.nebula",
                host.output.attr(),
                host.name
            ),
            Some("cfg: { inherit (cfg) enable specHash spec; validitySecs = cfg.validitySecs or null; }"),
        )
        .await?;

    Ok(HostInfo {
        host: host.name.clone(),
        enabled: raw.enable,
        spec_hash: raw.spec_hash,
        spec: raw.spec,
        validity_secs: raw.validity_secs.unwrap_or(90 * 86400),
    })
}
