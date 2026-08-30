//! Billing status endpoint: effective limits plus metered usage.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::extractor::AccountContext;
use crate::billing::usage::used_bytes;
use crate::error::AppError;
use crate::state::AppState;

/// Storage usage and effective limits of the authenticated owner.
#[derive(Debug, Serialize, ToSchema)]
pub struct BillingStatusResponse {
    pub used_bytes: i64,
    pub storage_quota_bytes: i64,
    pub max_file_bytes: i64,
    pub rate_limit_rpm: u32,
}

/// Builds the authenticated billing status router.
pub fn billing_status_router() -> Router<AppState> {
    Router::new().route("/v1/billing/status", get(get_billing_status))
}

#[utoipa::path(
    get,
    path = "/v1/billing/status",
    tag = "billing",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Usage and effective limits for the authenticated owner", body = BillingStatusResponse),
    )
)]
async fn get_billing_status(
    ctx: AccountContext,
    State(state): State<AppState>,
) -> Result<Json<BillingStatusResponse>, AppError> {
    let used_bytes = used_bytes(&state.pool, &ctx.owner).await?;
    Ok(Json(BillingStatusResponse {
        used_bytes,
        storage_quota_bytes: ctx.limits.storage_quota_bytes,
        max_file_bytes: ctx.limits.max_file_bytes,
        rate_limit_rpm: ctx.limits.rate_limit_rpm,
    }))
}
