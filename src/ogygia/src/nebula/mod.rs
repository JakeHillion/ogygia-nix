//! `ogygia nebula` subcommand: manage Nebula CA, host keys, and certificates.
//!
//! Mirrors agenix-rekey's UX: declarative cert spec per host in NixOS config,
//! a CLI that walks the flake and signs missing certificates via
//! `nebula-cert`. Host private keys never enter the operator's machine.

pub mod config;
pub mod flake_eval;
pub mod init;
pub mod keygen;
pub mod nebula_cert;
pub mod rekey;

use clap::Args;
use clap::Subcommand;

#[derive(Args)]
pub struct NebulaArgs {
    #[command(subcommand)]
    pub command: NebulaCommand,
}

#[derive(Subcommand)]
pub enum NebulaCommand {
    /// Initialize a new Nebula CA for this fleet
    Init(init::InitArgs),
    /// Generate a host keypair on a remote machine via SSH
    Keygen(keygen::KeygenArgs),
    /// Sign missing certificates for hosts whose spec has no matching .crt
    Rekey(rekey::RekeyArgs),
}

impl NebulaArgs {
    pub fn run(&self) -> anyhow::Result<()> {
        match &self.command {
            NebulaCommand::Init(args) => init::run(args),
            NebulaCommand::Keygen(args) => keygen::run(args),
            NebulaCommand::Rekey(args) => rekey::run(args),
        }
    }
}
