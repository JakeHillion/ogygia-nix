//! Nebula CLI commands for certificate management.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

use super::cert::{CertificateManager, HostConfig};

/// Nebula certificate management commands
#[derive(Parser)]
pub struct NebulaArgs {
    #[command(subcommand)]
    pub command: NebulaCommand,
}

#[derive(Subcommand)]
pub enum NebulaCommand {
    /// Discover hosts needing certificates from NixOS configurations
    Discover {
        /// Path to the flake to evaluate
        #[arg(short = 'f', long, default_value = ".")]
        flake: PathBuf,
    },
    /// Show certificate status for all hosts
    Status {
        /// Path to the flake to evaluate for host discovery
        #[arg(short = 'f', long, default_value = ".")]
        flake: PathBuf,
    },
    /// Rekey (rotate) certificates
    Rekey {
        /// Rekey all expiring certificates
        #[arg(short = 'a', long)]
        all: bool,
        /// Specific hostname to rekey (ignored if --all)
        hostname: Option<String>,
        /// Path to CA private key (will prompt for passphrase if encrypted)
        #[arg(short = 'k', long, env = "OGYGIA_NEBULA_CA_KEY")]
        ca_key: PathBuf,
        /// Path to CA certificate
        #[arg(short = 'c', long, default_value = "./nebula/ca.crt")]
        ca_cert: PathBuf,
        /// Path to the flake to evaluate
        #[arg(short = 'f', long, default_value = ".")]
        flake: PathBuf,
        /// Certificate validity in days
        #[arg(short = 'd', long, default_value = "90")]
        validity_days: u32,
        /// Rotate certificates expiring within this many days
        #[arg(short = 'R', long, default_value = "30")]
        rotate_before_days: u32,
        /// Groups to assign (comma-separated, for new hosts only)
        #[arg(short = 'g', long)]
        groups: Option<String>,
    },
}

impl NebulaArgs {
    pub fn run(&self) -> Result<()> {
        match &self.command {
            NebulaCommand::Discover { flake } => discover_hosts(flake),
            NebulaCommand::Status { flake } => show_status(flake),
            NebulaCommand::Rekey {
                all,
                hostname,
                ca_key,
                ca_cert,
                flake,
                validity_days,
                rotate_before_days,
                groups,
            } => {
                if !all && hostname.is_none() {
                    return Err(anyhow::anyhow!(
                        "Either --all flag or a hostname must be specified"
                    ));
                }
                rekey_certs(
                    *all,
                    hostname.as_deref(),
                    ca_key,
                    ca_cert,
                    flake,
                    *validity_days,
                    *rotate_before_days,
                    groups.as_deref(),
                )
            }
        }
    }
}

fn discover_hosts(flake: &PathBuf) -> Result<()> {
    info!("Discovering hosts from NixOS configurations...");

    let hosts = discover_hosts_from_nix(flake)?;

    if hosts.is_empty() {
        println!("No hosts found with ogygia.nebula enabled.");
        return Ok(());
    }

    println!("Found {} hosts with ogygia.nebula enabled:", hosts.len());
    println!();
    println!("{:<40} {:<18} {:<20} {}",
        "Host", "IP", "Groups", "Rekeyed Dir");
    println!("{}", "-".repeat(110));

    for (host, rekeyed_dir) in hosts {
        let groups_str = if host.groups.is_empty() {
            "[]".to_string()
        } else {
            format!("[{}]", host.groups.join(", "))
        };
        println!("{:<40} {:<18} {:<20} {}",
            host.fqdn, host.ip, groups_str, rekeyed_dir.display());
    }

    Ok(())
}

fn show_status(flake: &PathBuf) -> Result<()> {
    let hosts = discover_hosts_from_nix(flake)?;

    if hosts.is_empty() {
        println!("No hosts configured with ogygia.nebula.");
        return Ok(());
    }

    println!("{:<40} {:<18} {:<18} {:<14} {:<12} {}",
        "Host", "IP", "Groups", "Valid Until", "Expires In", "Status");
    println!("{}", "-".repeat(110));

    for (host, rekeyed_dir) in hosts {
        let groups_str = if host.groups.is_empty() {
            "[]".to_string()
        } else {
            format!("[{}]", host.groups.join(", "))
        };

        let manager = CertificateManager::new(rekeyed_dir);
        
        match manager.get_cert_info(&host)? {
            Some(info) => {
                let expires_in_days = info.days_until_expiry();
                let status = if expires_in_days <= 0 {
                    "✗ expired"
                } else if expires_in_days <= 30 {
                    "⚠ expiring soon"
                } else {
                    "✓ current"
                };

                println!("{:<40} {:<18} {:<18} {:<14} {:<12} {}",
                    host.fqdn,
                    host.ip,
                    groups_str,
                    info.format_valid_until(),
                    format!("{} days", expires_in_days),
                    status
                );
            }
            None => {
                println!("{:<40} {:<18} {:<18} {:<14} {:<12} {}",
                    host.fqdn,
                    host.ip,
                    groups_str,
                    "-",
                    "-",
                    "✗ missing"
                );
            }
        }
    }

    Ok(())
}

fn rekey_certs(
    all: bool,
    hostname: Option<&str>,
    ca_key: &PathBuf,
    ca_cert: &PathBuf,
    flake: &PathBuf,
    validity_days: u32,
    rotate_before_days: u32,
    groups_override: Option<&str>,
) -> Result<()> {
    let hosts_with_dirs = discover_hosts_from_nix(flake)?;

    let targets: Vec<(HostConfig, PathBuf)> = if all {
        if hostname.is_some() {
            println!("Warning: --all flag specified, ignoring hostname argument");
        }
        // Filter hosts that need rotation (expiring or missing)
        hosts_with_dirs
            .into_iter()
            .filter(|(host, rekeyed_dir)| {
                let manager = CertificateManager::new(rekeyed_dir.clone());
                match manager.get_cert_info(host) {
                    Ok(Some(info)) => info.days_until_expiry() <= rotate_before_days as i64,
                    _ => true, // Missing or error = needs rekey
                }
            })
            .collect()
    } else {
        let hostname = hostname.unwrap();
        hosts_with_dirs
            .into_iter()
            .filter(|(h, _)| h.fqdn == hostname)
            .collect()
    };

    if targets.is_empty() {
        println!("No certificates need rekeying.");
        return Ok(());
    }

    println!("Rekeying {} certificate(s)...", targets.len());
    println!("CA key: {}", ca_key.display());
    println!("Validity: {} days", validity_days);
    println!();

    for (host, rekeyed_dir) in targets {
        println!("Rekeying {} ({})...", host.fqdn, host.ip);
        println!("  Writing to: {}", rekeyed_dir.display());

        // Check if we have the host's public key
        let pub_key = match get_host_public_key(&host) {
            Some(key) => key,
            None => {
                println!("  ⚠ No public key found for {}. Deploy host first to generate keys.", host.fqdn);
                continue;
            }
        };

        // Apply groups override if specified (for new hosts mainly)
        let host = if let Some(groups_str) = groups_override {
            let groups: Vec<String> = groups_str.split(',').map(|s| s.trim().to_string()).collect();
            HostConfig {
                groups,
                ..host
            }
        } else {
            host
        };

        let manager = CertificateManager::new(rekeyed_dir);
        
        match manager.sign_certificate(&host, &pub_key, ca_key, ca_cert, validity_days) {
            Ok(path) => {
                println!("  ✓ Signed: {}", path.display());
                println!("    Content-addressed path: {}", manager.get_cert_path(&host).display());
            }
            Err(e) => {
                println!("  ✗ Failed: {}", e);
            }
        }
    }

    println!();
    println!("Remember to commit the rekeyed certificates before deploying.");

    Ok(())
}

fn discover_hosts_from_nix(flake: &PathBuf) -> Result<Vec<(HostConfig, PathBuf)>> {
    // Use nix eval to discover hosts with ogygia.nebula enabled
    // Also extract the rekeyedDir from each host's configuration
    let output = std::process::Command::new("nix")
        .args(&[
            "eval",
            "--json",
            &format!("{}#nixosConfigurations", flake.display()),
            "--apply",
            r#"configs: builtins.mapAttrs (name: cfg: {
                fqdn = cfg.config.networking.fqdn or name;
                nebula = cfg.config.ogygia.nebula or null;
                rekeyedDir = cfg.config.ogygia.nebula.rekeyedDir or null;
            }) configs"#,
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run nix eval: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("nix eval failed: {}", stderr));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("Failed to parse nix eval output: {}", e))?;

    let mut hosts = Vec::new();

    if let Some(configs) = json.as_object() {
        for (_hostname, config) in configs {
            if let Some(nebula) = config.get("nebula") {
                if nebula.is_null() {
                    continue;
                }

                let fqdn = config
                    .get("fqdn")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing fqdn in config"))?
                    .to_string();

                let ip = nebula
                    .get("ip")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing ip for {}", fqdn))?
                    .to_string();

                let groups = nebula
                    .get("groups")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|g| g.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                // Get the rekeyedDir from NixOS config, or use default
                let rekeyed_dir = config
                    .get("rekeyedDir")
                    .and_then(|v| v.as_str())
                    .map(|s| PathBuf::from(s))
                    .unwrap_or_else(|| PathBuf::from("./nebula/rekeyed"));

                let host = HostConfig {
                    fqdn,
                    ip,
                    groups,
                };

                hosts.push((host, rekeyed_dir));
            }
        }
    }

    Ok(hosts)
}

fn get_host_public_key(_host: &HostConfig) -> Option<String> {
    // TODO: Implement SSH/HTTP retrieval or local file lookup
    // For now, this is a placeholder - hosts need to expose their public keys
    // This would typically:
    // 1. Try to SSH to the host and read /data/nebula/host.pub
    // 2. Or read from a local cache if previously retrieved
    // 3. Or prompt the user to provide it manually
    None
}
