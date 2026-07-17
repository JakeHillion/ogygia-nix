use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use anyhow::bail;
use clap::Parser;
use ogygia_updated::Config;
use ogygia_updated::Outcome;
use ogygia_updated::Trigger;
use ogygia_updated::control;
use rand::RngExt;
use tracing::error;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Ogygia automatic update daemon.
#[derive(Parser)]
#[command(
    name = "ogygia-updated",
    version,
    about = "Ogygia automatic update daemon"
)]
struct Args {
    /// Path to the TOML configuration file
    #[arg(long)]
    config: PathBuf,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("ogygia_updated=info,warn")),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;

    let listener = control::bind(&config.daemon.socket_path)?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || control::listen(listener, sender));

    // Wait out the initial delay or the interval between cycles, but run
    // immediately when an update is requested over the control socket.
    let mut wait = Duration::from_secs(config.daemon.initial_delay_seconds);
    loop {
        let request = match receiver.recv_timeout(wait) {
            Ok(stream) => Some(stream),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => bail!("control socket listener exited"),
        };
        let trigger = if request.is_some() {
            info!("update requested over the control socket");
            Trigger::Manual
        } else {
            Trigger::Scheduled
        };

        let result = ogygia_updated::run_once(&config, trigger);
        let response = match &result {
            Ok(outcome) => {
                info!(%outcome, "update cycle finished");
                Ok(outcome.to_string())
            }
            Err(error) => {
                let message = format!("{error:#}");
                error!(message, "update cycle failed");
                Err(message)
            }
        };
        if let Some(mut stream) = request {
            control::respond(&mut stream, response);
        }

        // Activating a configuration may have replaced this unit's
        // definition, which switch-to-configuration never restarts
        // (restartIfChanged is off so it cannot kill its own parent
        // mid-cycle); exit so systemd brings the daemon back under the
        // new definition.
        if matches!(
            result,
            Ok(Outcome::Switched { .. } | Outcome::TestActivated { .. })
        ) {
            info!("activated a new configuration; exiting to adopt its unit definition");
            return Ok(());
        }

        let jitter = match config.daemon.jitter_seconds {
            0 => 0,
            bound => rand::rng().random_range(0..bound),
        };
        wait = Duration::from_secs(config.daemon.interval_seconds + jitter);
    }
}
