//! Application error type and its HTTP mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Errors convertible into HTTP responses across all modules.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    LengthRequired(String),
    #[error("{0}")]
    InsufficientStorage(String),
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("{0}")]
    Internal(String),
}

/// JSON body shape: `{"error":{"code":...,"message":...}}`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Validation(_) => (StatusCode::BAD_REQUEST, "validation"),
            Self::PayloadTooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            Self::LengthRequired(_) => (StatusCode::LENGTH_REQUIRED, "length_required"),
            Self::InsufficientStorage(_) => {
                (StatusCode::INSUFFICIENT_STORAGE, "insufficient_storage")
            }
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let message = match self {
            Self::Internal(source) => {
                tracing::error!(error = %source, "internal error");
                "internal error".to_owned()
            }
            other => other.to_string(),
        };
        let body = ErrorBody {
            error: ErrorDetail { code, message },
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use rstest::rstest;

    #[rstest]
    #[case(AppError::Unauthorized("no token".into()), 401, "unauthorized")]
    #[case(AppError::Forbidden("not yours".into()), 403, "forbidden")]
    #[case(AppError::NotFound("missing".into()), 404, "not_found")]
    #[case(AppError::Conflict("exists".into()), 409, "conflict")]
    #[case(AppError::Validation("bad name".into()), 400, "validation")]
    #[case(AppError::PayloadTooLarge("too big".into()), 413, "payload_too_large")]
    #[case(AppError::LengthRequired("no length".into()), 411, "length_required")]
    #[case(AppError::InsufficientStorage("quota exceeded".into()), 507, "insufficient_storage")]
    #[case(AppError::RateLimited, 429, "rate_limited")]
    #[case(AppError::Internal("db exploded".into()), 500, "internal")]
    #[tokio::test]
    async fn maps_variant_to_status_and_code(
        #[case] error: AppError,
        #[case] status: u16,
        #[case] code: &str,
    ) {
        let response = error.into_response();
        assert_eq!(response.status().as_u16(), status);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], code);
    }

    #[tokio::test]
    async fn internal_variant_hides_details_from_client() {
        let response = AppError::Internal("db exploded".into()).into_response();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "internal");
        assert_eq!(body["error"]["message"], "internal error");
    }
}
