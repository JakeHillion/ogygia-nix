//! Shelling out to the Nix CLIs.

use std::collections::HashMap;
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
use serde::Deserialize;
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
    /// Whether `store_path` was built locally rather than substituted from a
    /// binary cache — Nix's `ultimate` flag, read from `nix path-info --json`.
    pub async fn is_ultimate(&self, store_path: &str) -> Result<bool> {
        let output = Command::new(&*self.bin)
            .args(["path-info", "--json"])
            .arg(store_path)
            .output()
            .await
            .context("failed to run nix path-info --json")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("nix path-info failed: {}", stderr.trim()));
        }

        let stdout =
            std::str::from_utf8(&output.stdout).context("invalid UTF-8 in nix path-info output")?;
        ultimate_from_path_info_json(stdout, store_path)
    }

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

/// Read the `ultimate` flag for `store_path` from `nix path-info --json` output.
///
/// The output is a map keyed by store path, where the queried path's entry
/// carries a boolean `ultimate` (a path Nix doesn't know is reported as a `null`
/// entry). Split out from [`NixCli::is_ultimate`] so the parse can be exercised
/// against real `nix path-info --json` output without spawning `nix`.
fn ultimate_from_path_info_json(json: &str, store_path: &str) -> Result<bool> {
    #[derive(Deserialize)]
    struct Entry {
        #[serde(default)]
        ultimate: bool,
    }

    let map: HashMap<String, Option<Entry>> =
        serde_json::from_str(json).context("failed to parse nix path-info JSON")?;
    let entry = map
        .get(store_path)
        .with_context(|| format!("nix path-info returned no entry for {store_path}"))?
        .as_ref()
        .with_context(|| format!("{store_path} is not a valid store path"))?;
    Ok(entry.ultimate)
}

#[cfg(test)]
mod tests {
    use testcontainers::GenericImage;
    use testcontainers::ImageExt;
    use testcontainers::core::CmdWaitFor;
    use testcontainers::core::ExecCommand;
    use testcontainers::core::Mount;
    use testcontainers::core::WaitFor;
    use testcontainers::runners::AsyncRunner;

    use super::*;

    /// Shell script run inside the container to produce one locally-built path
    /// (Nix marks build outputs `ultimate`) and one substituted path (the base
    /// image's `bash`, imported into the store, so `ultimate` is unset). It
    /// writes each path and the golden `nix path-info --json` covering both into
    /// the bind-mounted `/work` directory.
    const SETUP_SCRIPT: &str = r#"
set -eu
export NIX_CONFIG='substituters =
experimental-features = nix-command'

SH=$(command -v bash)
# Resolve the profile symlink to the real store path, then take the store path
# containing bash: a substituted, non-ultimate path.
REAL=$(readlink -f "$SH")
NONULTIMATE=/nix/store/$(printf '%s' "${REAL#/nix/store/}" | cut -d/ -f1)

cat > /tmp/leaf.nix <<'NIXEOF'
{ sh }:
derivation {
  name = "ultimate-leaf";
  system = builtins.currentSystem;
  builder = sh;
  args = [ "-c" "echo built > $out" ];
}
NIXEOF

# A locally-built output: Nix sets its ultimate flag.
ULTIMATE=$(nix-build --no-out-link --argstr sh "$SH" /tmp/leaf.nix)

printf '%s\n' "$ULTIMATE" > /work/ultimate.txt
printf '%s\n' "$NONULTIMATE" > /work/nonultimate.txt
nix path-info --json "$ULTIMATE" "$NONULTIMATE" > /work/golden.json
chmod -R a+rw /work
"#;

    /// Confirm that `is_ultimate`'s parse reads Nix's `ultimate` flag correctly
    /// from real, version-pinned `nix path-info --json` output. Ground truth is
    /// how each path was created: a locally-built derivation is ultimate, a
    /// substituted path is not — independent of the flag we are reading.
    ///
    /// Requires a Docker daemon.
    #[tokio::test]
    async fn is_ultimate_matches_nix_path_info() {
        let work = tempfile::tempdir().unwrap();

        let container = GenericImage::new("nixos/nix", "2.34.7")
            .with_wait_for(WaitFor::message_on_stdout("container-ready"))
            .with_cmd(["sh", "-c", "echo container-ready && sleep infinity"])
            .with_mount(Mount::bind_mount(
                work.path().to_str().unwrap().to_string(),
                "/work",
            ))
            .start()
            .await
            .expect("start nix container");

        let mut setup = container
            .exec(
                ExecCommand::new(["sh", "-c", SETUP_SCRIPT])
                    .with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await
            .expect("exec setup script");
        let exit = setup.exit_code().await.expect("setup exit code");
        let _ = setup.stdout_to_vec().await;
        let setup_stderr =
            String::from_utf8_lossy(&setup.stderr_to_vec().await.unwrap()).to_string();
        if exit != Some(0) {
            panic!("setup script failed (exit {exit:?}):\n{setup_stderr}");
        }

        let ultimate = std::fs::read_to_string(work.path().join("ultimate.txt"))
            .expect("read ultimate.txt")
            .trim()
            .to_string();
        let nonultimate = std::fs::read_to_string(work.path().join("nonultimate.txt"))
            .expect("read nonultimate.txt")
            .trim()
            .to_string();
        let golden =
            std::fs::read_to_string(work.path().join("golden.json")).expect("read golden.json");

        assert!(
            ultimate_from_path_info_json(&golden, &ultimate).expect("parse ultimate path"),
            "locally-built path should be ultimate: {ultimate}",
        );
        assert!(
            !ultimate_from_path_info_json(&golden, &nonultimate).expect("parse non-ultimate path"),
            "substituted path should not be ultimate: {nonultimate}",
        );
    }
}
