#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(ogygia_macos_nebulad::daemon::run())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ogygia-macos-nebulad runs only on macOS");
    std::process::exit(1);
}
