//! `ogygia nebula keygen <host>` — run `nebula-cert keygen` on a remote host.
//!
//! SSHs in, generates the private key into /etc/nebula/host.key with the right
//! permissions, then `cat`s the public key back to the operator's stdout. The
//! public key is not persisted on the host: it lives only in the flake config
//! (`ogygia.nebula.pubKey`). Refuses to clobber an existing key unless --force.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use clap::Args;
use tokio::process::Command;

#[derive(Args)]
pub struct KeygenArgs {
    /// SSH target (e.g. `user@host`, defaults to using the bare host as the target).
    pub host: String,
    /// SSH user (overrides any user component baked into `host`).
    #[arg(long)]
    pub user: Option<String>,
    /// Overwrite an existing keypair on the host.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: &KeygenArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    runtime.block_on(async_run(args))
}

async fn async_run(args: &KeygenArgs) -> Result<()> {
    let ssh_target = ssh_target(args);

    // Heredoc-quoted to keep the remote script verbatim.
    let force_flag = if args.force { "1" } else { "0" };
    let remote_script = format!(
        r#"set -eu
FORCE={force_flag}
sudo mkdir -p /etc/nebula
sudo chmod 0700 /etc/nebula
if [ "$FORCE" != "1" ] && [ -f /etc/nebula/host.key ]; then
    echo "/etc/nebula/host.key already exists on this host; pass --force to overwrite" >&2
    exit 2
fi
TMPDIR=$(sudo mktemp -d)
trap 'sudo rm -rf "$TMPDIR"' EXIT
sudo nebula-cert keygen -out-key "$TMPDIR/host.key" -out-pub "$TMPDIR/host.pub"
sudo install -m 0600 "$TMPDIR/host.key" /etc/nebula/host.key
sudo cat "$TMPDIR/host.pub"
"#
    );

    tracing::info!(target = %ssh_target, "running nebula-cert keygen on host");

    let output = Command::new("ssh")
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(&ssh_target)
        .arg("--")
        .arg("bash")
        .arg("-s")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("failed to spawn ssh")?
        .write_stdin_and_wait(remote_script.as_bytes())
        .await?;

    if !output.status.success() {
        return Err(anyhow!(
            "remote keygen failed on {ssh_target} (exit {})",
            output.status.code().unwrap_or(-1)
        ));
    }

    let pub_pem =
        String::from_utf8(output.stdout).context("public key output was not valid UTF-8")?;
    let pub_pem = pub_pem.trim_end_matches('\n');

    println!("{pub_pem}");
    eprintln!();
    eprintln!(
        "Paste the above into your flake as `ogygia.nebula.pubKey`, then run `ogygia nebula rekey -a`."
    );
    Ok(())
}

fn ssh_target(args: &KeygenArgs) -> String {
    match &args.user {
        Some(u) => format!("{u}@{}", args.host),
        None => args.host.clone(),
    }
}

trait ChildExt {
    async fn write_stdin_and_wait(self, data: &[u8]) -> Result<std::process::Output>;
}

impl ChildExt for tokio::process::Child {
    async fn write_stdin_and_wait(mut self, data: &[u8]) -> Result<std::process::Output> {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin) = self.stdin.take() {
            stdin
                .write_all(data)
                .await
                .context("failed to write to ssh stdin")?;
            stdin.shutdown().await.ok();
        }
        self.wait_with_output()
            .await
            .context("failed to await ssh process")
    }
}
