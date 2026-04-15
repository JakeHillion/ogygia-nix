//! Multi-peer parallel NAR downloader.
//!
//! Downloads compressed NARs from multiple peers using adaptive parallelism:
//! small files complete from a single peer, large files scale to dozens of
//! peers using HTTP Range requests.

use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use tokio::io::AsyncSeekExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

use crate::config::CacheConfig;

/// Result of downloading a NAR from peers.
pub struct DownloadedNar {
    /// Path to the temporary file containing the compressed NAR.
    pub path: PathBuf,
    /// Compressed file size in bytes.
    pub size: u64,
}

/// Multi-peer parallel NAR downloader.
pub struct PeerDownloader {
    chunk_size: u64,
    escalation_delay: Duration,
    temp_dir: PathBuf,
}

impl PeerDownloader {
    pub fn new(config: &CacheConfig) -> Self {
        Self {
            chunk_size: config.chunk_size_mb * 1024 * 1024,
            escalation_delay: Duration::from_millis(config.escalation_delay_ms),
            temp_dir: config.dir.join("downloads"),
        }
    }

    /// Download a NAR from peers to a temporary file.
    ///
    /// Accepts a stream of validated peer NAR URLs. Starts with a single
    /// peer and escalates to parallel range-based downloading for large
    /// files after `escalation_delay`.
    pub async fn download(
        &self,
        client: &reqwest::Client,
        mut peer_urls: mpsc::UnboundedReceiver<String>,
    ) -> Result<DownloadedNar> {
        // Wait for the first peer URL
        let first_url = peer_urls
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("No peers available for NAR download"))?;

        // Send initial GET to determine Content-Length
        let response = client
            .get(&first_url)
            .send()
            .await
            .context("Failed to connect to peer")?;

        if !response.status().is_success() {
            anyhow::bail!("Peer returned {} for NAR download", response.status());
        }

        let content_length = response.content_length();

        // Create temp file
        let temp_path = self
            .temp_dir
            .join(format!("download-{}.nar.zst.tmp", rand::random::<u64>()));

        let result = match content_length {
            Some(total_size) if total_size > self.chunk_size => {
                self.download_parallel(
                    client, response, &first_url, total_size, &temp_path, peer_urls,
                )
                .await
            }
            _ => {
                // Small file or unknown size: single peer, stream through
                self.download_single(response, &temp_path).await
            }
        };

        match result {
            Ok(nar) => Ok(nar),
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                Err(e)
            }
        }
    }

    /// Download from a single peer, streaming the full response.
    async fn download_single(
        &self,
        response: reqwest::Response,
        temp_path: &Path,
    ) -> Result<DownloadedNar> {
        let mut file = tokio::fs::File::create(temp_path).await?;
        let mut total_size: u64 = 0;

        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Failed to read from peer")?;
            file.write_all(&chunk).await?;
            total_size += chunk.len() as u64;
        }

        file.flush().await?;

        Ok(DownloadedNar {
            path: temp_path.to_path_buf(),
            size: total_size,
        })
    }

    /// Download from multiple peers using range requests.
    async fn download_parallel(
        &self,
        client: &reqwest::Client,
        first_response: reqwest::Response,
        first_url: &str,
        total_size: u64,
        temp_path: &Path,
        mut peer_urls: mpsc::UnboundedReceiver<String>,
    ) -> Result<DownloadedNar> {
        // Pre-allocate the output file
        let file = tokio::fs::File::create(temp_path).await?;
        file.set_len(total_size).await?;
        let file = std::sync::Arc::new(tokio::sync::Mutex::new(file));

        // Build the work queue: first chunk is being handled by the initial
        // response, remaining chunks go into the queue
        let queue = std::sync::Arc::new(WorkQueue::new(total_size, self.chunk_size));

        // Take chunk 0 for the first peer (it's already streaming)
        let chunk0 = queue.take_chunk();

        // Spawn the first peer's streaming download for chunk 0
        let file_clone = file.clone();
        let chunk0_handle = tokio::spawn(async move {
            if let Some((offset, length)) = chunk0 {
                write_response_chunk(first_response, file_clone, offset, length).await
            } else {
                Ok(0)
            }
        });

        // After escalation delay OR chunk 0 completion, start parallel workers
        let escalation_delay = self.escalation_delay;
        let client = client.clone();
        let queue_clone = queue.clone();
        let file_clone = file.clone();
        let expected_size = total_size;

        let mut worker_handles = Vec::new();

        // Wait for either escalation delay or chunk 0 completion
        let chunk0_result = tokio::select! {
            result = chunk0_handle => {
                // Chunk 0 finished before escalation timer — might be done
                Some(result.context("chunk 0 task panicked")??)
            }
            _ = tokio::time::sleep(escalation_delay) => {
                None // Timer fired, start parallel workers
            }
        };

        // Collect available peer URLs and spawn workers
        let mut available_peers: Vec<String> = Vec::new();

        // Drain any immediately available peer URLs
        while let Ok(url) = peer_urls.try_recv() {
            available_peers.push(url);
        }

        // Also add the first peer for subsequent chunks (it supports ranges
        // if it returned Content-Length)
        let first_url_owned = first_url.to_string();

        // Spawn workers for available peers
        for peer_url in available_peers {
            let client = client.clone();
            let queue = queue_clone.clone();
            let file = file_clone.clone();
            worker_handles.push(tokio::spawn(async move {
                peer_worker(client, peer_url, queue, file, expected_size).await;
            }));
        }

        // If chunk 0 hasn't finished yet, wait for it and also continue
        // accepting new peers
        if chunk0_result.is_none() {
            // First peer can also work on subsequent chunks
            let client2 = client.clone();
            let queue2 = queue_clone.clone();
            let file2 = file_clone.clone();
            worker_handles.push(tokio::spawn(async move {
                peer_worker(client2, first_url_owned, queue2, file2, expected_size).await;
            }));

            // Continue accepting peer URLs until the channel closes
            // or all work is done
            let queue3 = queue_clone.clone();
            let client3 = client.clone();
            let file3 = file_clone.clone();
            worker_handles.push(tokio::spawn(async move {
                while let Some(url) = peer_urls.recv().await {
                    if queue3.is_complete() {
                        break;
                    }
                    let client = client3.clone();
                    let queue = queue3.clone();
                    let file = file3.clone();
                    tokio::spawn(async move {
                        peer_worker(client, url, queue, file, expected_size).await;
                    });
                }
            }));
        }

        // Wait for all workers to finish
        for handle in worker_handles {
            let _ = handle.await;
        }

        if !queue.is_complete() {
            anyhow::bail!("NAR download incomplete: not all chunks were fetched");
        }

        Ok(DownloadedNar {
            path: temp_path.to_path_buf(),
            size: total_size,
        })
    }
}

/// Write a response body to a file at a specific offset, up to `length` bytes.
async fn write_response_chunk(
    response: reqwest::Response,
    file: std::sync::Arc<tokio::sync::Mutex<tokio::fs::File>>,
    offset: u64,
    length: u64,
) -> Result<u64> {
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    use futures::StreamExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Failed to read from peer")?;
        let remaining = length - written;
        let to_write = chunk.len().min(remaining as usize);

        {
            let mut f = file.lock().await;
            f.seek(std::io::SeekFrom::Start(offset + written)).await?;
            f.write_all(&chunk[..to_write]).await?;
        }

        written += to_write as u64;
        if written >= length {
            break;
        }
    }

    Ok(written)
}

/// Worker loop for a single peer: take chunks from the queue and download them.
async fn peer_worker(
    client: reqwest::Client,
    peer_url: String,
    queue: std::sync::Arc<WorkQueue>,
    file: std::sync::Arc<tokio::sync::Mutex<tokio::fs::File>>,
    expected_total_size: u64,
) {
    let mut failures = 0u32;
    const MAX_FAILURES: u32 = 3;

    loop {
        let Some((offset, length)) = queue.take_chunk() else {
            break;
        };

        let end = offset + length - 1;
        let range_header = format!("bytes={offset}-{end}");

        let result = async {
            let resp = client
                .get(&peer_url)
                .header("Range", &range_header)
                .send()
                .await?;

            if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                // Peer supports range requests
                write_response_chunk(resp, file.clone(), offset, length).await?;
                Ok(true)
            } else if resp.status().is_success() {
                // Peer returned 200 — doesn't support ranges. Check
                // Content-Length matches to detect compression mismatch.
                if let Some(cl) = resp.content_length()
                    && cl != expected_total_size
                {
                    tracing::error!(
                        "Peer {} has different compressed size: expected {}, got {}",
                        peer_url,
                        expected_total_size,
                        cl
                    );
                    return Ok(false);
                }
                // Consume full response writing from offset 0
                write_response_chunk(resp, file.clone(), 0, expected_total_size).await?;
                // Mark all chunks as complete
                queue.mark_all_complete();
                Ok(true)
            } else {
                anyhow::bail!("Peer returned {}", resp.status());
            }
        }
        .await;

        match result {
            Ok(true) => {
                failures = 0;
            }
            Ok(false) => {
                // Compression mismatch — reject this peer entirely
                queue.return_chunk(offset, length);
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "Peer {} failed on chunk at offset {}: {}",
                    peer_url,
                    offset,
                    e
                );
                queue.return_chunk(offset, length);
                failures += 1;

                if failures >= MAX_FAILURES && !queue.is_only_worker() {
                    tracing::warn!("Peer {} banned after {} failures", peer_url, failures);
                    break;
                }

                // If this is the only peer, retry with backoff
                tokio::time::sleep(Duration::from_millis(100 * failures as u64)).await;
            }
        }
    }
}

/// Shared work queue for parallel chunk downloading.
struct WorkQueue {
    chunks: Mutex<VecDeque<(u64, u64)>>,
    completed_bytes: AtomicU64,
    total_size: u64,
    active_workers: AtomicU64,
}

impl WorkQueue {
    fn new(total_size: u64, chunk_size: u64) -> Self {
        let mut chunks = VecDeque::new();
        let mut offset = 0;
        while offset < total_size {
            let length = (total_size - offset).min(chunk_size);
            chunks.push_back((offset, length));
            offset += length;
        }
        Self {
            chunks: Mutex::new(chunks),
            completed_bytes: AtomicU64::new(0),
            total_size,
            active_workers: AtomicU64::new(0),
        }
    }

    fn take_chunk(&self) -> Option<(u64, u64)> {
        let mut chunks = self.chunks.blocking_lock();
        let chunk = chunks.pop_front();
        if chunk.is_some() {
            self.active_workers.fetch_add(1, Ordering::Relaxed);
        }
        chunk
    }

    fn return_chunk(&self, offset: u64, length: u64) {
        let mut chunks = self.chunks.blocking_lock();
        chunks.push_back((offset, length));
        self.active_workers.fetch_sub(1, Ordering::Relaxed);
    }

    fn mark_all_complete(&self) {
        self.completed_bytes
            .store(self.total_size, Ordering::Relaxed);
        let mut chunks = self.chunks.blocking_lock();
        chunks.clear();
    }

    fn is_complete(&self) -> bool {
        self.completed_bytes.load(Ordering::Relaxed) >= self.total_size
    }

    fn is_only_worker(&self) -> bool {
        self.active_workers.load(Ordering::Relaxed) <= 1
    }
}
