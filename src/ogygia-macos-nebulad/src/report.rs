//! Turning tool output into what the menu bar shows.

use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;

/// The longest error the status carries; the menu shows far less.
const MAX_ERROR: usize = 2000;

/// The part of a failed `nix eval` worth showing: Nix prints the evaluation
/// trace first and the error that caused it last, so keep from the last
/// `error:` on.
pub fn nix_error(message: &str) -> String {
    let tail = match message.rfind("error:") {
        Some(at) => &message[at..],
        None => message,
    };
    let tail = tail.trim();
    match tail.char_indices().nth(MAX_ERROR) {
        Some((cut, _)) => format!("{}…", &tail[..cut]),
        None => tail.to_owned(),
    }
}

/// What the menu shows about this Mac's certificate.
#[derive(Debug, PartialEq, Eq)]
pub struct CertInfo {
    /// The mesh address, without its prefix length.
    pub address: Option<String>,
    pub not_after: DateTime<Utc>,
}

/// Read `nebula-cert print -json` output, whose host certificate is the
/// first entry of an array.
pub fn cert_info(json: &[u8]) -> Result<CertInfo> {
    #[derive(Deserialize)]
    struct Printed {
        details: Details,
    }
    #[derive(Deserialize)]
    struct Details {
        networks: Option<Vec<String>>,
        #[serde(rename = "notAfter")]
        not_after: DateTime<Utc>,
    }

    let printed: Vec<Printed> =
        serde_json::from_slice(json).context("unexpected nebula-cert print output")?;
    let details = printed
        .into_iter()
        .next()
        .context("nebula-cert printed no certificate")?
        .details;
    let address = details
        .networks
        .unwrap_or_default()
        .into_iter()
        .next()
        .map(|net| net.split('/').next().unwrap_or(&net).to_owned());
    Ok(CertInfo {
        address,
        not_after: details.not_after,
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn the_trace_is_dropped() {
        let stderr = "error:\n       … while evaluating the attribute 'settings'\n\n       error: mac has no certificate for its current spec (abc).\n       Run `ogygia nebula rekey --host mac` and commit the result.\n";
        assert_eq!(
            nix_error(stderr),
            "error: mac has no certificate for its current spec (abc).\n       Run `ogygia nebula rekey --host mac` and commit the result."
        );
    }

    #[test]
    fn messages_without_an_error_are_kept() {
        assert_eq!(nix_error("  could not spawn nix\n"), "could not spawn nix");
    }

    #[test]
    fn long_errors_are_cut() {
        let long = format!("error: {}", "x".repeat(5000));
        let cut = nix_error(&long);
        assert_eq!(cut.chars().count(), MAX_ERROR + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn the_address_and_expiry_are_read() {
        let json = br#"[{"curve":"CURVE25519","details":{"groups":["laptops"],"isCa":false,"name":"mac","networks":["10.99.0.20/24"],"notAfter":"2027-01-15T11:02:29Z","notBefore":"2026-10-17T12:02:29+01:00","unsafeNetworks":null},"version":2}]"#;
        assert_eq!(
            cert_info(json).unwrap(),
            CertInfo {
                address: Some("10.99.0.20".into()),
                not_after: Utc.with_ymd_and_hms(2027, 1, 15, 11, 2, 29).unwrap(),
            }
        );
    }
}
