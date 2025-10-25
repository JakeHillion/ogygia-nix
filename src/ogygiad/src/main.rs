mod config;
mod zookeeper_versions;

use anyhow::Result;
use config::Config;
use std::path::PathBuf;
use tracing::{error, info};
use zookeeper_versions::ZooKeeperVersions;

#[derive(Debug)]
struct Args {
    config_path: PathBuf,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let config_path = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("Missing config file argument"))?;

        Ok(Args {
            config_path: PathBuf::from(config_path),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Starting ogygiad");

    // Parse arguments
    let args = Args::parse()?;
    info!("Loading config from: {:?}", args.config_path);

    // Load configuration
    let config = Config::from_file(&args.config_path)?;

    // Set up graceful shutdown
    let shutdown_signal = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
        info!("Received shutdown signal");
    };

    // Run tasks based on configuration
    let mut tasks = Vec::new();

    // ZooKeeper version upload task
    if config.zookeeper.enable_version_upload {
        info!("ZooKeeper version upload is enabled");

        let zk_versions = ZooKeeperVersions::new(
            &config.zookeeper.addresses,
            config.zookeeper.hostname.clone(),
        )
        .await?;

        tasks.push(tokio::spawn(async move {
            if let Err(e) = zk_versions.run().await {
                error!("ZooKeeper version upload failed: {}", e);
            }
        }));
    } else {
        info!("ZooKeeper version upload is disabled");
    }

    if tasks.is_empty() {
        info!("No tasks enabled, exiting");
        return Ok(());
    }

    // Wait for either shutdown signal or a task to complete
    tokio::select! {
        _ = shutdown_signal => {
            info!("Shutting down gracefully");
        }
        result = async {
            for task in tasks {
                task.await.ok();
            }
        } => {
            info!("All tasks completed");
            result
        }
    }

    Ok(())
}
