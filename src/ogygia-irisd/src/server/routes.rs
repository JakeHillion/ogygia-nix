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
/// - GET /`<hash>`.narinfo - store path info (queries peers if not local)
/// - GET /nar/`<path>` - download NAR (queries peers if not local)
/// - POST /rescan - rescan paths for updated signatures
///
/// Local-only routes (used for peer-to-peer, no fan-out):
/// - GET /local/`<hash>`.narinfo - local-only store path info
/// - GET /local/nar/`<path>` - local-only NAR download
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/nix-cache-info", get(handlers::nix_cache_info))
        .route("/bloom", get(handlers::get_bloom))
        .route("/providers/{hash}", get(handlers::get_providers))
        .route("/rescan", post(handlers::rescan))
        // Local-only endpoints (no peer fan-out) — used by peers
        .route("/local/{narinfo_path}", get(handlers::get_local_narinfo))
        .route("/local/nar/{*path}", get(handlers::get_local_nar))
        // Public endpoints (with peer fan-out)
        .route("/{narinfo_path}", get(handlers::get_narinfo))
        .route("/nar/{*path}", get(handlers::get_nar))
        .with_state(state)
}
