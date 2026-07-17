//! Manual trigger for the pull-based system updater.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

/// Ask the ogygia-updated daemon to run an update cycle now.
#[derive(Args)]
pub struct UpdateArgs {
    /// Control socket of the ogygia-updated daemon
    #[arg(long, default_value = ogygia_updated::control::DEFAULT_SOCKET_PATH)]
    socket: PathBuf,
}

impl UpdateArgs {
    pub fn run(&self) -> Result<()> {
        let message = ogygia_updated::control::request_update(&self.socket)?;
        println!("{message}");
        Ok(())
    }
}
