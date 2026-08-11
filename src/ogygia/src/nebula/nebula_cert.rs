//! Wrapper around the `nebula-cert` binary.
//!
//! Lazily discovers `nebula-cert` on PATH or in known NixOS fallback
//! locations and exposes async helpers for the subcommands ogygia needs:
//! `sign`, `keygen`, `ca`.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use tokio::process::Command;

const FALLBACKS: &[&str] = &[
    "/run/current-system/sw/bin/nebula-cert",
    "/nix/var/nix/profiles/default/bin/nebula-cert",
];

static BIN: OnceLock<&'static str> = OnceLock::new();

pub fn bin() -> &'static str {
    BIN.get_or_init(|| {
        // A Nix build embeds the store path of nebula-cert here so the
        // derivation carries a runtime dependency on nebula. Unset for plain
        // `cargo build` (e.g. the dev shell), which falls back to discovery.
        if let Some(path) = option_env!("OGYGIA_NEBULA_CERT_BIN") {
            tracing::debug!("using embedded nebula-cert path: {}", path);
            return path;
        }
        if which::which("nebula-cert").is_ok() {
            tracing::debug!("using nebula-cert from PATH");
            return "nebula-cert";
        }
        for path in FALLBACKS {
            if Path::new(path).exists() {
                tracing::debug!("using nebula-cert fallback: {}", path);
                return path;
            }
        }
        tracing::warn!("nebula-cert not found on PATH or in fallback locations");
        "nebula-cert"
    })
}

/// Inputs to `nebula-cert sign`.
pub struct SignArgs<'a> {
    pub ca_cert: &'a Path,
    pub ca_key: &'a Path,
    pub in_pub: &'a Path,
    pub name: &'a str,
    pub networks: &'a str,
    pub groups: &'a [String],
    pub duration_seconds: u64,
    pub out_cert: &'a Path,
}

/// Run `nebula-cert sign` and write the result to `out_cert`.
///
/// When the CA key is encrypted, `nebula-cert` prompts for the passphrase on
/// the controlling terminal, so stdin/stdout/stderr are inherited rather than
/// captured — otherwise the prompt would be swallowed and the read would hit a
/// closed stdin.
pub async fn sign(args: SignArgs<'_>) -> Result<()> {
    let duration = format!("{}s", args.duration_seconds);
    let groups = args.groups.join(",");

    let mut cmd = Command::new(bin());
    cmd.arg("sign");
    cmd.arg("-ca-key").arg(args.ca_key);
    cmd.arg("-ca-crt").arg(args.ca_cert);
    cmd.arg("-in-pub").arg(args.in_pub);
    cmd.arg("-name").arg(args.name);
    cmd.arg("-networks").arg(args.networks);
    if !groups.is_empty() {
        cmd.arg("-groups").arg(&groups);
    }
    cmd.arg("-duration").arg(&duration);
    cmd.arg("-out-crt").arg(args.out_cert);

    let status = cmd
        .status()
        .await
        .context("failed to spawn nebula-cert sign")?;

    if !status.success() {
        return Err(anyhow!("nebula-cert sign failed (see output above)"));
    }
    Ok(())
}

/// A signed certificate, as reported by `nebula-cert print`.
pub struct Cert {
    /// The name the certificate was signed with. `ogygia nebula rekey` signs
    /// with the `nixosConfigurations` attribute name, so this identifies the
    /// host a certificate on disk belongs to.
    pub name: String,
    pub groups: Vec<String>,
    pub not_after: DateTime<Utc>,
}

impl Cert {
    /// Whether the certificate is due for LetsEncrypt-style renewal: less than
    /// `1 - renew_after` of the configured `validity_secs` remains before the
    /// cert's actual expiry.
    ///
    /// Comparing remaining life against the configured `validity_secs` (how
    /// long a fresh cert is signed for) — rather than the cert's own signed
    /// window — means *extending* a host's validity renews an existing cert
    /// early (its remaining life is now a smaller fraction of the intended
    /// lifetime), while *shortening* it defers renewal. Because the trigger is
    /// remaining time to `not_after`, renewal always lands before real expiry;
    /// an expired cert is always due and a freshly signed one never is.
    pub fn past_renewal(&self, now: DateTime<Utc>, validity_secs: u64, renew_after: f64) -> bool {
        let remaining = (self.not_after - now).num_seconds();
        let renew_below = validity_secs as f64 * (1.0 - renew_after);
        remaining as f64 <= renew_below
    }
}

/// Read a signed certificate via `nebula-cert print -json`.
pub async fn read_cert(cert: &Path) -> Result<Cert> {
    let output = Command::new(bin())
        .arg("print")
        .arg("-json")
        .arg("-path")
        .arg(cert)
        .output()
        .await
        .context("failed to spawn nebula-cert print")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nebula-cert print failed: {}", stderr.trim()));
    }
    parse_cert(&output.stdout)
}

/// Extract the certificate from `nebula-cert print -json` output.
///
/// The command emits a JSON *array* of certificates (a file may hold a chain);
/// the host certificate is the first entry.
fn parse_cert(json: &[u8]) -> Result<Cert> {
    #[derive(Deserialize)]
    struct Printed {
        details: Details,
    }
    #[derive(Deserialize)]
    struct Details {
        name: String,
        /// `null`, not `[]`, for a certificate signed without groups.
        groups: Option<Vec<String>>,
        #[serde(rename = "notAfter")]
        not_after: DateTime<Utc>,
    }

    let printed: Vec<Printed> =
        serde_json::from_slice(json).context("failed to parse nebula-cert print JSON")?;
    let first = printed
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("nebula-cert print returned no certificates"))?;
    Ok(Cert {
        name: first.details.name,
        groups: first.details.groups.unwrap_or_default(),
        not_after: first.details.not_after,
    })
}

/// Compute the host-IP-plus-mask string passed to `nebula-cert sign -networks`,
/// e.g. ipv4 "10.42.0.1" + subnet "10.42.0.0/16" → "10.42.0.1/16".
pub fn network_spec(ipv4: &str, subnet: &str) -> Result<String> {
    let mask = subnet
        .split_once('/')
        .map(|(_, m)| m)
        .ok_or_else(|| anyhow!("subnet must be CIDR (got {subnet})"))?;
    Ok(format!("{ipv4}/{mask}"))
}

/// Materialise a PEM string into a tempfile, returning a handle that deletes
/// itself on drop. Used to pass `pubKey` content to `nebula-cert sign -in-pub`.
pub fn pem_to_tempfile(pem: &str) -> Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut tf = tempfile::Builder::new()
        .prefix("ogygia-nebula-pub-")
        .suffix(".pem")
        .tempfile()
        .context("failed to create tempfile for pubkey")?;
    tf.as_file_mut()
        .write_all(pem.as_bytes())
        .context("failed to write pubkey tempfile")?;
    if !pem.ends_with('\n') {
        tf.as_file_mut()
            .write_all(b"\n")
            .context("failed to write pubkey tempfile newline")?;
    }
    Ok(tf)
}

/// Resolve `nebula-cert ca` invocation: generate a new CA cert+key pair.
pub async fn create_ca(
    name: &str,
    out_cert: &Path,
    out_key: &Path,
    duration_seconds: u64,
) -> Result<()> {
    let duration = format!("{}s", duration_seconds);
    let mut cmd = Command::new(bin());
    cmd.arg("ca");
    cmd.arg("-name").arg(name);
    cmd.arg("-out-crt").arg(out_cert);
    cmd.arg("-out-key").arg(out_key);
    cmd.arg("-duration").arg(&duration);
    let output = cmd
        .output()
        .await
        .context("failed to spawn nebula-cert ca")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("nebula-cert ca failed: {}", stderr.trim()));
    }
    Ok(())
}

/// Default CA key path under the user's config home.
pub fn default_ca_key_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("ogygia").join("nebula").join("ca.key")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    /// A 90-day configured validity, matching the cert `window()` signs for.
    const VALIDITY_90D: u64 = 90 * 86400;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn cert_expiring_at(secs: i64) -> Cert {
        Cert {
            name: "host".to_string(),
            groups: Vec::new(),
            not_after: at(secs),
        }
    }

    /// A cert issued at t=0 and expiring at day 90.
    fn window() -> Cert {
        cert_expiring_at(90 * 86400)
    }

    /// End-to-end: mint a CA, generate a host keypair, sign a cert, then read
    /// it back — proving `read_cert`/`parse_cert` track the actual
    /// `nebula-cert print -json` output rather than an assumed shape.
    ///
    /// Hard-requires the real `nebula-cert`, provided on PATH by the dev shell
    /// and embedded into the CI nextest archive. A missing binary means a
    /// broken test environment and must fail loudly, not silently skip — a
    /// skipped round-trip is what let a parser bug reach the fleet before.
    #[tokio::test]
    async fn read_cert_round_trips_a_signed_cert() {
        let dir = tempfile::tempdir().unwrap();
        let ca_crt = dir.path().join("ca.crt");
        let ca_key = dir.path().join("ca.key");
        create_ca("round-trip-test", &ca_crt, &ca_key, 365 * 86400)
            .await
            .unwrap();

        let host_key = dir.path().join("host.key");
        let host_pub = dir.path().join("host.pub");
        let status = Command::new(bin())
            .arg("keygen")
            .arg("-out-key")
            .arg(&host_key)
            .arg("-out-pub")
            .arg(&host_pub)
            .status()
            .await
            .unwrap();
        assert!(status.success(), "nebula-cert keygen failed");

        let out_cert = dir.path().join("host.crt");
        let duration_secs: u64 = 90 * 86400;
        let signed_at = Utc::now();
        sign(SignArgs {
            ca_cert: &ca_crt,
            ca_key: &ca_key,
            in_pub: &host_pub,
            name: "host.round-trip.test",
            networks: "10.0.0.1/24",
            groups: &["servers".to_string(), "laptops".to_string()],
            duration_seconds: duration_secs,
            out_cert: &out_cert,
        })
        .await
        .unwrap();

        let v = read_cert(&out_cert).await.unwrap();

        assert_eq!(v.name, "host.round-trip.test");
        assert_eq!(v.groups, ["servers", "laptops"]);

        // nebula signs from ~now, so expiry lands one duration out (whole-second
        // rounding plus a few seconds of test slop).
        let expected_expiry = signed_at + chrono::Duration::seconds(duration_secs as i64);
        assert!(
            (v.not_after - expected_expiry).num_seconds().abs() <= 5,
            "unexpected expiry {}",
            v.not_after
        );

        // A freshly signed 90-day cert has ~full validity remaining, so it is
        // not due; it becomes due once under 2/3 (here 30 days) remain.
        assert!(!v.past_renewal(signed_at, duration_secs, 1.0 / 3.0));
        let near_expiry = v.not_after - chrono::Duration::seconds(30 * 86400);
        assert!(v.past_renewal(near_expiry, duration_secs, 1.0 / 3.0));
    }

    #[test]
    fn empty_cert_array_is_an_error() {
        assert!(parse_cert(b"[]").is_err());
    }

    #[test]
    fn null_groups_parse_as_empty() {
        let json =
            br#"[{"details":{"name":"bare","groups":null,"notAfter":"2026-10-27T18:28:48Z"}}]"#;
        assert!(parse_cert(json).unwrap().groups.is_empty());
    }

    #[test]
    fn fresh_cert_is_not_due() {
        // Day 10 of a 90-day policy is short of the 1/3 (day 30) threshold.
        assert!(!window().past_renewal(at(10 * 86400), VALIDITY_90D, 1.0 / 3.0));
    }

    #[test]
    fn past_threshold_is_due() {
        assert!(window().past_renewal(at(45 * 86400), VALIDITY_90D, 1.0 / 3.0));
    }

    #[test]
    fn expired_cert_is_due() {
        assert!(window().past_renewal(at(100 * 86400), VALIDITY_90D, 1.0 / 3.0));
    }

    #[test]
    fn not_yet_valid_cert_is_not_due() {
        // A clock behind the cert's issue time sees more than the full validity
        // remaining, so it must not trigger an immediate re-sign.
        assert!(!window().past_renewal(at(-3600), VALIDITY_90D, 1.0 / 3.0));
    }

    #[test]
    fn lengthening_validity_brings_renewal_forward() {
        // A 20-day-old 90-day cert (70 days left) is not due under its own
        // policy — 70 days is above the 60-day (2/3 of 90) window...
        assert!(!window().past_renewal(at(20 * 86400), VALIDITY_90D, 1.0 / 3.0));
        // ...but extending the policy to 365 days lifts the window to 243 days
        // (2/3 of 365), so 70 days remaining is now "nearing expiry" and it
        // renews immediately into a long-lived cert.
        assert!(window().past_renewal(at(20 * 86400), 365 * 86400, 1.0 / 3.0));
    }

    #[test]
    fn shortening_validity_defers_renewal() {
        // Shortening the policy to 30 days does not renew a cert with 70 days
        // left: it is well clear of the 20-day (2/3 of 30) window...
        assert!(!window().past_renewal(at(20 * 86400), 30 * 86400, 1.0 / 3.0));
        // ...and only renews once under 20 days remain, i.e. at day 71 of its
        // real 90-day window, never past the cert's actual expiry.
        assert!(window().past_renewal(at(71 * 86400), 30 * 86400, 1.0 / 3.0));
    }

    #[test]
    fn at_expiry_is_due() {
        // Zero remaining is at or below any positive renew window.
        assert!(cert_expiring_at(0).past_renewal(at(0), VALIDITY_90D, 1.0 / 3.0));
    }
}
