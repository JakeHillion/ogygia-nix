//! NAR generation from local /nix/store
//!
//! This module provides functions for generating NAR archives from store paths
//! using `nix-store --dump` with streaming zstd compression.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use async_compression::tokio::bufread::ZstdEncoder;
use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio_util::io::ReaderStream;

/// Buffer size for reading NAR output
const BUFFER_SIZE: usize = 64 * 1024; // 64KB

/// Generate a zstd-compressed NAR stream from a store path.
///
/// Runs `nix-store --dump <path>` and compresses the output with zstd
/// on-the-fly.
pub async fn generate_nar_stream(
    store_path: &Path,
) -> Result<impl Stream<Item = Result<Bytes>> + Send> {
    let mut child = Command::new("nix-store")
        .arg("--dump")
        .arg(store_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn nix-store --dump for {}",
                store_path.display()
            )
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

    let buf_reader = BufReader::with_capacity(BUFFER_SIZE, stdout);
    let encoder = ZstdEncoder::new(buf_reader);
    let reader_stream = ReaderStream::with_capacity(encoder, BUFFER_SIZE);

    // Hold the child process in the stream so it lives until the stream is
    // dropped, then map the io::Error to anyhow::Error.
    let stream = futures::stream::unfold(
        (reader_stream, child),
        |(mut reader_stream, child)| async move {
            let item = reader_stream.next().await?;
            Some((item.map_err(anyhow::Error::from), (reader_stream, child)))
        },
    );

    Ok(stream)
}
