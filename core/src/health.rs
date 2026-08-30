//! Liveness/readiness endpoints.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use crate::state::AppState;

/// `/healthz` (liveness) and `/readyz` (dependency readiness).
pub fn health_router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
        .route("/readyz", get(ready))
}

async fn ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    let database_ready = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let storage_ready = state.s3.check_ready().await.is_ok();
    if database_ready && storage_ready {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}
