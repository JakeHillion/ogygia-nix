//! HTTP request handlers for Nix binary cache protocol

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use serde::Deserialize;
use serde::Serialize;
use tokio_util::io::ReaderStream;

use super::AppState;
use crate::nix::cache::NarCache;
use crate::nix::narinfo::NarInfo;
use crate::nix::narinfo::nar_hash_to_hex;
use crate::nix::store::PathInfo;
use crate::nix::store::find_store_path;

/// GET /nix-cache-info
pub async fn nix_cache_info(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let info = format!(
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: {}\n",
        state.config.server.priority,
    );

    (
        StatusCode::OK,
        [("content-type", "text/x-nix-cache-info")],
        info,
    )
}

/// GET /bloom — return serialized local bloom filter
pub async fn get_bloom(State(state): State<Arc<AppState>>) -> Response {
    match state.local_bloom.serialize() {
        Ok(data) => (
            StatusCode::OK,
            [("content-type", "application/octet-stream")],
            data,
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to serialize bloom: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to serialize bloom",
            )
                .into_response()
        }
    }
}

/// GET /{hash}.narinfo
pub async fn get_narinfo(
    State(state): State<Arc<AppState>>,
    Path(narinfo_path): Path<String>,
) -> Response {
    let hash = match narinfo_path.strip_suffix(".narinfo") {
        Some(h) => h,
        None => {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
    };

    // 1. Try local store first
    if let Some(narinfo) = try_local_store_narinfo(hash, &state.nar_cache).await {
        return (
            StatusCode::OK,
            [("content-type", "text/x-nix-narinfo")],
            narinfo.serialize(),
        )
            .into_response();
    }

    // 2. Try peers via bloom lookup
    if let Some((mut narinfo, _)) = state.try_peer_narinfo(hash).await {
        // Rewrite the URL to use our format with the NarHash encoded as hex,
        // so the NAR request will be self-describing regardless of the peer's format.
        let filename = narinfo.url.rsplit('/').next().unwrap_or(&narinfo.url);
        narinfo.url = format!("nar/{}/{}", nar_hash_to_hex(&narinfo.nar_hash), filename);
        return (
            StatusCode::OK,
            [("content-type", "text/x-nix-narinfo")],
            narinfo.serialize(),
        )
            .into_response();
    }

    (StatusCode::NOT_FOUND, "Not found").into_response()
}

/// GET /local/{hash}.narinfo — local-only narinfo (no peer fan-out)
///
/// Used by peers to query this node's local store without triggering
/// cascading fan-out across the cluster.
pub async fn get_local_narinfo(
    State(state): State<Arc<AppState>>,
    Path(narinfo_path): Path<String>,
) -> Response {
    let hash = match narinfo_path.strip_suffix(".narinfo") {
        Some(h) => h,
        None => {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
    };

    match try_local_store_narinfo(hash, &state.nar_cache).await {
        Some(narinfo) => (
            StatusCode::OK,
            [("content-type", "text/x-nix-narinfo")],
            narinfo.serialize(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// Try to generate narinfo from local /nix/store path.
///
/// Ensures the NAR is cached so the response includes the real compressed
/// FileHash and FileSize (needed for integrity validation and range requests).
async fn try_local_store_narinfo(hash: &str, nar_cache: &NarCache) -> Option<NarInfo> {
    let store_path = find_store_path(hash).await?;

    tracing::debug!("Found store path for {}: {}", hash, store_path.display());

    let path_info = match PathInfo::from_store_path(&store_path).await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(
                "Failed to get path-info for {}: {}",
                store_path.display(),
                e
            );
            return None;
        }
    };

    let mut narinfo = path_info.to_narinfo(hash);

    // Generate/retrieve the cached NAR to get real FileHash and FileSize
    match nar_cache.ensure(hash, &store_path).await {
        Ok(cached) => {
            narinfo.file_hash = cached.file_hash.clone();
            narinfo.file_size = cached.file_size;
        }
        Err(e) => {
            tracing::error!("Failed to generate NAR for {}: {}", store_path.display(), e);
            return None;
        }
    }

    tracing::info!("Generated narinfo for {} from local store", hash);
    Some(narinfo)
}

/// GET /nar/{path}
///
/// Path format: `nar/{nar_hash}/{store_hash}-{name}.nar.zst`
/// The NarHash in the URL makes requests self-describing so we don't need
/// server-side state to match narinfo responses to NAR content.
pub async fn get_nar(State(state): State<Arc<AppState>>, Path(path): Path<String>) -> Response {
    let Some((nar_hash, hash)) = parse_nar_path(&path) else {
        tracing::warn!("Invalid NAR path format: {}", path);
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    let local = try_local_store_nar(hash, nar_hash, &state.nar_cache).await;
    if local.status() != StatusCode::NOT_FOUND {
        return local;
    }

    // Try peers via bloom lookup
    state.try_peer_nar(hash, nar_hash).await
}

/// GET /local/nar/{path} — local-only NAR download (no peer fan-out)
///
/// Used by peers to fetch NARs from this node's local store without
/// triggering cascading fan-out across the cluster.
pub async fn get_local_nar(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
) -> Response {
    let Some((nar_hash, hash)) = parse_nar_path(&path) else {
        tracing::warn!("Invalid NAR path format: {}", path);
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    try_local_store_nar(hash, nar_hash, &state.nar_cache).await
}

/// Parse a NAR path into `(nar_hash, store_hash)`.
///
/// Expected format: `{nar_hash}/{store_hash}-{name}.nar.{compression}`
fn parse_nar_path(path: &str) -> Option<(&str, &str)> {
    let (nar_hash, filename) = path.split_once('/')?;
    let hash = filename.split('-').next()?;
    (hash.len() == 32).then_some((nar_hash, hash))
}

/// Try to serve a NAR from the local store via the disk cache.
async fn try_local_store_nar(
    hash: &str,
    expected_nar_hash: &str,
    nar_cache: &NarCache,
) -> Response {
    let Some(store_path) = find_store_path(hash).await else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    // Verify the local NarHash matches what the URL claims
    match PathInfo::from_store_path(&store_path).await {
        Ok(info) if nar_hash_to_hex(&info.nar_hash) != expected_nar_hash => {
            tracing::debug!(
                "Local NarHash mismatch for {}: expected {}, got {}",
                hash,
                expected_nar_hash,
                nar_hash_to_hex(&info.nar_hash)
            );
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
        Err(e) => {
            tracing::warn!(
                "Failed to get path-info for {}: {}",
                store_path.display(),
                e
            );
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
        _ => {}
    }

    // Get from cache or generate, opening the file under the RwLock
    let (cached, file) = match nar_cache.ensure_and_open(hash, &store_path).await {
        Ok((cached, Some(file))) => (cached, file),
        Ok((_, None)) => {
            tracing::debug!("Cached NAR for {} evicted during open", hash);
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
        Err(e) => {
            tracing::error!("Failed to generate NAR for {}: {}", store_path.display(), e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate NAR").into_response();
        }
    };

    tracing::info!("Serving cached NAR for {}", store_path.display());
    let body = Body::from_stream(ReaderStream::new(file));
    let content_length = cached.file_size.to_string();
    (
        StatusCode::OK,
        [
            ("content-type", "application/zstd"),
            ("content-length", content_length.as_str()),
            ("x-ogygia-nar-file-hash", cached.file_hash.as_str()),
        ],
        body,
    )
        .into_response()
}

/// Query parameters for GET /providers/{hash}
#[derive(Debug, Deserialize)]
pub struct ProvidersQuery {
    #[serde(default)]
    pub verbose: bool,
}

/// Response body for GET /providers/{hash}
#[derive(Debug, Serialize)]
pub struct ProvidersResponse {
    pub hash: String,
    pub providers: Vec<String>,
    pub local: bool,
    /// Bloom filter candidates (only present when verbose=true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bloom_candidates: Option<Vec<String>>,
}

/// GET /providers/{hash} — find which peers have a store path
pub async fn get_providers(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ProvidersQuery>,
) -> Response {
    // Check local store
    let local = state.local_bloom.contains(&hash) && find_store_path(&hash).await.is_some();

    // Check peers via bloom lookup
    let peer_urls = &state.config.peers.urls;
    let mut providers = Vec::new();
    let mut bloom_candidates = Vec::new();

    if !peer_urls.is_empty() {
        state
            .peer_blooms
            .ensure_fresh(peer_urls, &state.http_client)
            .await;

        let candidates = state.peer_blooms.lookup(&hash).await;
        bloom_candidates = candidates.clone();

        for peer_url in &candidates {
            let url = format!("{}/local/{}.narinfo", peer_url.trim_end_matches('/'), hash);
            match state.http_client.get(&url).send().await {
                Ok(response) if response.status().is_success() => {
                    providers.push(peer_url.clone());
                }
                _ => {}
            }
        }
    }

    (
        StatusCode::OK,
        Json(ProvidersResponse {
            hash,
            providers,
            local,
            bloom_candidates: if query.verbose {
                Some(bloom_candidates)
            } else {
                None
            },
        }),
    )
        .into_response()
}

/// Request body for POST /rescan
#[derive(Debug, Deserialize)]
pub struct RescanRequest {
    pub paths: Vec<PathBuf>,
}

/// Response body for POST /rescan
#[derive(Debug, Serialize)]
pub struct RescanResponse {
    pub rescanned: usize,
    pub errors: usize,
}

/// POST /rescan - rescan store paths and insert hashes into bloom
///
/// This endpoint is called by `ogygia iris push` after signing paths.
/// Only indexes paths that are serveable (signed or content-addressed).
pub async fn rescan(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RescanRequest>,
) -> Response {
    let mut rescanned = 0;
    let mut errors = 0;

    for path in &request.paths {
        let path_str = path.to_string_lossy();

        // Validate path format
        if !path_str.starts_with("/nix/store/") {
            tracing::warn!("rescan: invalid path format: {}", path_str);
            errors += 1;
            continue;
        }

        // Query path info and verify serveability
        let info = match PathInfo::from_store_path(path).await {
            Ok(info) => info,
            Err(e) => {
                tracing::warn!("rescan: failed to query path info for {}: {}", path_str, e);
                errors += 1;
                continue;
            }
        };

        if !info.is_serveable() {
            tracing::warn!("rescan: path has no signatures: {}", path_str);
            errors += 1;
            continue;
        }

        // Extract hash from path
        let hash = match path_str
            .strip_prefix("/nix/store/")
            .and_then(|s| s.get(..32))
        {
            Some(h) if h.len() == 32 && h.chars().all(|c| c.is_ascii_alphanumeric()) => h,
            _ => {
                tracing::warn!("rescan: invalid hash in path: {}", path_str);
                errors += 1;
                continue;
            }
        };

        state.local_bloom.insert(hash);
        rescanned += 1;

        tracing::info!("rescan: indexed {}", path_str);
    }

    tracing::info!(
        "rescan complete: {} rescanned, {} errors",
        rescanned,
        errors
    );

    (StatusCode::OK, Json(RescanResponse { rescanned, errors })).into_response()
}
