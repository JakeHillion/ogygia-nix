//! Shelling out to the Nix CLIs.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use async_stream::try_stream;
use futures::Stream;
use futures::StreamExt;
use futures::pin_mut;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;

/// Fallback locations for the `nix` binary when it isn't on `PATH`.
const NIX_FALLBACKS: &[&str] = &[
    "/run/current-system/sw/bin/nix",
    "/nix/var/nix/profiles/default/bin/nix",
];

pub struct NixCli {
    /// `Arc` so it can be cloned into a command's `'static` stream without
    /// reallocating.
    bin: Arc<Path>,
}

impl Default for NixCli {
    /// Locate the `nix` binary: `PATH` first, then known fallback locations.
    fn default() -> Self {
        if which::which("nix").is_ok() {
            tracing::info!("using nix from PATH");
            return Self {
                bin: Arc::from(Path::new("nix")),
            };
        }
        for path in NIX_FALLBACKS {
            if Path::new(path).exists() {
                tracing::info!("using nix fallback: {path}");
                return Self {
                    bin: Arc::from(Path::new(path)),
                };
            }
        }
        tracing::warn!("nix not found on PATH or in fallback locations, assuming `nix`");
        Self {
            bin: Arc::from(Path::new("nix")),
        }
    }
}

impl NixCli {
    /// Sign the store paths in `paths` with the key in `key_file`.
    pub async fn sign_paths<P>(&self, key_file: &Path, paths: impl Stream<Item = P>) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let mut child = Command::new(&*self.bin)
            .args(["store", "sign", "--key-file"])
            .arg(key_file)
            .arg("--stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("failed to spawn nix store sign")?;

        let mut stdin = child.stdin.take().expect("child spawned with piped stdin");

        pin_mut!(paths);
        while let Some(path) = paths.next().await {
            stdin
                .write_all(path.as_ref().as_os_str().as_bytes())
                .await
                .context("writing paths to nix store sign")?;
            stdin.write_all(b"\n").await?;
        }
        // Close stdin so the child sees EOF and stops reading.
        drop(stdin);

        let status = child.wait().await.context("waiting for nix store sign")?;
        if !status.success() {
            return Err(anyhow!("nix store sign exited with {status}"));
        }

        Ok(())
    }

    /// Stream the closure of `paths` via `nix path-info -r`, one store path per
    /// item. Runs lazily on first poll; a spawn failure or non-zero exit surfaces
    /// as an `Err` item.
    pub fn compute_closure(
        &self,
        paths: Vec<String>,
    ) -> impl Stream<Item = Result<String>> + Send + 'static {
        let bin = self.bin.clone();
        try_stream! {
            if paths.is_empty() {
                return;
            }

            let mut child = Command::new(&*bin)
                .args(["path-info", "-r"])
                .args(&paths)
                .stdout(Stdio::piped())
                // stderr inherited so a full stderr pipe can't block the child
                // while we drain only stdout.
                .kill_on_drop(true)
                .spawn()
                .context("failed to spawn nix path-info -r")?;

            let stdout = child
                .stdout
                .take()
                .expect("child spawned with piped stdout");

            let mut lines = BufReader::new(stdout).lines();
            while let Some(line) = lines
                .next_line()
                .await
                .context("reading nix path-info output")?
            {
                if !line.is_empty() {
                    yield line;
                }
            }

            let status = child.wait().await.context("waiting for nix path-info")?;
            if !status.success() {
                Err(anyhow!("nix path-info -r exited with {status}"))?;
            }
        }
    }
}
