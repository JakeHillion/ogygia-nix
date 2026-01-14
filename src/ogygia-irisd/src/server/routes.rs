//! HTTP route definitions

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use axum::routing::post;

use super::AppState;
use super::handlers;

/// Create the main router with all cache endpoints
///
/// Nix binary cache protocol routes:
/// - GET /nix-cache-info - cache metadata
/// - GET /bloom - serialized bloom filter
/// - GET /`<hash>`.narinfo - store path info (HEAD handled automatically)
/// - POST /rescan - rescan paths for updated signatures
/// - GET /nar/`<path>` - download NAR
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/nix-cache-info", get(handlers::nix_cache_info))
        .route("/bloom", get(handlers::get_bloom))
        .route("/providers/{hash}", get(handlers::get_providers))
        .route("/rescan", post(handlers::rescan))
        // Use wildcard for narinfo since axum doesn't allow {param}.suffix pattern
        .route("/{narinfo_path}", get(handlers::get_narinfo))
        .route("/nar/{*path}", get(handlers::get_nar))
        .with_state(state)
}
