//! Public (unauthenticated) file download endpoint.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

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

async fn get_public_file(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Response, AppError> {
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

    #[test]
    fn immutable_cache_control_is_public_and_one_year() {
        assert_eq!(
            IMMUTABLE_CACHE_CONTROL,
            "public, max-age=31536000, immutable"
        );
    }
}
