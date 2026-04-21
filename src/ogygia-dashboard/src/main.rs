use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::routing::get;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::net::UnixListener;
use tower::ServiceExt;

mod config;
mod etcd;
mod git;
mod nixos;
mod web;

use crate::config::Config;
use crate::web::AppState;

#[derive(Parser)]
#[command(name = "ogygia-dashboard")]
#[command(about = "NixOS host status visualization webserver")]
struct Cli {
    /// Configuration file path (required)
    #[arg(short, long)]
    config: PathBuf,

    /// Port to bind the server to (TCP) - overrides config file
    #[arg(short, long)]
    port: Option<u16>,

    /// Unix socket path to bind to (alternative to TCP) - overrides config file
    #[arg(short, long)]
    socket: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // Load configuration
    let mut config = Config::load_from_file(&cli.config)?;

    // Override config with CLI arguments
    config.override_server_config(
        cli.port,
        cli.socket.map(|p| p.to_string_lossy().to_string()),
    );

    let app_state = Arc::new(AppState::new(config.clone()).await?);

    let app = Router::new()
        .route("/", get(web::index))
        .route("/web.css", get(web::css))
        .route("/nixos/commits", get(web::nixos_commits_html))
        .with_state(app_state);

    match &config.server.bind {
        crate::config::Bind::Unix { socket } => {
            let socket_path = PathBuf::from(socket);
            // Remove existing socket file if it exists
            if socket_path.exists() {
                std::fs::remove_file(&socket_path)?;
            }

            tracing::info!("Server starting on Unix socket: {}", socket_path.display());
            let listener = UnixListener::bind(&socket_path)?;

            loop {
                let (stream, _) = listener.accept().await?;
                let tower_service = app.clone();

                tokio::spawn(async move {
                    let socket = hyper_util::rt::TokioIo::new(stream);
                    let hyper_service = hyper::service::service_fn(move |request| {
                        tower_service.clone().oneshot(request)
                    });

                    if let Err(err) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(socket, hyper_service)
                        .await
                    {
                        eprintln!("Failed to serve connection: {err}");
                    }
                });
            }
        }
        crate::config::Bind::Tcp { port } => {
            let addr: SocketAddr = format!("0.0.0.0:{port}").parse()?;
            tracing::info!("Server starting on TCP: {addr}");
            let listener = TcpListener::bind(addr).await?;
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}
