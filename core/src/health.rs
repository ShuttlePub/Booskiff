//! Liveness/readiness endpoints.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;

use crate::state::AppState;

/// `/healthz` (liveness) and `/readyz` (dependency readiness).
pub fn health_router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(ready))
}

#[utoipa::path(get, path = "/healthz", tag = "health", responses((status = 200)))]
async fn healthz() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}

#[utoipa::path(get, path = "/readyz", tag = "health", responses((status = 200), (status = 503)))]
async fn ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    let database_ready = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    let storage_ready = state.s3.check_ready().await.is_ok();
    if database_ready && storage_ready {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}
