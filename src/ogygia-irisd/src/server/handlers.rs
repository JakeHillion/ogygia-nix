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
use futures::TryStreamExt;
use serde::Deserialize;
use serde::Serialize;

use super::AppState;
use crate::nix::nar::generate_nar_stream;
use crate::nix::narinfo::NarInfo;
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
    let stream = state.local_bloom.serialize_stream();
    let body = Body::from_stream(stream);
    (
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        body,
    )
        .into_response()
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
    if let Some(narinfo) = try_local_store_narinfo(hash).await {
        return (
            StatusCode::OK,
            [("content-type", "text/x-nix-narinfo")],
            narinfo.serialize(),
        )
            .into_response();
    }

    // 2. Try peers via bloom lookup
    if let Some((narinfo, _)) = state.try_peer_narinfo(hash).await {
        return (
            StatusCode::OK,
            [("content-type", "text/x-nix-narinfo")],
            narinfo.serialize(),
        )
            .into_response();
    }

    (StatusCode::NOT_FOUND, "Not found").into_response()
}

/// Try to generate narinfo from local /nix/store path
async fn try_local_store_narinfo(hash: &str) -> Option<NarInfo> {
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

    let narinfo = path_info.to_narinfo(hash);
    tracing::info!("Generated narinfo for {} from local store", hash);

    Some(narinfo)
}

/// GET /nar/{path}
pub async fn get_nar(State(state): State<Arc<AppState>>, Path(path): Path<String>) -> Response {
    let filename = path.rsplit('/').next().unwrap_or(&path);

    // Extract hash from filename (format: {hash}-{name}.nar.{compression})
    let hash = match filename.split('-').next() {
        Some(h) if h.len() == 32 => h,
        _ => {
            tracing::warn!("Invalid NAR path format: {}", path);
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
    };

    // Try local store first
    if let Some(store_path) = find_store_path(hash).await {
        match generate_nar_stream(&store_path).await {
            Ok(stream) => {
                tracing::info!("Streaming NAR for {}", store_path.display());
                let body =
                    Body::from_stream(stream.map_err(|e| std::io::Error::other(e.to_string())));
                return (StatusCode::OK, [("content-type", "application/zstd")], body)
                    .into_response();
            }
            Err(e) => {
                tracing::error!("Failed to generate NAR for {}: {}", store_path.display(), e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate NAR")
                    .into_response();
            }
        }
    }

    // Try peers via bloom lookup
    state.try_peer_nar(hash).await
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
            let url = format!("{}/{}.narinfo", peer_url.trim_end_matches('/'), hash);
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
