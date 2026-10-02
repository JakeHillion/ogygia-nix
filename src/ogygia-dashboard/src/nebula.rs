//! Nebula certificate-expiry alerts: a background producer feeding the generic
//! [`crate::alerts`] subsystem. Compiled only with the `nebula` feature.
//!
//! Three independent facts feed a host's expiry alert, and they come from
//! different points in history:
//!
//!   * the certificate's real `notAfter` — a property of the *deployed* commit,
//!     since that's the cert the host is actually pinned to. We `nix eval` the
//!     host's `ogygia.nebula.certPath` at that commit, then read the expiry out
//!     of the signed cert with `nebula-cert print`.
//!   * the same `notAfter` for the cert on the main tip — what a deploy would
//!     give the host today. Comparing the two separates a cert the CA still
//!     has to sign from one the host has merely not picked up yet.
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
    /// The cert the main tip would deploy, when it could be evaluated and
    /// nebula is still enabled for the host there.
    tip: Option<TipCert>,
    note: Option<String>,
}

/// The certificate sitting in the repository right now, for one host.
#[derive(Debug, Clone, Copy)]
struct TipCert {
    oid: Oid,
    not_after: DateTime<Utc>,
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
                    format!(
                        "The cert at commit {} could not be evaluated. The dashboard log has \
                         the nix eval error.",
                        short(oid)
                    )
                });
            }
        }
    }

    if !relevant {
        return None; // not a nebula host on either pinned commit
    }

    // What deploying the main tip would give this host. A failure here only
    // costs us the ability to narrow the remediation, so it degrades to `None`
    // rather than a user-visible note.
    let tip = match main_tip {
        Some(oid) => match resolve_pin(repo, oid, host, memo).await {
            Ok(Some(not_after)) => Some(TipCert { oid, not_after }),
            Ok(None) => None, // nebula disabled at the tip
            Err(e) => {
                tracing::warn!(%host, %oid, "nebula tip cert eval failed: {e:#}");
                None
            }
        },
        None => None,
    };

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
        tip,
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
    note.get_or_insert_with(|| {
        "The cert validity policy could not be determined, so expiry cannot be judged. The \
         dashboard log has the nix eval error."
            .to_string()
    });
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
            title: format!("Nebula cert on {} could not be evaluated", expiry.host),
            detail: note.clone(),
            hosts: vec![expiry.host.clone()],
        });
    };

    let level = level_for(not_after, now, validity)?; // healthy runway -> silent
    let remedy = Remedy::classify(expiry.tip, now, validity);

    let source = expiry
        .source
        .map(|s| s.as_ref().to_owned())
        .unwrap_or_else(|| "deployed".to_owned());
    let expired = not_after <= now;
    let state = if expired {
        "has expired"
    } else {
        "expires soon"
    };

    // The title carries the call to action, so it survives into anything that
    // shows titles alone; the detail carries the dates and the command.
    let title = match remedy.call_to_action() {
        Some(cta) => format!("Nebula cert on {} {state}: {cta}", expiry.host),
        None => format!("Nebula cert on {} {state}", expiry.host),
    };

    let fact = format!(
        "The {source} generation's cert {verb} {at}.",
        verb = if expired { "expired" } else { "expires" },
        at = when(not_after),
    );
    let action = match remedy {
        Remedy::Rekey => format!(
            "No newer cert on main: run `ogygia nebula rekey --host {host}`, then commit and \
             deploy.",
            host = expiry.host,
        ),
        Remedy::Deploy(tip) => format!(
            "A newer cert on main expires {tip_when}. Deploy {commit}; no rekey needed.",
            tip_when = when(tip.not_after),
            commit = short(tip.oid),
        ),
        Remedy::Unknown => format!(
            "The cert on main could not be read. Check whether main is already rekeyed; if \
             not, run `ogygia nebula rekey --host {host}`.",
            host = expiry.host,
        ),
    };

    Some(Alert {
        level,
        title,
        detail: format!("{fact} {action}"),
        hosts: vec![expiry.host.clone()],
    })
}

/// What the operator actually has to do, derived by asking whether the cert in
/// the repository would raise an alert of its own.
#[derive(Clone, Copy)]
enum Remedy {
    /// The tip's cert is expiring too, usually because it is the same cert, so
    /// a deploy alone would fix nothing.
    Rekey,
    /// The tip's cert is healthy: the host only has to deploy it.
    Deploy(TipCert),
    /// The tip's cert could not be read, so advise both steps.
    Unknown,
}

impl Remedy {
    fn classify(tip: Option<TipCert>, now: DateTime<Utc>, validity: u64) -> Self {
        match tip {
            Some(tip) if level_for(tip.not_after, now, validity).is_none() => Remedy::Deploy(tip),
            Some(_) => Remedy::Rekey,
            None => Remedy::Unknown,
        }
    }

    fn call_to_action(self) -> Option<&'static str> {
        match self {
            Remedy::Rekey => Some("rekey needed"),
            Remedy::Deploy(_) => Some("deploy needed"),
            Remedy::Unknown => None,
        }
    }
}

/// The severity a certificate's remaining runway earns, or `None` when it has
/// enough life left to stay quiet.
fn level_for(not_after: DateTime<Utc>, now: DateTime<Utc>, validity: u64) -> Option<AlertLevel> {
    let remaining = (not_after - now).num_seconds();
    if remaining <= 0 {
        return Some(AlertLevel::Critical);
    }
    let fraction = remaining as f64 / validity as f64;
    if fraction < WARNING_FRACTION {
        Some(AlertLevel::Warning)
    } else if fraction < INFO_FRACTION {
        Some(AlertLevel::Info)
    } else {
        None
    }
}

fn when(at: DateTime<Utc>) -> String {
    let (relative, absolute) = crate::web::format_relative_date(at);
    format!("{relative} ({absolute})")
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
    let eval: ValidityEval = nix_eval(
        repo,
        rev,
        host,
        "cfg: { inherit (cfg) enable validitySecs; }",
    )
    .await?;
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
        format!(
            "failed to parse nebula-cert print output for {}",
            cert.display()
        )
    })?;
    let first = printed.into_iter().next().ok_or_else(|| {
        anyhow!(
            "nebula-cert print returned no certificates for {}",
            cert.display()
        )
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

    /// A host whose tip cert couldn't be read, so the remedy is unknown.
    fn host_expiry(remaining_secs: i64, validity: u64) -> HostExpiry {
        HostExpiry {
            host: "host1.example.com".to_string(),
            not_after: Some(Utc::now() + chrono::Duration::seconds(remaining_secs)),
            source: Some(CommitState::Current),
            validity_secs: Some(validity),
            tip: None,
            note: None,
        }
    }

    fn with_tip(mut expiry: HostExpiry, tip_remaining_secs: i64) -> HostExpiry {
        expiry.tip = Some(TipCert {
            oid: Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap(),
            not_after: Utc::now() + chrono::Duration::seconds(tip_remaining_secs),
        });
        expiry
    }

    const VALIDITY: u64 = 90 * 86400;
    const HEALTHY: i64 = (VALIDITY as i64) * 90 / 100;

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
        assert_eq!(
            alert_for(&e, Utc::now()).unwrap().level,
            AlertLevel::Warning
        );
    }

    #[test]
    fn expired_is_critical() {
        let e = host_expiry(-1, VALIDITY);
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Critical);
        assert!(alert.title.contains("has expired"));
    }

    #[test]
    fn renewed_tip_asks_for_a_deploy() {
        let e = with_tip(host_expiry((VALIDITY as i64) * 40 / 100, VALIDITY), HEALTHY);
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Info);
        assert!(alert.title.contains("expires soon: deploy needed"));
        assert!(alert.detail.contains("no rekey needed"));
        assert!(alert.detail.contains("Deploy 0123456789ab"));
    }

    #[test]
    fn the_remedy_does_not_change_severity() {
        // Whether a deploy is achievable depends on reachability the dashboard
        // cannot see: a host that pulls its config over the overlay is
        // stranded by an expired cert. Severity describes the cert.
        for remaining in [
            -1,
            (VALIDITY as i64) * 20 / 100,
            (VALIDITY as i64) * 40 / 100,
        ] {
            let base = host_expiry(remaining, VALIDITY);
            let level = |e: HostExpiry| alert_for(&e, Utc::now()).map(|a| a.level);
            let rekey = level(with_tip(base.clone(), remaining));
            let deploy = level(with_tip(base.clone(), HEALTHY));
            assert_eq!(rekey, deploy, "remaining={remaining}");
            assert_eq!(rekey, level(base), "remaining={remaining}");
        }
    }

    #[test]
    fn aging_tip_asks_for_a_rekey() {
        // The tip cert is itself inside the info window, so a deploy alone
        // wouldn't fix anything: the CA has to sign.
        let e = with_tip(
            host_expiry((VALIDITY as i64) * 20 / 100, VALIDITY),
            (VALIDITY as i64) * 40 / 100,
        );
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Warning);
        assert!(alert.title.contains("expires soon: rekey needed"));
        assert!(
            alert
                .detail
                .contains("ogygia nebula rekey --host host1.example.com")
        );
    }

    #[test]
    fn expired_with_renewed_tip_still_asks_for_a_deploy() {
        let e = with_tip(host_expiry(-1, VALIDITY), HEALTHY);
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Critical);
        assert!(alert.title.contains("has expired: deploy needed"));
        assert!(alert.detail.contains("cert expired"));
    }

    #[test]
    fn unknown_tip_advises_both_steps() {
        let e = host_expiry((VALIDITY as i64) * 20 / 100, VALIDITY);
        let alert = alert_for(&e, Utc::now()).unwrap();
        assert_eq!(alert.level, AlertLevel::Warning);
        assert!(alert.detail.contains("ogygia nebula rekey --host"));
        assert!(alert.detail.contains("The cert on main could not be read"));
    }

    #[test]
    fn healthy_tip_does_not_alert_on_its_own() {
        // A healthy deployed cert stays silent whatever the tip says.
        let e = with_tip(
            host_expiry((VALIDITY as i64) * 60 / 100, VALIDITY),
            (VALIDITY as i64) * 10 / 100,
        );
        assert!(alert_for(&e, Utc::now()).is_none());
    }

    #[test]
    fn unresolved_with_note_is_info_degradation() {
        let e = HostExpiry {
            host: "host1.example.com".to_string(),
            not_after: None,
            source: None,
            validity_secs: None,
            tip: None,
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
            tip: None,
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
