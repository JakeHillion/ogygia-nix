//! Shared application state and peer lookup logic.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::RwLock;

use crate::bloom::local::LocalBloom;
use crate::bloom::peers::PeerBlooms;
use crate::config::Config;
use crate::nix::db::NixDb;
use crate::nix::narinfo::NarInfo;

/// Shared application state
pub struct AppState {
    pub config: Arc<Config>,
    pub local_bloom: Arc<LocalBloom>,
    pub peer_blooms: Arc<PeerBlooms>,
    pub http_client: reqwest::Client,
    pub nix_db: tokio::sync::Mutex<NixDb>,
    /// Maps store path hash → NarHash for narinfos we've served to clients.
    pub narinfo_cache: RwLock<HashMap<String, String>>,
}

impl AppState {
    /// Try to fetch narinfo from peer via bloom filter lookup.
    ///
    /// Streams bloom lookups concurrently with narinfo fetches: as each
    /// peer's bloom becomes available and matches the hash, a narinfo fetch
    /// is started immediately — without waiting for all blooms to arrive.
    ///
    /// Uses hash-affinity: if we previously served a narinfo for this hash, prefer
    /// a peer whose NarHash matches. This ensures consistency between narinfo and
    /// NAR responses for the same store path.
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

        let expected = self.narinfo_cache.read().await.get(hash).cloned();

        let client = self.http_client.clone();
        let mut narinfo_futs = FuturesUnordered::new();
        let mut first_valid: Option<(NarInfo, String)> = None;

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

                    if expected.as_ref().is_some_and(|h| h == &narinfo.nar_hash) {
                        tracing::info!(
                            "narinfo {} fetched from peer {} (NarHash matches cache)",
                            hash,
                            peer_url
                        );
                        return Some((narinfo, peer_url));
                    }

                    if first_valid.is_none() {
                        first_valid = Some((narinfo, peer_url));
                    }
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

        if let Some((ref narinfo, ref peer_url)) = first_valid {
            tracing::info!("narinfo {} fetched from peer {}", hash, peer_url);
            self.narinfo_cache
                .write()
                .await
                .insert(hash.to_string(), narinfo.nar_hash.clone());
        }

        first_valid
    }

    /// Try to proxy a NAR from a peer found via bloom filter lookup.
    ///
    /// Streams bloom lookups concurrently with narinfo fetches: as each
    /// peer's bloom becomes available and matches the hash, a narinfo fetch
    /// is started immediately — without waiting for all blooms to arrive.
    ///
    /// Uses hash-affinity: looks up the expected NarHash from the narinfo cache
    /// and skips peers whose NarHash doesn't match. This ensures the NAR content
    /// is consistent with the narinfo we previously served.
    pub(super) async fn try_peer_nar(&self, hash: &str) -> Response {
        let peer_urls = &self.config.peers.urls;
        if peer_urls.is_empty() {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }

        let trusted_keys = &self.config.trust.trusted_keys;

        let mut candidate_rx = self
            .peer_blooms
            .lookup_stream(peer_urls, hash, &self.http_client)
            .await;

        let expected = self.narinfo_cache.read().await.get(hash).cloned();

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

                    // Skip peers whose NarHash doesn't match our cached value
                    if expected.as_ref().is_some_and(|h| h != &narinfo.nar_hash) {
                        continue;
                    }

                    let nar_url = format!("{}/{}", peer_url.trim_end_matches('/'), narinfo.url);
                    match stream_nar_from_url(&self.http_client, &nar_url).await {
                        Some(response) => {
                            tracing::info!("Proxying NAR {} from peer {}", hash, peer_url);
                            return response;
                        }
                        None => continue,
                    }
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

        (StatusCode::NOT_FOUND, "Not found").into_response()
    }
}

/// Fetch and parse a narinfo from a single peer.
async fn fetch_narinfo_from_peer(
    client: reqwest::Client,
    peer_url: String,
    hash: String,
) -> Option<(String, NarInfo)> {
    let url = format!("{}/{}.narinfo", peer_url.trim_end_matches('/'), hash);
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

/// Fetch a NAR from a URL and return it as a streaming response.
async fn stream_nar_from_url(client: &reqwest::Client, url: &str) -> Option<Response> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/x-nix-nar")
        .to_string();

    let body = Body::from_stream(
        response
            .bytes_stream()
            .map_err(|e| std::io::Error::other(e.to_string())),
    );

    Some(
        (
            StatusCode::OK,
            [("content-type", content_type.as_str())],
            body,
        )
            .into_response(),
    )
}
