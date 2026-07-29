//! `ogygia nebula rekey` — sign missing host certificates by walking the flake.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use clap::Args;
use ogygia_nixutils::Nix;

use super::config::Config;
use super::flake_eval;
use super::nebula_cert;
use super::nebula_cert::Cert;
use super::nebula_cert::SignArgs;

/// Re-sign a tracked cert once it is this fraction into its lifetime
/// (LetsEncrypt-style time-based rotation).
///
/// Deliberately aggressive: we renew at 1/3 elapsed (day 30 of a 90-day cert)
/// to stay well clear of expiry. Real Let's Encrypt only renews at ~2/3
/// elapsed; once expiry monitoring proves this reliable we may relax toward
/// 2.0/3.0.
const RENEW_AFTER_FRACTION: f64 = 1.0 / 3.0;

#[derive(Args)]
pub struct RekeyArgs {
    /// Process every host in the flake.
    #[arg(short = 'a', long)]
    pub all: bool,
    /// Process a single host.
    #[arg(long)]
    pub host: Option<String>,
    /// Re-sign even if a matching cert already exists.
    #[arg(long)]
    pub force: bool,
    /// Show what would happen without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Flake reference; defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub flake: String,
}

pub fn run(args: &RekeyArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(async_run(args))
}

async fn async_run(args: &RekeyArgs) -> Result<()> {
    if !args.all && args.host.is_none() {
        return Err(anyhow!("specify -a/--all or --host <name>"));
    }
    if args.all && args.host.is_some() {
        return Err(anyhow!("-a and --host are mutually exclusive"));
    }

    let config = Config::load()?;
    let ca_key = if !args.dry_run {
        Some(config.ca_key_path()?)
    } else {
        None
    };

    let nix = Nix::default();

    let targets: Vec<String> = if let Some(host) = &args.host {
        vec![host.clone()]
    } else {
        flake_eval::list_hosts(&nix, &args.flake).await?
    };

    let on_disk = read_cert_dir(&config.cert_dir).await?;
    let now = Utc::now();

    let mut current_cert_paths: HashSet<PathBuf> = HashSet::new();
    let mut signed = 0usize;
    let mut skipped = 0usize;

    for host in &targets {
        let info = flake_eval::host_info(&nix, &args.flake, host).await?;
        if !info.enabled {
            tracing::debug!(%host, "skipping (nebula disabled)");
            skipped += 1;
            continue;
        }
        let (spec, spec_hash) = match (info.spec.as_ref(), info.spec_hash.as_ref()) {
            (Some(s), Some(h)) => (s, h),
            _ => {
                tracing::warn!(%host, "nebula.spec is null (pubKey not set?); skipping");
                skipped += 1;
                continue;
            }
        };

        let cert_path = config.cert_path(spec_hash);
        current_cert_paths.insert(cert_path.clone());

        // What the new cert is replacing: the cert filed under this spec when
        // one exists, otherwise the last cert signed for this host under a
        // different spec — the filename is the spec hash, so any spec change
        // moves the host to a new file.
        let previous = on_disk
            .get(&cert_path)
            .or_else(|| latest_signed_for(&on_disk, &info.host));

        // A missing cert must be signed; a content-correct one is re-signed only
        // when forced or once it crosses the renewal threshold. Because the
        // filename is the spec hash, an existing file already matches the config
        // — only its remaining lifetime is in question.
        let reason = if !cert_path.exists() {
            if previous.is_some() {
                "spec changed"
            } else {
                "new host"
            }
        } else if args.force {
            "forced"
        } else if let Some(cert) = on_disk.get(&cert_path) {
            if cert.past_renewal(now, info.validity_secs, RENEW_AFTER_FRACTION) {
                "nearing expiry"
            } else {
                tracing::info!(%host, hash = %spec_hash, "cert up to date");
                skipped += 1;
                continue;
            }
        } else {
            // Fail safe: an unreadable cert is left untouched. Blindly
            // re-signing would let a read bug rotate the whole fleet, and a
            // genuinely corrupt cert is a job for the operator.
            tracing::warn!(%host, "could not read existing cert; skipping");
            skipped += 1;
            continue;
        };

        // Printed before signing so the operator sees the change ahead of the
        // CA passphrase prompt that `nebula-cert sign` puts on the terminal.
        let delta = Delta {
            host: &info.host,
            reason,
            previous,
            groups: &spec.groups,
            not_after: now + TimeDelta::seconds(info.validity_secs as i64),
        };
        for line in delta.lines(args.dry_run) {
            println!("{line}");
        }

        if args.dry_run {
            continue;
        }

        let parent = cert_path
            .parent()
            .ok_or_else(|| anyhow!("invalid cert path"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let pub_tempfile = nebula_cert::pem_to_tempfile(&spec.pub_key)?;
        let networks = nebula_cert::network_spec(&spec.ipv4, &spec.subnet)?;

        sign_with_atomic_rename(
            &cert_path,
            SignArgs {
                ca_cert: &config.ca_cert_path,
                ca_key: ca_key.as_deref().expect("checked above"),
                in_pub: pub_tempfile.path(),
                name: &info.host,
                networks: &networks,
                groups: &spec.groups,
                duration_seconds: info.validity_secs,
                out_cert: &cert_path,
            },
        )
        .await?;

        println!("signed {}", cert_path.display());
        signed += 1;
    }

    let pruned = if args.all && !args.dry_run {
        prune_orphans(&config.cert_dir, &current_cert_paths)?
    } else {
        0
    };

    eprintln!("rekey: {signed} signed, {skipped} unchanged, {pruned} orphans pruned");
    Ok(())
}

/// What a rekey is about to change for one host.
struct Delta<'a> {
    /// The `nixosConfigurations` attribute name, which is also the cert name.
    host: &'a str,
    reason: &'static str,
    /// The cert being replaced, absent for a host signed for the first time.
    previous: Option<&'a Cert>,
    groups: &'a [String],
    not_after: DateTime<Utc>,
}

impl Delta<'_> {
    /// The summary to show the operator, one line per element.
    fn lines(&self, dry_run: bool) -> Vec<String> {
        let verb = if dry_run { "would rekey" } else { "rekey" };
        let mut lines = vec![format!("{verb} {} ({})", self.host, self.reason)];

        let was: &[String] = self.previous.map_or(&[], |c| &c.groups);
        let added = only_in(self.groups, was);
        let removed = only_in(was, self.groups);
        if !added.is_empty() {
            lines.push(format!("  + {}", added.join(" ")));
        }
        if !removed.is_empty() {
            lines.push(format!("  - {}", removed.join(" ")));
        }

        // Re-signing a spec change usually lands on the same expiry day it
        // would have anyway, so only spell out a move that actually happened.
        let expiry = date(self.not_after);
        lines.push(match self.previous.map(|prev| date(prev.not_after)) {
            Some(before) if before != expiry => format!("  expires {before} → {expiry}"),
            _ => format!("  expires {expiry}"),
        });
        lines
    }
}

fn only_in<'a>(groups: &'a [String], other: &[String]) -> Vec<&'a str> {
    groups
        .iter()
        .filter(|g| !other.contains(g))
        .map(String::as_str)
        .collect()
}

fn date(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%d").to_string()
}

/// Every readable host certificate in `cert_dir`, keyed by path.
///
/// A file that is present but unreadable is omitted and warned about, so a
/// caller must treat "on disk but absent here" as corrupt rather than missing.
async fn read_cert_dir(cert_dir: &Path) -> Result<HashMap<PathBuf, Cert>> {
    let mut certs = HashMap::new();
    if !cert_dir.exists() {
        return Ok(certs);
    }
    for entry in std::fs::read_dir(cert_dir)
        .with_context(|| format!("failed to read {}", cert_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("crt") {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("ca.crt") {
            continue;
        }
        match nebula_cert::read_cert(&path).await {
            Ok(cert) => {
                certs.insert(path, cert);
            }
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "could not read cert"),
        }
    }
    Ok(certs)
}

/// The longest-lived cert signed for `host`, which is the one it is currently
/// running if its spec has moved on since.
fn latest_signed_for<'a>(certs: &'a HashMap<PathBuf, Cert>, host: &str) -> Option<&'a Cert> {
    certs
        .values()
        .filter(|c| c.name == host)
        .max_by_key(|c| c.not_after)
}

async fn sign_with_atomic_rename(final_path: &Path, args: SignArgs<'_>) -> Result<()> {
    let tmp = tempfile::Builder::new()
        .prefix(".ogygia-nebula-sign-")
        .suffix(".crt")
        .tempfile_in(final_path.parent().unwrap())
        .context("failed to create signing tempfile")?;
    let tmp_path = tmp.path().to_path_buf();
    drop(tmp); // close before nebula-cert writes

    let sign_args = SignArgs {
        out_cert: &tmp_path,
        ..args
    };
    nebula_cert::sign(sign_args).await?;
    std::fs::rename(&tmp_path, final_path).with_context(|| {
        format!(
            "failed to rename {} -> {}",
            tmp_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

fn prune_orphans(cert_dir: &Path, keep: &HashSet<PathBuf>) -> Result<usize> {
    let mut pruned = 0usize;
    if !cert_dir.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(cert_dir)
        .with_context(|| format!("failed to read {}", cert_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("crt") {
            continue;
        }
        // Never prune the CA cert.
        if path.file_name().and_then(|n| n.to_str()) == Some("ca.crt") {
            continue;
        }
        if keep.contains(&path) {
            continue;
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove orphan {}", path.display()))?;
        tracing::info!(path = %path.display(), "pruned orphan cert");
        pruned += 1;
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn groups(names: &[&str]) -> Vec<String> {
        names.iter().map(|g| g.to_string()).collect()
    }

    fn previous(names: &[&str], not_after: i64) -> Cert {
        Cert {
            name: "willow".to_string(),
            groups: groups(names),
            not_after: at(not_after),
        }
    }

    #[test]
    fn a_new_host_has_no_previous_expiry() {
        let delta = Delta {
            host: "willow",
            reason: "new host",
            previous: None,
            groups: &groups(&["servers", "monitoring"]),
            not_after: at(90 * 86400),
        };
        assert_eq!(
            delta.lines(false),
            [
                "rekey willow (new host)",
                "  + servers monitoring",
                "  expires 1970-04-01",
            ]
        );
    }

    #[test]
    fn group_changes_show_as_additions_and_removals() {
        let prev = previous(&["laptops", "servers"], 30 * 86400);
        let delta = Delta {
            host: "willow",
            reason: "spec changed",
            previous: Some(&prev),
            groups: &groups(&["servers", "monitoring"]),
            not_after: at(90 * 86400),
        };
        assert_eq!(
            delta.lines(false),
            [
                "rekey willow (spec changed)",
                "  + monitoring",
                "  - laptops",
                "  expires 1970-01-31 → 1970-04-01",
            ]
        );
    }

    #[test]
    fn an_unmoved_expiry_is_stated_once() {
        let prev = previous(&["laptops"], 90 * 86400);
        let delta = Delta {
            host: "willow",
            reason: "spec changed",
            previous: Some(&prev),
            groups: &groups(&["servers"]),
            not_after: at(90 * 86400),
        };
        assert_eq!(
            delta.lines(false),
            [
                "rekey willow (spec changed)",
                "  + servers",
                "  - laptops",
                "  expires 1970-04-01",
            ]
        );
    }

    #[test]
    fn an_extension_shows_only_the_expiry_move() {
        let prev = previous(&["servers"], 30 * 86400);
        let delta = Delta {
            host: "willow",
            reason: "nearing expiry",
            previous: Some(&prev),
            groups: &groups(&["servers"]),
            not_after: at(90 * 86400),
        };
        assert_eq!(
            delta.lines(false),
            [
                "rekey willow (nearing expiry)",
                "  expires 1970-01-31 → 1970-04-01",
            ]
        );
    }

    #[test]
    fn a_dry_run_is_phrased_as_a_proposal() {
        let delta = Delta {
            host: "willow",
            reason: "forced",
            previous: None,
            groups: &[],
            not_after: at(90 * 86400),
        };
        assert_eq!(
            delta.lines(true),
            ["would rekey willow (forced)", "  expires 1970-04-01"]
        );
    }

    #[test]
    fn the_newest_cert_wins_when_a_host_has_several() {
        let certs = HashMap::from([
            (PathBuf::from("old.crt"), previous(&["servers"], 30 * 86400)),
            (PathBuf::from("new.crt"), previous(&["laptops"], 60 * 86400)),
            (
                PathBuf::from("other.crt"),
                Cert {
                    name: "birch".to_string(),
                    groups: Vec::new(),
                    not_after: at(90 * 86400),
                },
            ),
        ]);
        let found = latest_signed_for(&certs, "willow").unwrap();
        assert_eq!(found.groups, ["laptops"]);
        assert!(latest_signed_for(&certs, "absent").is_none());
    }
}
