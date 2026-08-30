//! Liveness/readiness endpoints.

use axum::Router;
use axum::routing::get;

/// `/healthz` (liveness) and `/readyz` (readiness). Readiness always
/// reports ok until Wave-3 wires real dependency checks.
pub fn health_router() -> Router<()> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
}
