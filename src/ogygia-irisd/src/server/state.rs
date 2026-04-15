//! Shared application state and peer lookup logic.

use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;

use crate::bloom::local::LocalBloom;
use crate::bloom::peers::PeerBlooms;
use crate::config::Config;
use crate::downloader::PeerDownloader;
use crate::nix::cache::NarCache;
use crate::nix::narinfo::NarInfo;
use crate::nix::narinfo::nar_hash_to_sri;

/// Shared application state
pub struct AppState {
    pub config: Arc<Config>,
    pub local_bloom: Arc<LocalBloom>,
    pub peer_blooms: Arc<PeerBlooms>,
    pub http_client: reqwest::Client,
    pub nar_cache: Arc<NarCache>,
    pub downloader: Arc<PeerDownloader>,
}

impl AppState {
    /// Try to fetch narinfo from peer via bloom filter lookup.
    ///
    /// Streams bloom lookups concurrently with narinfo fetches: as each
    /// peer's bloom becomes available and matches the hash, a narinfo fetch
    /// is started immediately — without waiting for all blooms to arrive.
    pub(super) async fn try_peer_narinfo(&self, hash: &str) -> Option<(NarInfo, String)> {
        let peer_urls = &self.config.peers.urls;
        if peer_urls.is_empty() {
            return None;
        }

        let trusted_keys = &self.config.trust.trusted_keys;

        let mut candidate_rx = self
            .peer_blooms
            .lookup_stream(peer_urls, hash, &self.http_client)
            .await;

        let client = self.http_client.clone();
        let mut narinfo_futs = FuturesUnordered::new();

        loop {
            tokio::select! {
                biased;

                Some(result) = narinfo_futs.next() => {
                    let Some((peer_url, narinfo)): Option<(String, NarInfo)> = result else {
                        continue;
                    };

                    if !trusted_keys.is_empty() && !narinfo.has_trusted_signature(trusted_keys) {
                        tracing::debug!(
                            "narinfo {} from {} has no trusted signature, skipping",
                            hash,
                            peer_url
                        );
                        continue;
                    }

                    tracing::info!("narinfo {} fetched from peer {}", hash, peer_url);
                    return Some((narinfo, peer_url));
                }

                Some(peer_url) = candidate_rx.recv() => {
                    narinfo_futs.push(fetch_narinfo_from_peer(
                        client.clone(),
                        peer_url,
                        hash.to_string(),
                    ));
                }

                else => break,
            }
        }

        None
    }

    /// Try to download a NAR from peers using the multi-peer downloader.
    ///
    /// Validates bloom lookups and narinfo concurrently, producing a stream
    /// of validated peer NAR URLs. The downloader fetches chunks from
    /// multiple peers in parallel for large files.
    ///
    /// The downloaded NAR is NOT cached — `/nix/store` is the cache for
    /// remote content. The temp file is streamed to the client then deleted.
    pub(super) async fn try_peer_nar(&self, hash: &str, expected_nar_hash: &str) -> Response {
        let peer_urls = &self.config.peers.urls;
        if peer_urls.is_empty() {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }

        let trusted_keys = &self.config.trust.trusted_keys;

        let mut candidate_rx = self
            .peer_blooms
            .lookup_stream(peer_urls, hash, &self.http_client)
            .await;

        let expected_sri = nar_hash_to_sri(expected_nar_hash);

        let client = self.http_client.clone();
        let mut narinfo_futs = FuturesUnordered::new();

        // Channel for sending validated peer NAR URLs to the downloader
        let (url_tx, url_rx) = mpsc::unbounded_channel::<String>();

        // Spawn the URL producer: validate bloom+narinfo, send URLs to downloader
        let producer_handle = {
            let hash = hash.to_string();
            let expected_sri = expected_sri.clone();
            let trusted_keys = trusted_keys.clone();
            let client = client.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;

                        Some(result) = narinfo_futs.next() => {
                            let Some((peer_url, narinfo)): Option<(String, NarInfo)> = result else {
                                continue;
                            };

                            if !trusted_keys.is_empty() && !narinfo.has_trusted_signature(&trusted_keys) {
                                tracing::debug!(
                                    "narinfo {} from {} has no trusted signature, skipping",
                                    hash,
                                    peer_url
                                );
                                continue;
                            }

                            if narinfo.nar_hash != expected_sri {
                                continue;
                            }

                            let nar_url = format!(
                                "{}/local/{}",
                                peer_url.trim_end_matches('/'),
                                narinfo.url
                            );
                            if url_tx.send(nar_url).is_err() {
                                break; // Downloader dropped the receiver
                            }
                        }

                        Some(peer_url) = candidate_rx.recv() => {
                            narinfo_futs.push(fetch_narinfo_from_peer(
                                client.clone(),
                                peer_url,
                                hash.clone(),
                            ));
                        }

                        else => break,
                    }
                }
            })
        };

        // Run the multi-peer downloader
        let download_result = self.downloader.download(&self.http_client, url_rx).await;

        // Clean up the producer
        producer_handle.abort();

        match download_result {
            Ok(downloaded) => {
                tracing::info!(
                    "Downloaded NAR {} from peers ({} bytes)",
                    hash,
                    downloaded.size
                );
                serve_temp_file(downloaded.path, downloaded.size).await
            }
            Err(e) => {
                tracing::warn!("Failed to download NAR {} from peers: {}", hash, e);
                (StatusCode::NOT_FOUND, "Not found").into_response()
            }
        }
    }
}

/// Serve a temp file as a streaming response, then delete it.
async fn serve_temp_file(path: std::path::PathBuf, size: u64) -> Response {
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to open downloaded NAR: {}", e);
            let _ = tokio::fs::remove_file(&path).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read NAR").into_response();
        }
    };

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream.map_err(|e| std::io::Error::other(e.to_string())));

    // Spawn cleanup to delete the temp file after the response is sent.
    // We can't delete it immediately since the body is streaming from it.
    // Instead, we rely on the file handle keeping it alive on Linux (unlink
    // while open is safe). Delete after a delay to ensure the stream has
    // time to start.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        let _ = tokio::fs::remove_file(&path).await;
    });

    (
        StatusCode::OK,
        [
            ("content-type", "application/zstd"),
            ("content-length", &size.to_string()),
        ],
        body,
    )
        .into_response()
}

/// Fetch and parse a narinfo from a single peer.
async fn fetch_narinfo_from_peer(
    client: reqwest::Client,
    peer_url: String,
    hash: String,
) -> Option<(String, NarInfo)> {
    let url = format!("{}/local/{}.narinfo", peer_url.trim_end_matches('/'), hash);
    let response = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(
                "Peer {} returned {} for narinfo {}",
                peer_url,
                r.status(),
                hash
            );
            return None;
        }
        Err(e) => {
            tracing::warn!("Failed to fetch narinfo from {}: {}", peer_url, e);
            return None;
        }
    };
    let body = match response.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Failed to read narinfo from {}: {}", peer_url, e);
            return None;
        }
    };
    match NarInfo::parse(&body) {
        Ok(narinfo) => Some((peer_url, narinfo)),
        Err(e) => {
            tracing::warn!("Failed to parse narinfo from {}: {}", peer_url, e);
            None
        }
    }
}
