//! Turning the evaluated `settings` into the file Nebula reads.

use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde_json::Value;

/// Nebula's config for this Mac: the evaluated settings with the path of the
/// key, which never leaves the Mac and so is never in the fleet repository.
///
/// The result is JSON, which Nebula reads as the YAML subset it is.
pub fn render(mut settings: Value, key: &Path) -> Result<Vec<u8>> {
    let Some(pki) = settings.get_mut("pki").and_then(Value::as_object_mut) else {
        bail!("the evaluated settings have no `pki` section");
    };
    if pki.contains_key("key") {
        bail!("the evaluated settings carry a key; a device's key must stay on the device");
    }
    let key = key.to_str().context("the key path is not UTF-8")?;
    pki.insert("key".into(), Value::String(key.into()));

    let mut out = serde_json::to_vec_pretty(&settings)?;
    out.push(b'\n');
    Ok(out)
}

/// The PEM certificate in a rendered config, for reporting this Mac's
/// address.
pub fn certificate(config: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(config).ok()?;
    value.get("pki")?.get("cert")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_key_path_is_added() {
        let settings = json!({ "pki": { "ca": "CA", "cert": "CERT" }, "listen": { "port": 0 } });
        let out = render(settings, Path::new("/var/lib/host.key")).unwrap();
        let out: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            out,
            json!({
                "pki": { "ca": "CA", "cert": "CERT", "key": "/var/lib/host.key" },
                "listen": { "port": 0 },
            })
        );
    }

    #[test]
    fn a_key_from_the_repository_is_refused() {
        let settings = json!({ "pki": { "ca": "CA", "cert": "CERT", "key": "SECRET" } });
        assert!(render(settings, Path::new("/k")).is_err());
    }

    #[test]
    fn settings_without_pki_are_refused() {
        assert!(render(json!({ "listen": {} }), Path::new("/k")).is_err());
        assert!(render(json!("nonsense"), Path::new("/k")).is_err());
    }

    #[test]
    fn the_certificate_is_read_back() {
        let out = render(
            json!({ "pki": { "ca": "CA", "cert": "CERT" } }),
            Path::new("/k"),
        )
        .unwrap();
        assert_eq!(certificate(&out).as_deref(), Some("CERT"));
    }
}
