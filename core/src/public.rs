//! Public (unauthenticated) file download endpoint.

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;

use crate::error::AppError;
use crate::model::OBJECT_KIND_ORIGINAL;
use crate::state::AppState;

const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

#[derive(Debug, sqlx::FromRow)]
struct PublicFile {
    id: uuid::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct OriginalObject {
    storage_key: String,
}

/// Build the unauthenticated public file router.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/public/{key}", get(get_public_file))
}

#[utoipa::path(
    get,
    path = "/public/{key}",
    tag = "public",
    params(("key" = String, Path, description = "Public key of a published file")),
    responses(
        (status = 200, description = "The file content, served with immutable caching"),
        (status = 404, description = "Unknown or unpublished key"),
    )
)]
async fn get_public_file(
    State(state): State<AppState>,
    Path(key): Path<String>,
    connect_info: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Response, AppError> {
    let admitted = match connect_info {
        Ok(ConnectInfo(addr)) => state.public_rate_limiter.check(addr.ip()),
        // Deliberately bypass limiting for oneshot tests or non-TCP listeners
        // without ConnectInfo. The production TCP server always injects it.
        Err(_) => true,
    };
    if !admitted {
        return Err(AppError::RateLimited);
    }
    let file = sqlx::query_as::<_, PublicFile>(
        "SELECT id FROM files WHERE public_key = $1 AND is_public = TRUE",
    )
    .bind(&key)
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("public file lookup failed: {err}")))?
    .ok_or_else(|| AppError::NotFound(format!("public file {key}")))?;

    let object = sqlx::query_as::<_, OriginalObject>(
        "SELECT storage_key FROM file_objects WHERE file_id = $1 AND object_kind = $2",
    )
    .bind(file.id)
    .bind(OBJECT_KIND_ORIGINAL)
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("public object lookup failed: {err}")))?
    .ok_or_else(|| AppError::NotFound(format!("original object for file {}", file.id)))?;

    let (content_type, content_length, stream) =
        state.s3.get_streaming(&object.storage_key).await?;
    let content_type = HeaderValue::from_str(&content_type)
        .map_err(|err| AppError::Internal(format!("invalid object content type: {err}")))?;
    let content_length = HeaderValue::from_str(&content_length.to_string())
        .map_err(|err| AppError::Internal(format!("invalid object content length: {err}")))?;

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, content_type);
    headers.insert(CONTENT_LENGTH, content_length);
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(IMMUTABLE_CACHE_CONTROL),
    );
    Ok((StatusCode::OK, headers, Body::new(stream.into_inner())).into_response())
}

#[cfg(test)]
mod tests {
    use super::IMMUTABLE_CACHE_CONTROL;
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn public_router_limits_each_ip_and_bypasses_missing_connect_info() {
        // Given a closed lazy pool and a two-request public budget.
        let config = crate::config::Config::default();
        let pool = sqlx::PgPool::connect_lazy("postgres://127.0.0.1:9/never").unwrap();
        pool.close().await;
        let state = AppState {
            pool,
            s3: crate::storage::Storage::build(&config).await.unwrap(),
            jwks_cache: crate::auth::jwks::JwksCache::new(Vec::new()),
            rate_limiters: Arc::new(crate::state::RateLimiters::default()),
            billing_cache: Arc::new(crate::billing::cache::BillingCache::new(60)),
            public_rate_limiter: Arc::new(crate::auth::rate_limit::PublicRateLimiter::new(2)),
            config,
        };
        let app = public_router().with_state(state);
        let first_ip = SocketAddr::from(([192, 0, 2, 1], 1234));
        let same_ip_new_port = SocketAddr::from(([192, 0, 2, 1], 5678));
        let other_ip = SocketAddr::from(([192, 0, 2, 2], 1234));

        // When requests exhaust one IP, change IP, then omit connection info.
        for (addr, limited) in [
            (Some(first_ip), false),
            (Some(same_ip_new_port), false),
            (Some(first_ip), true),
            (Some(other_ip), false),
            (None, false),
            (None, false),
            (None, false),
        ] {
            let mut request = Request::builder()
                .uri("/public/test-key")
                .body(Body::empty())
                .unwrap();
            if let Some(addr) = addr {
                request.extensions_mut().insert(ConnectInfo(addr));
            }
            let response = app.clone().oneshot(request).await.unwrap();

            // Then only the exhausted IP receives the standard 429 envelope.
            if limited {
                assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                let bytes = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(body["error"]["code"], "rate_limited");
            } else {
                assert_ne!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            }
        }
    }

    #[test]
    fn immutable_cache_control_is_public_and_one_year() {
        assert_eq!(
            IMMUTABLE_CACHE_CONTROL,
            "public, max-age=31536000, immutable"
        );
    }
}
