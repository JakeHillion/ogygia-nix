use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::str;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use tracing::info;

/// A switch-to-configuration action, mirroring nixos-rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// Activate now without touching the boot loader.
    Test,
    /// Activate now and make it the boot default.
    Switch,
    /// Make it the boot default for the next boot, without activating now.
    Boot,
}

impl Activation {
    fn arg(self) -> &'static str {
        match self {
            Activation::Test => "test",
            Activation::Switch => "switch",
            Activation::Boot => "boot",
        }
    }
}

/// Side effects of an update cycle, abstracted for testing.
pub trait System {
    /// Build `flake_ref` and return its store path.
    fn build(&self, flake_ref: &str) -> Result<PathBuf>;
    /// Point `profile` at `store_path` as a new generation.
    fn set_profile(&self, profile: &Path, store_path: &Path) -> Result<()>;
    /// Run `store_path`'s switch-to-configuration with `action`.
    fn switch_to_configuration(&self, store_path: &Path, action: Activation) -> Result<()>;
    /// Kernel, initrd, and kernel-modules links of a system, used to
    /// detect updates that need a reboot. Missing links resolve to None.
    fn kernel_links(&self, system: &Path) -> [Option<PathBuf>; 3];
    /// Schedule a reboot in `delay_minutes` minutes.
    fn schedule_reboot(&self, delay_minutes: u32) -> Result<()>;
}

pub struct RealSystem;

const EXPERIMENTAL_FEATURES: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

impl System for RealSystem {
    fn build(&self, flake_ref: &str) -> Result<PathBuf> {
        let mut command = Command::new("nix");
        command
            .arg("build")
            .args(EXPERIMENTAL_FEATURES)
            .args(["--option", "always-allow-substitutes", "true"])
            .args(["--no-link", "--print-out-paths"])
            .arg(flake_ref)
            .stderr(std::process::Stdio::inherit());
        info!(?command, "building");
        let output = command.output().context("running nix build")?;
        if !output.status.success() {
            bail!("nix build {flake_ref} failed with {}", output.status);
        }
        let stdout = str::from_utf8(&output.stdout).context("decoding nix build output")?;
        let path = stdout
            .lines()
            .next_back()
            .context("nix build printed no out paths")?;
        Ok(PathBuf::from(path))
    }

    fn set_profile(&self, profile: &Path, store_path: &Path) -> Result<()> {
        run(Command::new("nix-env")
            .arg("-p")
            .arg(profile)
            .arg("--set")
            .arg(store_path))
    }

    fn switch_to_configuration(&self, store_path: &Path, action: Activation) -> Result<()> {
        run(Command::new(store_path.join("bin/switch-to-configuration")).arg(action.arg()))
    }

    fn kernel_links(&self, system: &Path) -> [Option<PathBuf>; 3] {
        ["initrd", "kernel", "kernel-modules"].map(|name| fs::read_link(system.join(name)).ok())
    }

    fn schedule_reboot(&self, delay_minutes: u32) -> Result<()> {
        run(Command::new("shutdown")
            .arg("-r")
            .arg(format!("+{delay_minutes}")))
    }
}

fn run(command: &mut Command) -> Result<()> {
    info!(?command, "running");
    let status = command
        .status()
        .with_context(|| format!("running {:?}", command.get_program()))?;
    if !status.success() {
        bail!("{:?} failed with {status}", command.get_program());
    }
    Ok(())
}
