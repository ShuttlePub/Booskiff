//! Folder CRUD handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::extractor::AccountContext;
use crate::error::AppError;
use crate::model::Owner;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct FolderResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    pub name: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RenameFolderRequest {
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct FolderListResponse {
    pub items: Vec<FolderResponse>,
}

#[derive(Debug, sqlx::FromRow)]
struct FolderRow {
    id: Uuid,
    name: String,
    created_at: OffsetDateTime,
}

/// Builds the authenticated folder CRUD router.
pub fn folders_router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_folder).get(list_folders))
        .route(
            "/{id}",
            get(get_folder).patch(rename_folder).delete(delete_folder),
        )
}

#[utoipa::path(post, path = "/v1/folders", tag = "folders", security(("bearer_auth" = [])))]
async fn create_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Json(request): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<FolderResponse>), AppError> {
    validate_folder_name(&request.name)?;
    let row = sqlx::query_as::<_, FolderRow>(
        "INSERT INTO folders (owner_type, owner_id, name) VALUES ($1, $2, $3) \
         RETURNING id, name, created_at",
    )
    .bind(&context.owner.owner_type)
    .bind(&context.owner.owner_id)
    .bind(&request.name)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(folder_response(row)?)))
}

#[utoipa::path(get, path = "/v1/folders", tag = "folders", security(("bearer_auth" = [])))]
async fn list_folders(
    context: AccountContext,
    State(state): State<AppState>,
) -> Result<Json<FolderListResponse>, AppError> {
    let rows = sqlx::query_as::<_, FolderRow>(
        "SELECT id, name, created_at FROM folders \
         WHERE owner_type = $1 AND owner_id = $2 ORDER BY created_at ASC",
    )
    .bind(&context.owner.owner_type)
    .bind(&context.owner.owner_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal_database_error)?;
    let items = rows
        .into_iter()
        .map(folder_response)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(FolderListResponse { items }))
}

#[utoipa::path(get, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])))]
async fn get_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FolderResponse>, AppError> {
    let row = find_folder(&state.pool, &context.owner, id).await?;
    Ok(Json(folder_response(row)?))
}

#[utoipa::path(patch, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])))]
async fn rename_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<RenameFolderRequest>,
) -> Result<Json<FolderResponse>, AppError> {
    validate_folder_name(&request.name)?;
    let row = sqlx::query_as::<_, FolderRow>(
        "UPDATE folders SET name = $1 WHERE id = $2 AND owner_type = $3 AND owner_id = $4 \
         RETURNING id, name, created_at",
    )
    .bind(&request.name)
    .bind(id)
    .bind(&context.owner.owner_type)
    .bind(&context.owner.owner_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or_else(|| AppError::NotFound("folder not found".into()))?;
    Ok(Json(folder_response(row)?))
}

#[utoipa::path(delete, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])))]
async fn delete_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result =
        sqlx::query("DELETE FROM folders WHERE id = $1 AND owner_type = $2 AND owner_id = $3")
            .bind(id)
            .bind(&context.owner.owner_type)
            .bind(&context.owner.owner_id)
            .execute(&state.pool)
            .await
            .map_err(internal_database_error)?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("folder not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn find_folder(pool: &sqlx::PgPool, owner: &Owner, id: Uuid) -> Result<FolderRow, AppError> {
    sqlx::query_as::<_, FolderRow>(
        "SELECT id, name, created_at FROM folders \
         WHERE id = $1 AND owner_type = $2 AND owner_id = $3",
    )
    .bind(id)
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_optional(pool)
    .await
    .map_err(internal_database_error)?
    .ok_or_else(|| AppError::NotFound("folder not found".into()))
}

fn folder_response(row: FolderRow) -> Result<FolderResponse, AppError> {
    let created_at = row
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| AppError::Internal(format!("format folder timestamp failed: {err}")))?;
    Ok(FolderResponse {
        id: row.id,
        name: row.name,
        created_at,
    })
}

fn validate_folder_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::Validation("folder name must not be blank".into()));
    }
    if name.chars().count() > 255 {
        return Err(AppError::Validation("folder name is too long".into()));
    }
    Ok(())
}

fn map_database_error(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(database_error)
            if database_error.code().as_deref() == Some("23505") =>
        {
            AppError::Conflict("folder name already exists".into())
        }
        _ => internal_database_error(error),
    }
}

fn internal_database_error(error: sqlx::Error) -> AppError {
    AppError::Internal(format!("folder database operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{find_folder, map_database_error, validate_folder_name};
    use crate::error::AppError;
    use uuid::Uuid;

    #[test]
    fn folder_name_rejects_blank_input() {
        assert!(matches!(
            validate_folder_name("  \n"),
            Err(AppError::Validation(_))
        ));
    }

    #[test]
    fn folder_name_rejects_more_than_255_characters() {
        let name = "a".repeat(256);
        assert!(matches!(
            validate_folder_name(&name),
            Err(AppError::Validation(_))
        ));
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn pg_folder_lifecycle_is_owner_scoped_and_unlinks_files() {
        let url = std::env::var("BOOSKIFF_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff".to_owned());
        let pool = sqlx::PgPool::connect(&url).await.expect("connect postgres");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrate");
        let owner = crate::model::Owner::new("test-folders", Uuid::now_v7().to_string());
        let other = crate::model::Owner::new("test-folders", Uuid::now_v7().to_string());
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO folders (owner_type, owner_id, name) VALUES ($1, $2, 'one') RETURNING id",
        )
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .fetch_one(&pool)
        .await
        .expect("insert folder");
        let duplicate =
            sqlx::query("INSERT INTO folders (owner_type, owner_id, name) VALUES ($1, $2, 'one')")
                .bind(&owner.owner_type)
                .bind(&owner.owner_id)
                .execute(&pool)
                .await
                .expect_err("duplicate folder");
        assert!(matches!(
            map_database_error(duplicate),
            AppError::Conflict(_)
        ));
        assert!(matches!(
            find_folder(&pool, &other, id).await,
            Err(AppError::NotFound(_))
        ));
        sqlx::query("INSERT INTO files (owner_type, owner_id, folder_id, name, mime_type, size_bytes) VALUES ($1, $2, $3, 'f', 'text/plain', 0)")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .bind(id)
            .execute(&pool)
            .await
            .expect("insert file");
        sqlx::query("DELETE FROM folders WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("delete folder");
        assert!(
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT folder_id FROM files WHERE owner_id = $1",
            )
            .bind(&owner.owner_id)
            .fetch_one(&pool)
            .await
            .expect("file lookup")
            .is_none()
        );
        sqlx::query("DELETE FROM files WHERE owner_id = $1")
            .bind(&owner.owner_id)
            .execute(&pool)
            .await
            .expect("cleanup file");
    }
}
