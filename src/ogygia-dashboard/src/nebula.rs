//! Nebula certificate-expiry alerts: a background producer feeding the generic
//! [`crate::alerts`] subsystem. Compiled only with the `nebula` feature.
//!
//! Two independent facts feed a host's expiry alert, and they come from
//! different points in history:
//!
//!   * the certificate's real `notAfter` — a property of the *deployed* commit,
//!     since that's the cert the host is actually pinned to. We `nix eval` the
//!     host's `ogygia.nebula.certPath` at that commit, then read the expiry out
//!     of the signed cert with `nebula-cert print`.
//!   * `validitySecs` — the *policy* the alert thresholds scale against. That's
//!     a property of *now*, so it's evaluated on the main tip; changing it takes
//!     effect immediately rather than waiting for every host to redeploy.
//!
//! Two cache layers keep work off the request path (see [`spawn`]):
//!
//!   * **Layer A** — the per-host expiry snapshot ([`HostExpiry`]). Expensive
//!     (`nix eval` + `nebula-cert`), so it is recomputed only when the etcd
//!     host-state version changes, and every eval is memoized.
//!   * **Layer B** — the [`Alert`] list. Cheap: it derives severity from Layer A
//!     and the wall clock, so it is recomputed on a timer too, letting a cert
//!     cross a threshold without any input change.
//!
//! Everything degrades gracefully: one un-evaluable historical commit surfaces
//! as an informational alert rather than blanking the whole section.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Utc;
use git2::Oid;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::alerts::Alert;
use crate::alerts::AlertLevel;
use crate::alerts::AlertsSnapshot;
use crate::config::Config;
use crate::etcd::Etcd;
use crate::etcd::HostStates;
use crate::git::GitManager;
use crate::nixos::CommitState;

/// An operator's runway before a cert expires is measured as a fraction of the
/// cert's configured validity, so the windows scale with the policy.
const INFO_FRACTION: f64 = 0.50;
const WARNING_FRACTION: f64 = 0.25;

/// How often Layer B is recomputed so threshold crossings surface without an
/// input change. Day-scale thresholds don't need finer granularity.
const TICK: Duration = Duration::from_secs(60);

/// Spawn the background task that keeps the alerts snapshot up to date. Runs one
/// pass immediately, then on every host-state change or timer tick.
pub fn spawn(
    config: Config,
    git: Arc<GitManager>,
    etcd: Arc<Etcd>,
    slot: Arc<Mutex<Arc<AlertsSnapshot>>>,
) {
    tokio::spawn(async move {
        let _ = config; // reserved for future per-fleet alert tuning
        let mut changes = etcd.subscribe();
        let mut memo = Memo::default();
        let mut layer_a: Vec<HostExpiry> = Vec::new();
        let mut resolved_version: Option<usize> = None;

        loop {
            let state = etcd.state().await;

            if resolved_version != Some(state.version) {
                match resolve_expiries(&git, &state, &mut memo).await {
                    Ok(expiries) => {
                        layer_a = expiries;
                        resolved_version = Some(state.version);
                    }
                    Err(e) => {
                        // Keep the previous snapshot and retry on the next tick.
                        tracing::error!("failed to resolve nebula cert expiries: {e:#}");
                    }
                }
            }

            let alerts = compute_alerts(&layer_a, Utc::now());
            *slot.lock().await = Arc::new(AlertsSnapshot { alerts });

            tokio::select! {
                r = changes.changed() => {
                    if r.is_err() {
                        return; // etcd dropped; nothing left to watch
                    }
                }
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

/// Per-host expiry facts (Layer A). `not_after`/`validity_secs` are `None` when
/// they couldn't be determined; `note` then explains why so Layer B can emit an
/// informational alert instead of silently dropping the host.
#[derive(Debug, Clone)]
struct HostExpiry {
    host: String,
    not_after: Option<DateTime<Utc>>,
    source: Option<CommitState>,
    validity_secs: Option<u64>,
    note: Option<String>,
}

/// Memoized eval results, persisted across version bumps so a change only
/// re-evaluates the hosts that actually moved. Only successes are cached;
/// failures are retried on the next recompute.
#[derive(Default)]
struct Memo {
    /// `(host, commit) -> cert store path` (`None` = nebula disabled there).
    cert_paths: HashMap<(String, Oid), Option<PathBuf>>,
    /// `cert store path -> notAfter`. Keyed on the cert's content, so a re-sign
    /// (new expiry) is a new path and re-parses.
    expiries: HashMap<PathBuf, DateTime<Utc>>,
    /// `(host, commit) -> validitySecs`.
    validities: HashMap<(String, Oid), u64>,
}

/// Layer A: resolve every host's soonest cert expiry and its validity policy.
async fn resolve_expiries(
    git: &GitManager,
    state: &HostStates,
    memo: &mut Memo,
) -> Result<Vec<HostExpiry>> {
    let repo = git
        .repo_path()
        .ok_or_else(|| anyhow!("git manager has no repository path"))?;

    // Fetch once if any referenced commit is missing locally, so Nix's `?rev=`
    // lookup can resolve it.
    let needed: Vec<Oid> = state
        .host_states
        .values()
        .flat_map(|s| [s[CommitState::Current], s[CommitState::NextBoot]])
        .flatten()
        .collect();
    if needed.iter().any(|oid| !git.has_commit(*oid)) {
        git.fetch_updates().await?;
    }

    let main_tip = git.get_main_tip().ok();

    let mut out = Vec::new();
    for (host, states) in &state.host_states {
        if let Some(expiry) = resolve_host(&repo, host, states, main_tip, memo).await {
            out.push(expiry);
        }
    }
    Ok(out)
}

/// Resolve one host, or `None` when nebula isn't in play for it (nothing to
/// alert on).
async fn resolve_host(
    repo: &Path,
    host: &str,
    states: &enum_map::EnumMap<CommitState, Option<Oid>>,
    main_tip: Option<Oid>,
    memo: &mut Memo,
) -> Option<HostExpiry> {
    // The certs the host is actually pinned to: its current and nextboot
    // commits, deduplicated (they're usually identical).
    let mut seen = HashSet::new();
    let mut pins = Vec::new();
    for source in [CommitState::Current, CommitState::NextBoot] {
        if let Some(oid) = states[source]
            && seen.insert(oid)
        {
            pins.push((source, oid));
        }
    }
    if pins.is_empty() {
        return None;
    }

    let mut earliest: Option<(DateTime<Utc>, CommitState, Oid)> = None;
    let mut relevant = false;
    let mut note: Option<String> = None;

    for (source, oid) in pins {
        match resolve_pin(repo, oid, host, memo).await {
            Ok(Some(not_after)) => {
                relevant = true;
                if earliest.is_none_or(|(e, _, _)| not_after < e) {
                    earliest = Some((not_after, source, oid));
                }
            }
            Ok(None) => {} // nebula disabled at this commit
            Err(e) => {
                relevant = true;
                tracing::warn!(%host, %oid, "nebula cert eval failed: {e:#}");
                note.get_or_insert_with(|| {
                    format!("could not evaluate certificate at commit {}", short(oid))
                });
            }
        }
    }

    if !relevant {
        return None; // not a nebula host on either pinned commit
    }

    // validitySecs comes from the main tip (current policy). Fall back to the
    // pinned commit that gave us the earliest expiry, then give up.
    let validity_secs = resolve_validity(
        repo,
        host,
        main_tip,
        earliest.map(|(_, _, oid)| oid),
        memo,
        &mut note,
    )
    .await;

    Some(HostExpiry {
        host: host.to_string(),
        not_after: earliest.map(|(na, _, _)| na),
        source: earliest.map(|(_, s, _)| s),
        validity_secs,
        note,
    })
}

/// Resolve a single `(host, commit)` cert to its expiry, memoizing both the
/// eval and the parse. `Ok(None)` = nebula disabled there.
async fn resolve_pin(
    repo: &Path,
    oid: Oid,
    host: &str,
    memo: &mut Memo,
) -> Result<Option<DateTime<Utc>>> {
    let key = (host.to_string(), oid);
    let cert_path = match memo.cert_paths.get(&key) {
        Some(cached) => cached.clone(),
        None => {
            let resolved = eval_cert_path(repo, oid, host).await?;
            memo.cert_paths.insert(key, resolved.clone());
            resolved
        }
    };

    let Some(cert_path) = cert_path else {
        return Ok(None);
    };

    if let Some(expiry) = memo.expiries.get(&cert_path) {
        return Ok(Some(*expiry));
    }
    let expiry = cert_not_after(&cert_path).await?;
    memo.expiries.insert(cert_path, expiry);
    Ok(Some(expiry))
}

/// Resolve a host's validity policy, preferring the main tip and falling back
/// to the deployed commit. Records a note (for a degraded alert) if neither
/// works.
async fn resolve_validity(
    repo: &Path,
    host: &str,
    main_tip: Option<Oid>,
    fallback: Option<Oid>,
    memo: &mut Memo,
    note: &mut Option<String>,
) -> Option<u64> {
    for oid in [main_tip, fallback].into_iter().flatten() {
        let key = (host.to_string(), oid);
        if let Some(v) = memo.validities.get(&key) {
            return Some(*v);
        }
        match eval_validity_secs(repo, oid, host).await {
            Ok(Some(v)) => {
                memo.validities.insert(key, v);
                return Some(v);
            }
            Ok(None) => {} // disabled here; try the fallback
            Err(e) => tracing::warn!(%host, %oid, "nebula validitySecs eval failed: {e:#}"),
        }
    }
    note.get_or_insert_with(|| "could not determine certificate validity policy".to_string());
    None
}

/// Layer B: turn per-host expiry facts into alerts at the current instant.
fn compute_alerts(expiries: &[HostExpiry], now: DateTime<Utc>) -> Vec<Alert> {
    let mut alerts: Vec<Alert> = expiries.iter().filter_map(|e| alert_for(e, now)).collect();
    // Most severe first.
    alerts.sort_by_key(|a| std::cmp::Reverse(a.level));
    alerts
}

fn alert_for(expiry: &HostExpiry, now: DateTime<Utc>) -> Option<Alert> {
    let (Some(not_after), Some(validity)) = (expiry.not_after, expiry.validity_secs) else {
        // Couldn't fully resolve — surface it rather than hide the host.
        return expiry.note.as_ref().map(|note| Alert {
            level: AlertLevel::Info,
            title: format!("Nebula cert for {} could not be evaluated", expiry.host),
            detail: note.clone(),
            hosts: vec![expiry.host.clone()],
        });
    };

    let remaining = (not_after - now).num_seconds();
    let source = expiry
        .source
        .map(|s| s.as_ref().to_owned())
        .unwrap_or_else(|| "deployed".to_owned());
    let when = format!(
        "{} ({})",
        crate::web::format_relative_date(not_after).0,
        not_after.format("%Y-%m-%d %H:%M:%S UTC")
    );
    let remediation = "Renew with `ogygia nebula rekey`, then deploy.";

    if remaining <= 0 {
        return Some(Alert {
            level: AlertLevel::Critical,
            title: format!("Nebula cert for {} has EXPIRED", expiry.host),
            detail: format!("Certificate ({source}) expired {when}. {remediation}"),
            hosts: vec![expiry.host.clone()],
        });
    }

    let fraction = remaining as f64 / validity as f64;
    let level = if fraction < WARNING_FRACTION {
        AlertLevel::Warning
    } else if fraction < INFO_FRACTION {
        AlertLevel::Info
    } else {
        return None; // healthy runway
    };

    Some(Alert {
        level,
        title: format!("Nebula cert for {} expires soon", expiry.host),
        detail: format!("Certificate ({source}) expires {when}. {remediation}"),
        hosts: vec![expiry.host.clone()],
    })
}

fn short(oid: Oid) -> String {
    oid.to_string()[..12].to_string()
}

// --- Nix / nebula-cert primitives -----------------------------------------

/// Locate the `nebula-cert` binary. A Nix build embeds the store path via
/// `OGYGIA_NEBULA_CERT_BIN` so the derivation carries the runtime dependency;
/// a plain `cargo build` falls back to `PATH` discovery.
fn nebula_cert_bin() -> &'static str {
    option_env!("OGYGIA_NEBULA_CERT_BIN").unwrap_or("nebula-cert")
}

/// The subset of `ogygia.nebula` we evaluate for a deployed commit.
#[derive(Debug, Deserialize)]
struct CertEval {
    enable: bool,
    #[serde(rename = "certPath")]
    cert_path: Option<String>,
}

/// The subset of `ogygia.nebula` we evaluate for the validity policy.
#[derive(Debug, Deserialize)]
struct ValidityEval {
    enable: bool,
    #[serde(rename = "validitySecs")]
    validity_secs: u64,
}

/// One entry of `nebula-cert print -json` output (a JSON array of certs); only
/// `details.notAfter` is of interest.
#[derive(Debug, Deserialize)]
struct CertPrint {
    details: CertDetails,
}

#[derive(Debug, Deserialize)]
struct CertDetails {
    #[serde(rename = "notAfter")]
    not_after: DateTime<Utc>,
}

/// The store path of a host's certificate at a commit, or `None` when the host
/// has nebula disabled or unconfigured there (no cert to track).
async fn eval_cert_path(repo: &Path, rev: Oid, host: &str) -> Result<Option<PathBuf>> {
    let eval: CertEval =
        nix_eval(repo, rev, host, "cfg: { inherit (cfg) enable certPath; }").await?;
    if !eval.enable {
        return Ok(None);
    }
    Ok(eval.cert_path.map(PathBuf::from))
}

/// A host's configured certificate validity period at `rev`, or `None` when the
/// host has nebula disabled there.
async fn eval_validity_secs(repo: &Path, rev: Oid, host: &str) -> Result<Option<u64>> {
    let eval: ValidityEval =
        nix_eval(repo, rev, host, "cfg: { inherit (cfg) enable validitySecs; }").await?;
    Ok(eval.enable.then_some(eval.validity_secs))
}

/// Read a signed Nebula certificate's expiry via `nebula-cert print -json`.
async fn cert_not_after(cert: &Path) -> Result<DateTime<Utc>> {
    let output = Command::new(nebula_cert_bin())
        .arg("print")
        .arg("-json")
        .arg("-path")
        .arg(cert)
        .output()
        .await
        .context("failed to spawn nebula-cert print")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "nebula-cert print {} failed: {}",
            cert.display(),
            stderr.trim()
        ));
    }

    let printed: Vec<CertPrint> = serde_json::from_slice(&output.stdout).with_context(|| {
        format!("failed to parse nebula-cert print output for {}", cert.display())
    })?;
    let first = printed.into_iter().next().ok_or_else(|| {
        anyhow!("nebula-cert print returned no certificates for {}", cert.display())
    })?;
    Ok(first.details.not_after)
}

/// `nix eval --json` of `ogygia.nebula` for one host at one commit, transformed
/// by `apply` and deserialized into `T`.
///
/// The commit is addressed as a `git+file://…?rev=` flake so no working-tree
/// checkout is needed; `allRefs=1` lets Nix find revs that are only reachable
/// via remote-tracking refs (e.g. archived deployed commits). The host name is
/// quoted as a single attribute-path component so FQDN attribute names work.
async fn nix_eval<T: DeserializeOwned>(
    repo: &Path,
    rev: Oid,
    host: &str,
    apply: &str,
) -> Result<T> {
    let installable = format!(
        "git+file://{repo}?rev={rev}&allRefs=1#nixosConfigurations.\"{host}\".config.ogygia.nebula",
        repo = repo.display(),
    );

    let output = Command::new("nix")
        .args(["eval", "--json"])
        .arg(&installable)
        .args(["--apply", apply])
        .output()
        .await
        .with_context(|| format!("failed to spawn nix eval for {installable}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nix eval {installable} failed: {}", stderr.trim()));
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse nix eval output for {installable}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_expiry(remaining_secs: i64, validity: u64) -> HostExpiry {
        HostExpiry {
            host: "host1.example.com".to_string(),
            not_after: Some(Utc::now() + chrono::Duration::seconds(remaining_secs)),
            source: Some(CommitState::Current),
            validity_secs: Some(validity),
            note: None,
        }
    }

    const VALIDITY: u64 = 90 * 86400;

    #[test]
    fn healthy_cert_has_no_alert() {
        // 60% of a 90-day life remaining -> above the 50% info threshold.
        let e = host_expiry((VALIDITY as i64) * 60 / 100, VALIDITY);
        assert!(alert_for(&e, Utc::now()).is_none());
    }

    #[test]
    fn below_half_is_info() {
        let e = host_expiry((VALIDITY as i64) * 40 / 100, VALIDITY);
        assert_eq!(alert_for(&e, Utc::now()).unwrap().level, AlertLevel::Info);
    }

    #[test]
    fn below_quarter_is_warning() {
        let e = host_expiry((VALIDITY as i64) * 20 / 100, VALIDITY);
        assert_eq!(alert_for(&e, Utc::now()).unwrap().level, AlertLevel::Warning);
    }

    #[test]
    fn expired_is_critical() {
        let e = host_expiry(-1, VALIDITY);
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Critical);
        assert!(alert.title.contains("EXPIRED"));
    }

    #[test]
    fn unresolved_with_note_is_info_degradation() {
        let e = HostExpiry {
            host: "host1.example.com".to_string(),
            not_after: None,
            source: None,
            validity_secs: None,
            note: Some("could not evaluate".to_string()),
        };
        assert_eq!(alert_for(&e, Utc::now()).unwrap().level, AlertLevel::Info);
    }

    #[test]
    fn unresolved_without_note_is_silent() {
        let e = HostExpiry {
            host: "host1.example.com".to_string(),
            not_after: None,
            source: None,
            validity_secs: None,
            note: None,
        };
        assert!(alert_for(&e, Utc::now()).is_none());
    }

    #[test]
    fn alerts_are_sorted_most_severe_first() {
        let expiries = vec![
            host_expiry((VALIDITY as i64) * 40 / 100, VALIDITY), // info
            host_expiry(-1, VALIDITY),                           // critical
            host_expiry((VALIDITY as i64) * 20 / 100, VALIDITY), // warning
        ];
        let alerts = compute_alerts(&expiries, Utc::now());
        let levels: Vec<_> = alerts.iter().map(|a| a.level).collect();
        assert_eq!(
            levels,
            vec![AlertLevel::Critical, AlertLevel::Warning, AlertLevel::Info]
        );
    }
}
