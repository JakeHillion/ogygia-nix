//! Runs the `clevis` command line for the cryptography, so blobs stay
//! byte-compatible with what the initrd decrypts.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// How long one clevis invocation may take. Decrypting talks to every bound
/// Tang server through curl, which bounds connecting but not a stalled
/// reply.
const TIMEOUT: Duration = Duration::from_secs(60);

pub async fn decrypt(jwe: &[u8]) -> Result<Vec<u8>> {
    run(&["decrypt"], jwe).await
}

pub async fn encrypt_sss(config: &Value, plaintext: &[u8]) -> Result<Vec<u8>> {
    run(&["encrypt", "sss", &config.to_string()], plaintext).await
}

async fn run(args: &[&str], stdin: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new("clevis")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("running clevis")?;
    let output = timeout(TIMEOUT, async move {
        let mut input = child.stdin.take().context("opening clevis stdin")?;
        input.write_all(stdin).await.context("writing to clevis")?;
        drop(input);
        child.wait_with_output().await.context("waiting for clevis")
    })
    .await
    .with_context(|| {
        format!(
            "clevis {} did not finish within {}s",
            args[0],
            TIMEOUT.as_secs()
        )
    })??;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success(),
        "clevis {} failed ({}): {}",
        args[0],
        output.status,
        stderr.trim()
    );
    // clevis exits 0 after some failed decryptions, so no output is a
    // failure too.
    ensure!(
        !output.stdout.is_empty(),
        "clevis {} produced no output: {}",
        args[0],
        stderr.trim()
    );
    Ok(output.stdout)
}
