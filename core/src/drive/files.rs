// allow: SIZE_OK — this file owns the complete files HTTP resource; the task
// contract freezes both implementation files and requires all seven handlers,
// DTOs, upload transaction, OpenAPI annotations, and ignored integration tests.
//! File CRUD, listing, streaming upload, publishing, and download URL handlers.

use std::time::Duration;

use aws_smithy_types::body::SdkBody;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::auth::extractor::AccountContext;
use crate::billing::usage::{add_used_bytes, subtract_used_bytes, used_bytes};
use crate::error::AppError;
use crate::model::{OBJECT_KIND_ORIGINAL, Owner};
use crate::state::AppState;
use crate::storage::Storage;

use super::upload_body::CountingBody;

const DEFAULT_LIST_LIMIT: i64 = 50;
const MAX_LIST_LIMIT: i64 = 200;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct FileResponse {
    pub id: Uuid,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub folder_id: Option<Uuid>,
    pub is_public: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct FileListResponse {
    pub items: Vec<FileResponse>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UrlResponse {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    name: Option<String>,
    mime: Option<String>,
    folder_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    folder_id: Option<Uuid>,
}

#[derive(Debug, FromRow)]
struct FileRow {
    id: Uuid,
    owner_type: String,
    owner_id: String,
    name: String,
    mime_type: String,
    size_bytes: i64,
    folder_id: Option<Uuid>,
    is_public: bool,
    created_at: time::OffsetDateTime,
}

struct UploadedFile {
    id: Uuid,
    folder_id: Option<Uuid>,
    name: String,
    mime: String,
    key: String,
    size_bytes: i64,
}

impl FileRow {
    fn into_response(self, owner: &Owner) -> Result<FileResponse, AppError> {
        if self.owner_type != owner.owner_type || self.owner_id != owner.owner_id {
            return Err(AppError::NotFound(format!("file {}", self.id)));
        }
        Ok(FileResponse {
            id: self.id,
            name: self.name,
            mime_type: self.mime_type,
            size_bytes: self.size_bytes,
            folder_id: self.folder_id,
            is_public: self.is_public,
            created_at: self
                .created_at
                .format(&Rfc3339)
                .map_err(|err| AppError::Internal(format!("format file timestamp: {err}")))?,
        })
    }
}

/// CountingBody in upload_file is the sole body-size enforcement path.
/// DefaultBodyLimit would be a no-op because upload_file extracts Body directly.
pub fn upload_router() -> Router<AppState> {
    Router::new().route("/v1/files", post(upload_file))
}

pub fn files_router() -> Router<AppState> {
    Router::new()
        .route("/v1/files", get(list_files))
        .route("/v1/files/{id}", get(get_file).delete(delete_file))
        .route("/v1/files/{id}/download-url", get(download_url))
        .route(
            "/v1/files/{id}/publish",
            post(publish_file).delete(unpublish_file),
        )
}

#[utoipa::path(post, path = "/v1/files", tag = "files", security(("bearer_auth" = [])), responses((status = 201, body = FileResponse)))]
async fn upload_file(
    ctx: AccountContext,
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<FileResponse>), AppError> {
    let name = query
        .name
        .ok_or_else(|| AppError::Validation("file name is required".into()))?;
    super::validate_name(&name, "file")?;
    let mime = query
        .mime
        .filter(|mime| !mime.trim().is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    if let Some(folder_id) = query.folder_id {
        ensure_owned_folder(&state.pool, &ctx.owner, folder_id).await?;
    }
    let declared = parse_content_length(&headers)?;
    if declared > ctx.limits.max_file_bytes {
        return Err(AppError::PayloadTooLarge(
            "file exceeds maximum size".into(),
        ));
    }
    let current_usage = used_bytes(&state.pool, &ctx.owner).await?;
    if current_usage.saturating_add(declared) > ctx.limits.storage_quota_bytes {
        return Err(AppError::InsufficientStorage(
            "storage quota exceeded".into(),
        ));
    }

    let file_id = Uuid::now_v7();
    let key = Storage::object_key(&ctx.owner, &file_id, OBJECT_KIND_ORIGINAL);
    let cap = declared.min(ctx.limits.max_file_bytes);
    let cap = u64::try_from(cap)
        .map_err(|_| AppError::Validation("content length must be non-negative".into()))?;
    let declared_u64 = u64::try_from(declared)
        .map_err(|_| AppError::Validation("content length must be non-negative".into()))?;
    let (counting_body, count) = CountingBody::new(body, cap, declared_u64);
    let put_result = state
        .s3
        .put_streaming(&key, SdkBody::from_body_1_x(counting_body), declared, &mime)
        .await;
    if let Err(error) = put_result {
        let counted = count.load();
        let _ = state.s3.delete_object(&key).await;
        if counted > u64::try_from(ctx.limits.max_file_bytes).unwrap_or(u64::MAX) {
            return Err(AppError::PayloadTooLarge(
                "file exceeds maximum size".into(),
            ));
        }
        if counted > declared_u64 {
            return Err(AppError::Validation(
                "body exceeds declared content length".into(),
            ));
        }
        return Err(error);
    }
    if count.load() != declared_u64 {
        let _ = state.s3.delete_object(&key).await;
        return Err(AppError::Validation(
            "body length does not match content length".into(),
        ));
    }

    let response = persist_uploaded_file(
        &state,
        &ctx,
        UploadedFile {
            id: file_id,
            folder_id: query.folder_id,
            name,
            mime,
            key: key.clone(),
            size_bytes: declared,
        },
    )
    .await;
    if response.is_err() {
        let _ = state.s3.delete_object(&key).await;
    }
    response.map(|file| (StatusCode::CREATED, Json(file)))
}

#[utoipa::path(get, path = "/v1/files", tag = "files", security(("bearer_auth" = [])), responses((status = 200, body = FileListResponse)))]
async fn list_files(
    ctx: AccountContext,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<FileListResponse>, AppError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    let offset = query.offset.unwrap_or(0);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(AppError::Validation(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }
    if offset < 0 {
        return Err(AppError::Validation("offset must be non-negative".into()));
    }
    let rows = sqlx::query_as::<_, FileRow>(
        "SELECT id, owner_type, owner_id, name, mime_type, size_bytes, folder_id, is_public, created_at \
         FROM files WHERE owner_type = $1 AND owner_id = $2 \
           AND ($3::uuid IS NULL OR folder_id = $3) \
         ORDER BY created_at DESC LIMIT $4 OFFSET $5",
    )
    .bind(&ctx.owner.owner_type)
    .bind(&ctx.owner.owner_id)
    .bind(query.folder_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("list files: {err}")))?;
    let items = rows
        .into_iter()
        .map(|row| row.into_response(&ctx.owner))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(FileListResponse { items }))
}

#[utoipa::path(get, path = "/v1/files/{id}", tag = "files", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = FileResponse), (status = 404)))]
async fn get_file(
    ctx: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FileResponse>, AppError> {
    load_owned_file(&state.pool, &ctx.owner, id)
        .await?
        .into_response(&ctx.owner)
        .map(Json)
}

#[utoipa::path(delete, path = "/v1/files/{id}", tag = "files", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 204), (status = 404)))]
async fn delete_file(
    ctx: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let row = load_owned_file(&state.pool, &ctx.owner, id).await?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|err| AppError::Internal(format!("begin delete file: {err}")))?;
    let objects = sqlx::query("DELETE FROM file_objects WHERE file_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|err| AppError::Internal(format!("delete file objects: {err}")))?;
    if objects.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("file {id}")));
    }
    let row_delete = sqlx::query("DELETE FROM files WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|err| AppError::Internal(format!("delete file row: {err}")))?;
    // A loser of a concurrent delete unblocks with zero rows; this guard
    // prevents a double quota refund (commit-dropped tx rolls back).
    if row_delete.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("file {id}")));
    }
    subtract_used_bytes(&mut tx, &ctx.owner, row.size_bytes).await?;
    tx.commit()
        .await
        .map_err(|err| AppError::Internal(format!("commit delete file: {err}")))?;
    let prefix = format!("{}/{}/{id}/", ctx.owner.owner_type, ctx.owner.owner_id);
    let _ = state.s3.delete_prefix(&prefix).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/files/{id}/download-url", tag = "files", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = UrlResponse), (status = 404)))]
async fn download_url(
    ctx: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<UrlResponse>, AppError> {
    load_owned_file(&state.pool, &ctx.owner, id).await?;
    let key: Option<String> = sqlx::query_scalar(
        "SELECT storage_key FROM file_objects WHERE file_id = $1 AND object_kind = $2",
    )
    .bind(id)
    .bind(OBJECT_KIND_ORIGINAL)
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("load original object: {err}")))?;
    let key = key.ok_or_else(|| AppError::NotFound(format!("file object {id}")))?;
    let ttl = Duration::from_secs(state.config.presigned_get_ttl_secs);
    let url = state.s3.presign_get(&key, ttl).await?;
    Ok(Json(UrlResponse { url }))
}

#[utoipa::path(post, path = "/v1/files/{id}/publish", tag = "files", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = UrlResponse), (status = 404)))]
async fn publish_file(
    ctx: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<UrlResponse>, AppError> {
    load_owned_file(&state.pool, &ctx.owner, id).await?;
    let mut bytes = [0_u8; 32];
    rand::fill(&mut bytes);
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let result = sqlx::query(
        "UPDATE files SET is_public = TRUE, public_key = $4 \
         WHERE id = $1 AND owner_type = $2 AND owner_id = $3",
    )
    .bind(id)
    .bind(&ctx.owner.owner_type)
    .bind(&ctx.owner.owner_id)
    .bind(&key)
    .execute(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("publish file: {err}")))?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("file {id}")));
    }
    let base = state.config.public_base_url.trim_end_matches('/');
    Ok(Json(UrlResponse {
        url: format!("{base}/public/{key}"),
    }))
}

#[utoipa::path(delete, path = "/v1/files/{id}/publish", tag = "files", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 204), (status = 404)))]
async fn unpublish_file(
    ctx: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result = sqlx::query(
        "UPDATE files SET is_public = FALSE, public_key = NULL \
         WHERE id = $1 AND owner_type = $2 AND owner_id = $3",
    )
    .bind(id)
    .bind(&ctx.owner.owner_type)
    .bind(&ctx.owner.owner_id)
    .execute(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("unpublish file: {err}")))?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("file {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn parse_content_length(headers: &HeaderMap) -> Result<i64, AppError> {
    let raw = headers
        .get(header::CONTENT_LENGTH)
        .ok_or_else(|| AppError::LengthRequired("content-length header is required".into()))?
        .to_str()
        .map_err(|_| AppError::Validation("invalid content-length header".into()))?;
    let declared = raw
        .parse::<i64>()
        .map_err(|_| AppError::Validation("invalid content-length header".into()))?;
    if declared < 0 {
        return Err(AppError::Validation(
            "content length must be non-negative".into(),
        ));
    }
    Ok(declared)
}

async fn ensure_owned_folder(
    pool: &sqlx::PgPool,
    owner: &Owner,
    folder_id: Uuid,
) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE id = $1 AND owner_type = $2 AND owner_id = $3)",
    )
    .bind(folder_id)
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_one(pool)
    .await
    .map_err(|err| AppError::Internal(format!("check upload folder: {err}")))?;
    if exists {
        Ok(())
    } else {
        Err(AppError::NotFound(format!("folder {folder_id}")))
    }
}

async fn load_owned_file(
    pool: &sqlx::PgPool,
    owner: &Owner,
    id: Uuid,
) -> Result<FileRow, AppError> {
    sqlx::query_as::<_, FileRow>(
        "SELECT id, owner_type, owner_id, name, mime_type, size_bytes, folder_id, is_public, created_at \
         FROM files WHERE id = $1 AND owner_type = $2 AND owner_id = $3",
    )
    .bind(id)
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| AppError::Internal(format!("load file: {err}")))?
    .ok_or_else(|| AppError::NotFound(format!("file {id}")))
}

async fn persist_uploaded_file(
    state: &AppState,
    ctx: &AccountContext,
    upload: UploadedFile,
) -> Result<FileResponse, AppError> {
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|err| AppError::Internal(format!("begin upload transaction: {err}")))?;
    let row = sqlx::query_as::<_, FileRow>(
        "INSERT INTO files \
         (id, owner_type, owner_id, folder_id, name, mime_type, size_bytes, is_public, public_key) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE, NULL) \
         RETURNING id, owner_type, owner_id, name, mime_type, size_bytes, folder_id, is_public, created_at",
    )
    .bind(upload.id)
    .bind(&ctx.owner.owner_type)
    .bind(&ctx.owner.owner_id)
    .bind(upload.folder_id)
    .bind(upload.name)
    .bind(&upload.mime)
    .bind(upload.size_bytes)
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| map_insert_file_error(err, upload.folder_id))?;
    sqlx::query(
        "INSERT INTO file_objects \
         (file_id, object_kind, storage_key, size_bytes, mime_type) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(upload.id)
    .bind(OBJECT_KIND_ORIGINAL)
    .bind(upload.key)
    .bind(upload.size_bytes)
    .bind(upload.mime)
    .execute(&mut *tx)
    .await
    .map_err(|err| AppError::Internal(format!("insert file object: {err}")))?;
    let new_total = add_used_bytes(&mut tx, &ctx.owner, upload.size_bytes).await?;
    if new_total > ctx.limits.storage_quota_bytes {
        tx.rollback()
            .await
            .map_err(|err| AppError::Internal(format!("rollback upload quota: {err}")))?;
        return Err(AppError::InsufficientStorage(
            "storage quota exceeded".into(),
        ));
    }
    let response = row.into_response(&ctx.owner)?;
    tx.commit()
        .await
        .map_err(|err| AppError::Internal(format!("commit upload: {err}")))?;
    Ok(response)
}

fn map_insert_file_error(error: sqlx::Error, folder_id: Option<Uuid>) -> AppError {
    match (&error, folder_id) {
        (sqlx::Error::Database(database_error), Some(folder_id))
            if database_error.code().as_deref() == Some("23503") =>
        {
            AppError::NotFound(format!("folder {folder_id}"))
        }
        _ => AppError::Internal(format!("insert file: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn upload_returns_413_when_declared_length_exceeds_limit() {
        // Given: a closed pool makes any database access fail.
        let config = Config::default();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        pool.close().await;
        let storage = Storage::build(&config).await.unwrap();
        let state = test_state(config, pool, storage).await;
        let ctx = AccountContext {
            owner: Owner::new("test-files", Uuid::now_v7().to_string()),
            limits: crate::model::Limits {
                storage_quota_bytes: 1024,
                max_file_bytes: 4,
                rate_limit_rpm: 100,
            },
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "5".parse().unwrap());

        // When
        let error = upload_file(
            ctx,
            State(state),
            Query(UploadQuery {
                name: Some("large.txt".into()),
                mime: None,
                folder_id: None,
            }),
            headers,
            Body::empty(),
        )
        .await
        .unwrap_err();

        // Then
        assert!(matches!(error, AppError::PayloadTooLarge(_)));
        assert_eq!(
            error.into_response().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    #[ignore = "requires compose postgres and MinIO"]
    async fn upload_returns_413_when_actual_body_exceeds_limit() {
        // Given: the declared length fits exactly, but the body exceeds the cap.
        let config = Config::default();
        let pool = test_pool().await;
        let storage = Storage::build(&config).await.unwrap();
        storage.ensure_bucket().await.unwrap();
        let state = test_state(config, pool, storage).await;
        let ctx = AccountContext {
            owner: Owner::new("test-files", Uuid::now_v7().to_string()),
            limits: crate::model::Limits {
                storage_quota_bytes: 1024,
                max_file_bytes: 4,
                rate_limit_rpm: 100,
            },
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "4".parse().unwrap());

        // When
        let error = upload_file(
            ctx,
            State(state),
            Query(UploadQuery {
                name: Some("large.txt".into()),
                mime: None,
                folder_id: None,
            }),
            headers,
            Body::from("12345"),
        )
        .await
        .unwrap_err();

        // Then
        assert!(matches!(error, AppError::PayloadTooLarge(_)));
        assert_eq!(
            error.into_response().status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn pg_upload_into_vanished_folder_is_not_found() {
        // Given: the folder ID does not exist in PostgreSQL.
        let config = Config::default();
        let pool = test_pool().await;
        let storage = Storage::build(&config).await.unwrap();
        let state = test_state(config, pool, storage).await;
        let folder_id = Uuid::now_v7();
        let ctx = AccountContext {
            owner: Owner::new("test-drive-files", Uuid::now_v7().to_string()),
            limits: crate::model::Limits {
                storage_quota_bytes: 1024,
                max_file_bytes: 1024,
                rate_limit_rpm: 100,
            },
        };

        // When
        let error = persist_uploaded_file(
            &state,
            &ctx,
            UploadedFile {
                id: Uuid::now_v7(),
                folder_id: Some(folder_id),
                name: "vanished-folder.txt".into(),
                mime: "text/plain".into(),
                key: "test-drive-files/vanished-folder.txt".into(),
                size_bytes: 1,
            },
        )
        .await
        .unwrap_err();

        // Then
        assert!(matches!(error, AppError::NotFound(_)));
    }

    #[test]
    fn foreign_file_row_maps_to_not_found() {
        // Given
        let owner = Owner::new("account", "alice");
        let row = FileRow {
            id: Uuid::now_v7(),
            owner_type: "account".into(),
            owner_id: "bob".into(),
            name: "secret.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 4,
            folder_id: None,
            is_public: false,
            created_at: time::OffsetDateTime::now_utc(),
        };

        // When
        let error = row.into_response(&owner).unwrap_err();

        // Then
        assert!(matches!(error, AppError::NotFound(_)));
    }

    #[tokio::test]
    #[ignore = "requires compose postgres and MinIO"]
    async fn upload_and_delete_roundtrip_ignored() {
        let config = Config::default();
        let pool = test_pool().await;
        let storage = Storage::build(&config).await.unwrap();
        storage.ensure_bucket().await.unwrap();
        let owner = Owner::new("test-files", Uuid::now_v7().to_string());
        let payload = b"handler integration";
        let state = test_state(config, pool.clone(), storage.clone()).await;
        let ctx = AccountContext {
            owner: owner.clone(),
            limits: crate::model::Limits {
                storage_quota_bytes: 1024,
                max_file_bytes: 1024,
                rate_limit_rpm: 100,
            },
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_LENGTH,
            payload.len().to_string().parse().unwrap(),
        );

        let (_, Json(created)) = upload_file(
            ctx,
            State(state.clone()),
            Query(UploadQuery {
                name: Some("roundtrip.txt".into()),
                mime: Some("text/plain".into()),
                folder_id: None,
            }),
            headers,
            Body::from(payload.as_slice()),
        )
        .await
        .unwrap();
        assert_eq!(created.name, "roundtrip.txt");
        assert_eq!(used_bytes(&pool, &owner).await.unwrap(), 19);

        let Json(download) = download_url(
            AccountContext {
                owner: owner.clone(),
                limits: crate::model::Limits {
                    storage_quota_bytes: 1024,
                    max_file_bytes: 1024,
                    rate_limit_rpm: 100,
                },
            },
            State(state.clone()),
            Path(created.id),
        )
        .await
        .unwrap();
        assert!(download.url.contains(&created.id.to_string()));

        let status = delete_file(
            AccountContext {
                owner: owner.clone(),
                limits: crate::model::Limits {
                    storage_quota_bytes: 1024,
                    max_file_bytes: 1024,
                    rate_limit_rpm: 100,
                },
            },
            State(state),
            Path(created.id),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(used_bytes(&pool, &owner).await.unwrap(), 0);
    }

    async fn test_state(config: Config, pool: sqlx::PgPool, s3: Storage) -> AppState {
        AppState {
            billing_cache: std::sync::Arc::new(crate::billing::cache::BillingCache::new(
                config.billing_cache_ttl_secs,
            )),
            jwks_cache: crate::auth::jwks::JwksCache::new(config.jwt_trusted_issuers.clone()),
            pool,
            s3,
            config,
            rate_limiters: std::sync::Arc::new(crate::state::RateLimiters::default()),
            public_rate_limiter: std::sync::Arc::new(
                crate::auth::rate_limit::PublicRateLimiter::new(300),
            ),
        }
    }

    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("BOOSKIFF_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff".to_owned());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap_or_else(|err| panic!("PG test needs postgres at {url}: {err}"));
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }
}
