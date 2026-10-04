//! Folder CRUD handlers.

use axum::extract::{Path, Query, State};
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

#[derive(Debug, Serialize, ToSchema)]
pub struct FolderResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    pub name: String,
    /// Omitted or null creates a root folder.
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListFoldersQuery {
    /// Only root folders. Cannot be combined with parent_id.
    pub root: Option<bool>,
    /// Only immediate children of this folder; omitted filters preserve legacy all-folders listing.
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteFolderQuery {
    /// Reject deletion if the folder contains files. Child folders always prevent deletion.
    pub require_empty: Option<bool>,
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
    parent_id: Option<Uuid>,
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

#[utoipa::path(post, path = "/v1/folders", tag = "folders", security(("bearer_auth" = [])), request_body = CreateFolderRequest, responses((status = 201, body = FolderResponse), (status = 400), (status = 404), (status = 409)))]
async fn create_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Json(request): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<FolderResponse>), AppError> {
    validate_folder_name(&request.name)?;
    let mut tx = state.pool.begin().await.map_err(internal_database_error)?;
    if let Some(parent_id) = request.parent_id {
        // Share the parent key lock with the FK check through commit. A concurrent
        // deletion either waits for this child, or wins and returns a clean 404.
        lock_owned_folder(&mut tx, &context.owner, parent_id, false).await?;
    }
    let row = sqlx::query_as::<_, FolderRow>(
        "INSERT INTO folders (owner_type, owner_id, name, parent_id) VALUES ($1, $2, $3, $4) \
         RETURNING id, name, created_at, parent_id",
    )
    .bind(&context.owner.owner_type)
    .bind(&context.owner.owner_id)
    .bind(&request.name)
    .bind(request.parent_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_database_error)?;
    tx.commit().await.map_err(internal_database_error)?;
    Ok((StatusCode::CREATED, Json(folder_response(row)?)))
}

#[utoipa::path(get, path = "/v1/folders", tag = "folders", security(("bearer_auth" = [])), params(ListFoldersQuery), responses((status = 200, body = FolderListResponse), (status = 400), (status = 404)))]
async fn list_folders(
    context: AccountContext,
    State(state): State<AppState>,
    Query(query): Query<ListFoldersQuery>,
) -> Result<Json<FolderListResponse>, AppError> {
    let root = query.root.unwrap_or(false);
    if root && query.parent_id.is_some() {
        return Err(AppError::Validation(
            "root and parent_id cannot be combined".into(),
        ));
    }
    if let Some(parent_id) = query.parent_id {
        find_folder(&state.pool, &context.owner, parent_id).await?;
    }
    let rows = sqlx::query_as::<_, FolderRow>(
        "SELECT id, name, created_at, parent_id FROM folders \
         WHERE owner_type = $1 AND owner_id = $2 \
           AND (NOT $3 OR parent_id IS NULL) \
           AND ($4::uuid IS NULL OR parent_id = $4) \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(&context.owner.owner_type)
    .bind(&context.owner.owner_id)
    .bind(root)
    .bind(query.parent_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal_database_error)?;
    let items = rows
        .into_iter()
        .map(folder_response)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(FolderListResponse { items }))
}

#[utoipa::path(get, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = FolderResponse), (status = 404)))]
async fn get_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FolderResponse>, AppError> {
    let row = find_folder(&state.pool, &context.owner, id).await?;
    Ok(Json(folder_response(row)?))
}

#[utoipa::path(patch, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), request_body = RenameFolderRequest, responses((status = 200, body = FolderResponse), (status = 400), (status = 404), (status = 409)))]
async fn rename_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<RenameFolderRequest>,
) -> Result<Json<FolderResponse>, AppError> {
    validate_folder_name(&request.name)?;
    let row = sqlx::query_as::<_, FolderRow>(
        "UPDATE folders SET name = $1 WHERE id = $2 AND owner_type = $3 AND owner_id = $4 \
         RETURNING id, name, created_at, parent_id",
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

#[utoipa::path(delete, path = "/v1/folders/{id}", tag = "folders", security(("bearer_auth" = [])), params(("id" = Uuid, Path), DeleteFolderQuery), responses((status = 204), (status = 404), (status = 409)))]
async fn delete_folder(
    context: AccountContext,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteFolderQuery>,
) -> Result<StatusCode, AppError> {
    let mut tx = state.pool.begin().await.map_err(internal_database_error)?;
    // FOR UPDATE conflicts with the FK key-share lock taken by both child
    // creation and file insertion. Checking emptiness after acquiring it makes
    // require_empty atomic, including concurrent uploads.
    lock_owned_folder(&mut tx, &context.owner, id, true).await?;
    let nonempty: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE parent_id = $1) \
         OR ($2 AND EXISTS(SELECT 1 FROM files WHERE folder_id = $1))",
    )
    .bind(id)
    .bind(query.require_empty.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await
    .map_err(internal_database_error)?;
    if nonempty {
        return Err(AppError::Conflict("folder is not empty".into()));
    }
    sqlx::query("DELETE FROM folders WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
    tx.commit().await.map_err(internal_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn lock_owned_folder(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &Owner,
    id: Uuid,
    exclusive: bool,
) -> Result<(), AppError> {
    let query = if exclusive {
        "SELECT id FROM folders WHERE id = $1 AND owner_type = $2 AND owner_id = $3 FOR UPDATE"
    } else {
        "SELECT id FROM folders WHERE id = $1 AND owner_type = $2 AND owner_id = $3 FOR KEY SHARE"
    };
    sqlx::query_scalar::<_, Uuid>(query)
        .bind(id)
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(internal_database_error)?
        .ok_or_else(|| AppError::NotFound("folder not found".into()))?;
    Ok(())
}

async fn find_folder(pool: &sqlx::PgPool, owner: &Owner, id: Uuid) -> Result<FolderRow, AppError> {
    sqlx::query_as::<_, FolderRow>(
        "SELECT id, name, created_at, parent_id FROM folders \
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
        parent_id: row.parent_id,
    })
}

fn validate_folder_name(name: &str) -> Result<(), AppError> {
    super::validate_name(name, "folder")
}

fn map_database_error(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(database_error)
            if database_error.code().as_deref() == Some("23505") =>
        {
            AppError::Conflict("folder name already exists".into())
        }
        sqlx::Error::Database(database_error)
            if database_error.code().as_deref() == Some("23503") =>
        {
            AppError::Conflict("folder is not empty".into())
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

#[cfg(test)]
#[path = "folder_hierarchy_tests.rs"]
mod hierarchy_tests;
