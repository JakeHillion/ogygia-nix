//! Nebula certificate management and content-addressed storage.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

/// Host configuration that defines a Nebula certificate identity
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostConfig {
    pub fqdn: String,
    pub ip: String,
    pub groups: Vec<String>,
}

impl HostConfig {
    /// Compute the content-addressed hash for this host configuration
    /// The hash includes: public key fingerprint + IP + groups + FQDN
    /// This ensures the store path is stable across certificate rotations
    pub fn compute_identity_hash(&self, pub_key_fingerprint: &str) -> String {
        let mut hasher = Sha256::new();
        
        // Include all configuration that affects certificate identity
        hasher.update(pub_key_fingerprint.as_bytes());
        hasher.update(self.ip.as_bytes());
        hasher.update(self.fqdn.as_bytes());
        
        // Sort groups for consistent hashing
        let mut sorted_groups = self.groups.clone();
        sorted_groups.sort();
        for group in sorted_groups {
            hasher.update(group.as_bytes());
        }
        
        let result = hasher.finalize();
        hex::encode(&result[..16]) // Use first 16 bytes (32 hex chars) for readability
    }
}

/// Unix timestamp wrapper for JSON serialization
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Timestamp(#[serde(with = "serde_millis")] pub SystemTime);

mod serde_millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let millis = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        serializer.serialize_u64(millis)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }
}

/// Certificate metadata stored alongside the certificate file
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertMetadata {
    pub fqdn: String,
    pub ip: String,
    pub groups: Vec<String>,
    pub valid_until: Timestamp,
    pub issued_at: Timestamp,
    pub identity_hash: String,
    pub public_key_fingerprint: String,
}

impl CertMetadata {
    pub fn days_until_expiry(&self) -> i64 {
        let now = SystemTime::now();
        match self.valid_until.0.duration_since(now) {
            Ok(duration) => duration.as_secs() as i64 / 86400,
            Err(_) => -1, // Already expired
        }
    }
}

/// Parsed certificate information
#[derive(Clone, Debug)]
pub struct CertInfo {
    pub fqdn: String,
    pub ip: String,
    pub groups: Vec<String>,
    pub valid_until_secs: u64,
    pub identity_hash: String,
}

impl CertInfo {
    pub fn days_until_expiry(&self) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let remaining = self.valid_until_secs.saturating_sub(now);
        remaining as i64 / 86400
    }

    pub fn format_valid_until(&self) -> String {
        // Format as YYYY-MM-DD using simple date calculation
        // This is a simplified formatter - for display purposes
        let secs = self.valid_until_secs;
        let days_since_epoch = secs / 86400;
        
        // Rough approximation: 1970 + days/365.25
        let year = 1970 + (days_since_epoch as f64 / 365.25) as i64;
        let day_of_year = days_since_epoch % 365;
        let month = (day_of_year / 30) + 1;
        let day = (day_of_year % 30) + 1;
        
        format!("{:04}-{:02}-{:02}", year, month, day)
    }
}

/// Manages content-addressed Nebula certificates
pub struct CertificateManager {
    rekeyed_dir: PathBuf,
}

impl CertificateManager {
    pub fn new(rekeyed_dir: PathBuf) -> Self {
        // Ensure directory exists
        if let Err(e) = fs::create_dir_all(&rekeyed_dir) {
            warn!("Failed to create rekeyed directory: {}", e);
        }
        Self { rekeyed_dir }
    }

    /// Get the content-addressed path for a host's certificate
    pub fn get_cert_path(&self, host: &HostConfig) -> PathBuf {
        // We need the public key to compute the hash, so we'll use a placeholder
        // and update the actual storage location when signing
        // This is called during status checks before we have the key
        self.rekeyed_dir.join(format!("*.{}.crt", host.fqdn))
    }

    /// Find the actual certificate file for a host
    pub fn find_cert_file(&self, host: &HostConfig) -> Option<PathBuf> {
        let pattern = format!("*.{}.crt", host.fqdn);
        let entries = fs::read_dir(&self.rekeyed_dir).ok()?;
        
        for entry in entries {
            let entry = entry.ok()?;
            let filename = entry.file_name();
            let filename_str = filename.to_string_lossy();
            
            if filename_str.ends_with(&format!(".{}.crt", host.fqdn)) {
                return Some(entry.path());
            }
        }
        
        None
    }

    /// Get certificate info for a host if it exists
    pub fn get_cert_info(&self, host: &HostConfig) -> Result<Option<CertInfo>> {
        let cert_path = match self.find_cert_file(host) {
            Some(path) => path,
            None => return Ok(None),
        };

        let metadata_path = cert_path.with_extension("json");
        
        // Try to read metadata first (faster than parsing cert)
        if let Ok(content) = fs::read_to_string(&metadata_path) {
            if let Ok(metadata) = serde_json::from_str::<CertMetadata>(&content) {
                return Ok(Some(CertInfo {
                    fqdn: metadata.fqdn,
                    ip: metadata.ip,
                    groups: metadata.groups,
                    valid_until_secs: metadata.valid_until.0
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    identity_hash: metadata.identity_hash,
                }));
            }
        }

        // Fallback: parse certificate directly
        self.parse_cert_info(&cert_path)
    }

    /// Parse certificate info from a Nebula certificate file
    fn parse_cert_info(&self, cert_path: &PathBuf) -> Result<Option<CertInfo>> {
        // Nebula uses a custom certificate format, not standard x509
        // We'll need to use nebula-cert to inspect it
        let output = Command::new("nebula-cert")
            .args(&["print", "-json", "-path", &cert_path.to_string_lossy()])
            .output()
            .context("Failed to run nebula-cert print")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("nebula-cert print failed: {}", stderr);
            return Ok(None);
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .context("Failed to parse nebula-cert output")?;

        let details = json
            .get("details")
            .ok_or_else(|| anyhow::anyhow!("Missing details in cert print output"))?;

        let fqdn = details
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let ip = details
            .get("ips")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|ip| ip.as_str())
            .unwrap_or("unknown")
            .to_string();

        let groups = details
            .get("groups")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let not_after = details
            .get("notAfter")
            .and_then(|v| v.as_str())
            // Parse RFC3339 format: 2024-01-15T10:30:00Z
            .and_then(|s| {
                // Simple RFC3339 parser for the format from nebula-cert
                // Expected format: "2006-01-02T15:04:05Z"
                if s.len() >= 19 {
                    let year: i64 = s[0..4].parse().ok()?;
                    let month: i64 = s[5..7].parse().ok()?;
                    let day: i64 = s[8..10].parse().ok()?;
                    let hour: i64 = s[11..13].parse().ok()?;
                    let min: i64 = s[14..16].parse().ok()?;
                    let sec: i64 = s[17..19].parse().ok()?;
                    
                    // Simple calculation (not accounting for leap years precisely)
                    let days_from_epoch = (year - 1970) * 365 + (month - 1) * 30 + (day - 1);
                    let secs_from_epoch = days_from_epoch * 86400 + hour * 3600 + min * 60 + sec;
                    Some(secs_from_epoch as u64)
                } else {
                    None
                }
            })
            .unwrap_or(0);

        // Extract identity hash from filename
        let filename = cert_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let identity_hash = filename
            .split('.')
            .next()
            .unwrap_or("unknown")
            .to_string();

        Ok(Some(CertInfo {
            fqdn,
            ip,
            groups,
            valid_until_secs: not_after,
            identity_hash,
        }))
    }

    /// Sign a certificate for a host using nebula-cert CLI
    pub fn sign_certificate(
        &self,
        host: &HostConfig,
        pub_key: &str,
        ca_key: &PathBuf,
        ca_cert: &PathBuf,
        validity_days: u32,
    ) -> Result<PathBuf> {
        // Compute identity hash
        let pub_key_fingerprint = compute_pub_key_fingerprint(pub_key)?;
        let identity_hash = host.compute_identity_hash(&pub_key_fingerprint);

        // Content-addressed path
        let cert_filename = format!("{}.{}.crt", identity_hash, host.fqdn);
        let cert_path = self.rekeyed_dir.join(&cert_filename);
        let metadata_path = cert_path.with_extension("json");

        // Create temporary public key file
        let temp_pub_key = std::env::temp_dir().join(format!("nebula-{}-pub.key", host.fqdn));
        fs::write(&temp_pub_key, pub_key)
            .with_context(|| "Failed to write temporary public key file")?;

        // Build nebula-cert sign command
        let mut cmd = Command::new("nebula-cert");
        cmd.args(&[
            "sign",
            "-ca-key",
            &ca_key.to_string_lossy(),
            "-ca-crt",
            &ca_cert.to_string_lossy(),
            "-in-pub",
            &temp_pub_key.to_string_lossy(),
            "-out-crt",
            &cert_path.to_string_lossy(),
            "-name",
            &host.fqdn,
            "-ip",
            &host.ip,
            "-duration",
            &format!("{}h", validity_days * 24),
        ]);

        // Add groups if any
        for group in &host.groups {
            cmd.arg("-groups").arg(group);
        }

        info!("Running: {:?}", cmd);
        let output = cmd.output()
            .with_context(|| "Failed to run nebula-cert sign")?;

        // Cleanup temp file
        let _ = fs::remove_file(&temp_pub_key);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("nebula-cert sign failed: {}", stderr));
        }

        // Write metadata file
        let now = SystemTime::now();
        let valid_until = now + std::time::Duration::from_secs(validity_days as u64 * 86400);
        
        let metadata = CertMetadata {
            fqdn: host.fqdn.clone(),
            ip: host.ip.clone(),
            groups: host.groups.clone(),
            valid_until: Timestamp(valid_until),
            issued_at: Timestamp(now),
            identity_hash: identity_hash.clone(),
            public_key_fingerprint: pub_key_fingerprint,
        };

        let metadata_json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&metadata_path, metadata_json)
            .with_context(|| "Failed to write metadata file")?;

        debug!(
            "Signed certificate for {} at {}",
            host.fqdn,
            cert_path.display()
        );

        Ok(cert_path)
    }
}

/// Compute a fingerprint for a public key
fn compute_pub_key_fingerprint(pub_key: &str) -> Result<String> {
    use base64::Engine;
    
    // Nebula public keys are base64-encoded Curve25519 keys
    // We'll hash the raw bytes for consistent identity
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(pub_key.trim())
        .map_err(|e| anyhow::anyhow!("Invalid base64 in public key: {}", e))?;
    
    let mut hasher = Sha256::new();
    hasher.update(&decoded);
    let result = hasher.finalize();
    
    Ok(hex::encode(&result[..8])) // Use first 8 bytes (16 hex chars)
}
