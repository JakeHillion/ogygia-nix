//! Iris subcommand for interacting with irisd.
//!
//! This module provides commands for pushing locally-built store paths
//! to the irisd binary cache, and querying peer blooms for providers.

pub mod client;
pub mod nix;
pub mod providers;
pub mod push;

use clap::Args;
use clap::Subcommand;

/// Iris subcommand arguments
#[derive(Args)]
pub struct IrisArgs {
    #[command(subcommand)]
    pub command: IrisCommand,
}

/// Available Iris subcommands
#[derive(Subcommand)]
pub enum IrisCommand {
    /// Push store paths to irisd cache
    Push(push::PushArgs),
    /// Query peer blooms for providers of a store path
    Providers(providers::ProvidersArgs),
}

impl IrisArgs {
    /// Execute the iris subcommand
    pub fn run(&self) -> anyhow::Result<()> {
        match &self.command {
            IrisCommand::Push(args) => push::run(args),
            IrisCommand::Providers(args) => providers::run(args),
        }
    }
}
