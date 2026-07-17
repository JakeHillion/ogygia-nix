//! Client for the ogygia-updated daemon: manual updates and canaries.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use clap::Subcommand;

/// Drive the ogygia-updated daemon (root only). With no subcommand, reset
/// the host to the tracked branch tip now.
#[derive(Args)]
pub struct UpdateArgs {
    /// Control socket of the ogygia-updated daemon
    #[arg(long, default_value = ogygia_updated::control::DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    #[command(subcommand)]
    command: Option<UpdateCommand>,
}

#[derive(Subcommand)]
enum UpdateCommand {
    /// Trial a branch on this host, or report the active trial
    Canary(CanaryArgs),
}

/// `canary <branch>` starts a trial; `canary status` reports one.
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
struct CanaryArgs {
    /// Branch to trial; its tip is pinned onto this host now
    branch: Option<String>,

    /// How long to hold the trial before reverting (e.g. 24h, 90m, 2d)
    #[arg(
        long = "for",
        value_name = "DURATION",
        default_value = "24h",
        conflicts_with = "forever"
    )]
    duration: humantime::Duration,

    /// Hold the trial with no timeout
    #[arg(long)]
    forever: bool,

    #[command(subcommand)]
    command: Option<CanaryCommand>,
}

#[derive(Subcommand)]
enum CanaryCommand {
    /// Show the current or most-recent canary
    Status,
}

impl UpdateArgs {
    pub fn run(&self) -> Result<()> {
        let message = match &self.command {
            None => ogygia_updated::control::request_update(&self.socket)?,
            Some(UpdateCommand::Canary(canary)) => canary.run(&self.socket)?,
        };
        println!("{message}");
        Ok(())
    }
}

impl CanaryArgs {
    fn run(&self, socket: &Path) -> Result<String> {
        match &self.command {
            Some(CanaryCommand::Status) => ogygia_updated::control::request_canary_status(socket),
            None => {
                let branch = self
                    .branch
                    .clone()
                    .context("a branch to trial is required (or use `canary status`)")?;
                let timeout = (!self.forever).then(|| Duration::from(self.duration).as_secs());
                ogygia_updated::control::request_canary(socket, branch, timeout)
            }
        }
    }
}
