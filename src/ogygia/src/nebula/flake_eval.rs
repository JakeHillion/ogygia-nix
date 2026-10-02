//! Extracting per-host Nebula state from the flake.
//!
//! Reads `nixosConfigurations.<host>.config.ogygia.nebula.{enable,spec,specHash}`
//! without ever importing the host certificate (which may be missing on a fresh
//! checkout — that's exactly when rekey runs). Evaluation is done in-process
//! by [`ogygia_nix_eval`], or by `nix eval` through [`ogygia_nixutils::Nix`];
//! both produce the same JSON, and this module only owns the
//! `ogygia.nebula` schema.

use anyhow::Context;
use anyhow::Result;
use clap::ValueEnum;
use ogygia_nixutils::Nix;
use serde::Deserialize;

/// Which Nix evaluator answers flake queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Evaluator {
    /// The in-process evaluator; needs no Nix installation and fetches
    /// inputs missing from the store itself.
    #[default]
    Builtin,
    /// `nix eval`.
    Nix,
}

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

/// Selects only the attributes rekey needs so evaluation never touches
/// `certPath`: importing a missing cert is exactly what rekey exists to
/// resolve.
const NEBULA_APPLY: &str =
    "cfg: { inherit (cfg) enable specHash spec; validitySecs = cfg.validitySecs or null; }";

/// The attribute path of a host's nebula config. The host name is quoted as
/// a single attribute-path component, so FQDN attribute names containing
/// dots work.
fn nebula_attr(host: &str) -> String {
    format!("nixosConfigurations.\"{host}\".config.ogygia.nebula")
}

fn host_info(host: &str, raw: Raw) -> HostInfo {
    HostInfo {
        host: host.to_string(),
        enabled: raw.enable,
        spec_hash: raw.spec_hash,
        spec: raw.spec,
        validity_secs: raw.validity_secs.unwrap_or(90 * 86400),
    }
}

/// Evaluate the nebula state of `host`, or of every host in
/// `flake_ref#nixosConfigurations` if `host` is `None`.
pub async fn hosts(
    evaluator: Evaluator,
    flake_ref: &str,
    host: Option<&str>,
) -> Result<Vec<HostInfo>> {
    match evaluator {
        Evaluator::Builtin => {
            let flake_ref = flake_ref.to_owned();
            let host = host.map(str::to_owned);
            tokio::task::spawn_blocking(move || builtin_hosts(&flake_ref, host.as_deref()))
                .await
                .context("flake evaluation panicked")?
        }
        Evaluator::Nix => nix_hosts(flake_ref, host).await,
    }
}

fn builtin_hosts(flake_ref: &str, host: Option<&str>) -> Result<Vec<HostInfo>> {
    ogygia_nix_eval::with_flake(flake_ref, |session| -> Result<Vec<HostInfo>> {
        let targets: Vec<String> = match host {
            Some(h) => vec![h.to_owned()],
            None => serde_json::from_value(
                session.eval_json("nixosConfigurations", Some("builtins.attrNames"))?,
            )?,
        };
        targets
            .iter()
            .map(|h| {
                let raw: Raw = serde_json::from_value(
                    session
                        .eval_json(&nebula_attr(h), Some(NEBULA_APPLY))
                        .with_context(|| format!("evaluating {h}"))?,
                )?;
                Ok(host_info(h, raw))
            })
            .collect()
    })?
}

async fn nix_hosts(flake_ref: &str, host: Option<&str>) -> Result<Vec<HostInfo>> {
    let nix = Nix::default();
    let targets: Vec<String> = match host {
        Some(h) => vec![h.to_owned()],
        None => {
            nix.eval_json(
                &format!("{flake_ref}#nixosConfigurations"),
                Some("builtins.attrNames"),
            )
            .await?
        }
    };
    let mut out = Vec::with_capacity(targets.len());
    for h in &targets {
        let raw: Raw = nix
            .eval_json(
                &format!("{flake_ref}#{}", nebula_attr(h)),
                Some(NEBULA_APPLY),
            )
            .await?;
        out.push(host_info(h, raw));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flake shaped like a NixOS configuration as far as rekey looks.
    const FLAKE: &str = r#"{
      outputs = { self }: {
        nixosConfigurations = {
          "a.example.com".config.ogygia.nebula = {
            enable = true;
            spec = {
              name = "a.example.com";
              ipv4 = "10.0.0.1";
              subnet = "10.0.0.0/24";
              pubKey = "KEY";
              groups = [ "ssh" ];
              caFingerprint = "abc";
              version = 2;
            };
            specHash = builtins.substring 0 32 (builtins.hashString "sha256" "a");
            validitySecs = 3600;
            certPath = throw "rekey must not read certPath";
          };
          "b.example.com".config.ogygia.nebula = {
            enable = false;
            spec = null;
            specHash = null;
          };
        };
      };
    }"#;

    #[tokio::test]
    async fn builtin_evaluator_reads_every_host() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("flake.nix"), FLAKE).unwrap();
        let flake = dir.path().to_str().unwrap();

        let all = hosts(Evaluator::Builtin, flake, None).await.unwrap();
        assert_eq!(
            all.iter().map(|h| h.host.as_str()).collect::<Vec<_>>(),
            ["a.example.com", "b.example.com"]
        );
        let a = &all[0];
        assert!(a.enabled);
        assert_eq!(a.validity_secs, 3600);
        assert_eq!(a.spec.as_ref().unwrap().groups, ["ssh"]);
        assert_eq!(
            a.spec_hash.as_deref(),
            Some("ca978112ca1bbdcafac231b39a23dc4d")
        );
        let b = &all[1];
        assert!(!b.enabled);
        assert!(b.spec.is_none());
        // Missing validitySecs falls back to 90 days.
        assert_eq!(b.validity_secs, 90 * 86400);

        let one = hosts(Evaluator::Builtin, flake, Some("b.example.com"))
            .await
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].host, "b.example.com");
    }
}
