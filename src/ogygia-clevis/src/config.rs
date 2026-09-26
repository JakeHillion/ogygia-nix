use std::fmt;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use serde::Deserialize;

/// Configuration, deserialized from JSON.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The Clevis JWE to maintain. It must already exist and be
    /// decryptable from this host.
    pub secret_file: PathBuf,
    pub spec: Spec,
}

/// The `clevis encrypt sss` configuration to bind against.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// How many pins are needed to decrypt.
    pub t: usize,
    pub pins: Pins,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub tang: Vec<Pin>,
}

/// A Tang server, identified by its URL and the thumbprint of the signing
/// key its advertisement must be signed by. The thumbprint is mandatory:
/// without it clevis would trust whatever keys the URL serves, and whoever
/// holds those keys can decrypt the blob from its header alone.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub url: String,
    pub thp: String,
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.url, self.thp)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let config: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        config
            .spec
            .validate()
            .with_context(|| format!("invalid spec in {}", path.display()))?;
        Ok(config)
    }
}

impl Spec {
    fn validate(&self) -> Result<()> {
        ensure!(self.t >= 1, "threshold t must be at least 1");
        ensure!(
            self.t <= self.pins.tang.len(),
            "threshold t ({}) exceeds the number of tang pins ({})",
            self.t,
            self.pins.tang.len()
        );
        for (i, pin) in self.pins.tang.iter().enumerate() {
            ensure!(
                !self.pins.tang[..i].contains(pin),
                "duplicate tang pin {pin}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    fn load(json: &str) -> Result<Config> {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(json.as_bytes()).unwrap();
        Config::load(file.path())
    }

    const VALID: &str = r#"{
        "secret_file": "/data/disk_encryption.jwe",
        "spec": {
            "t": 1,
            "pins": {
                "tang": [
                    {"url": "http://tang1:7654", "thp": "H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE"},
                    {"url": "http://tang2:7654", "thp": "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms"}
                ]
            }
        }
    }"#;

    #[test]
    fn parses_spec() {
        let config = load(VALID).unwrap();
        assert_eq!(config.secret_file, Path::new("/data/disk_encryption.jwe"));
        assert_eq!(config.spec.t, 1);
        assert_eq!(config.spec.pins.tang.len(), 2);
        assert_eq!(config.spec.pins.tang[1].url, "http://tang2:7654");
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = load(&VALID.replacen("\"spec\"", "\"extra\": 1, \"spec\"", 1)).unwrap_err();
        assert!(format!("{err:#}").contains("extra"), "{err:#}");
    }

    #[test]
    fn rejects_pin_without_thumbprint() {
        let json = VALID.replace(
            r#", "thp": "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms""#,
            "",
        );
        let err = load(&json).unwrap_err();
        assert!(format!("{err:#}").contains("thp"), "{err:#}");
    }

    #[test]
    fn rejects_threshold_above_pin_count() {
        let err = load(&VALID.replace("\"t\": 1", "\"t\": 3")).unwrap_err();
        assert!(format!("{err:#}").contains("exceeds"), "{err:#}");
    }

    #[test]
    fn rejects_zero_threshold() {
        let err = load(&VALID.replace("\"t\": 1", "\"t\": 0")).unwrap_err();
        assert!(format!("{err:#}").contains("at least 1"), "{err:#}");
    }

    #[test]
    fn rejects_duplicate_pins() {
        let json = VALID
            .replace("http://tang2:7654", "http://tang1:7654")
            .replace(
                "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms",
                "H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE",
            );
        let err = load(&json).unwrap_err();
        assert!(format!("{err:#}").contains("duplicate"), "{err:#}");
    }
}
