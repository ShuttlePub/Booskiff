//! PostgreSQL-backed handler and migration tests for the hierarchy contract.
use super::*;
use crate::drive::test_support::{context, setup};

async fn create(
    state: &AppState,
    owner: &Owner,
    name: &str,
    parent_id: Option<Uuid>,
) -> FolderResponse {
    create_folder(
        context(owner),
        State(state.clone()),
        Json(CreateFolderRequest {
            name: name.into(),
            parent_id,
        }),
    )
    .await
    .unwrap()
    .1
    .0
}

async fn remove(
    state: &AppState,
    owner: &Owner,
    id: Uuid,
    require_empty: bool,
) -> Result<StatusCode, AppError> {
    delete_folder(
        context(owner),
        State(state.clone()),
        Path(id),
        Query(DeleteFolderQuery {
            require_empty: Some(require_empty),
        }),
    )
    .await
}

async fn list(
    state: &AppState,
    owner: &Owner,
    root: Option<bool>,
    parent_id: Option<Uuid>,
) -> Result<Vec<FolderResponse>, AppError> {
    list_folders(
        context(owner),
        State(state.clone()),
        Query(ListFoldersQuery { root, parent_id }),
    )
    .await
    .map(|result| result.0.items)
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_hierarchy_lists_direct_children_and_scopes_sibling_names() {
    let (state, owner) = setup("hierarchy").await;
    let a = create(&state, &owner, "A", None).await;
    let b = create(&state, &owner, "B", None).await;
    let child = create(&state, &owner, "documents", Some(a.id)).await;
    let same_name = create(&state, &owner, "documents", Some(b.id)).await;
    let grandchild = create(&state, &owner, "design", Some(child.id)).await;
    assert_eq!(child.parent_id, Some(a.id));
    let roots = list(&state, &owner, Some(true), None).await.unwrap();
    assert_eq!(
        roots.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![a.id, b.id]
    );
    let children = list(&state, &owner, None, Some(a.id)).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, child.id);
    assert_eq!(list(&state, &owner, None, None).await.unwrap().len(), 5);
    assert_eq!(
        list(&state, &owner, Some(false), None).await.unwrap().len(),
        5
    );
    assert!(matches!(
        list(&state, &owner, Some(true), Some(a.id)).await,
        Err(AppError::Validation(_))
    ));
    for parent_id in [None, Some(a.id)] {
        let name = if parent_id.is_some() {
            "documents"
        } else {
            "A"
        };
        assert!(matches!(
            create_folder(
                context(&owner),
                State(state.clone()),
                Json(CreateFolderRequest {
                    name: name.into(),
                    parent_id
                })
            )
            .await,
            Err(AppError::Conflict(_))
        ));
    }
    let sibling = create(&state, &owner, "sibling", Some(a.id)).await;
    assert!(matches!(
        rename_folder(
            context(&owner),
            State(state.clone()),
            Path(sibling.id),
            Json(RenameFolderRequest {
                name: "documents".into()
            })
        )
        .await,
        Err(AppError::Conflict(_))
    ));
    // Renaming preserves the parent; a same-name folder in a different parent is valid.
    let renamed = rename_folder(
        context(&owner),
        State(state.clone()),
        Path(sibling.id),
        Json(RenameFolderRequest {
            name: "design".into(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(renamed.parent_id, Some(a.id));
    for id in [
        grandchild.id,
        sibling.id,
        child.id,
        same_name.id,
        a.id,
        b.id,
    ] {
        assert_eq!(
            remove(&state, &owner, id, true).await.unwrap(),
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_hierarchy_hides_foreign_parents_and_enforces_database_ownership() {
    let (state, owner) = setup("owner-check").await;
    let parent = create(&state, &owner, "parent", None).await;
    let foreign = Owner::new(&owner.owner_type, Uuid::now_v7().to_string());
    for parent_id in [parent.id, Uuid::now_v7()] {
        assert!(matches!(
            create_folder(
                context(&foreign),
                State(state.clone()),
                Json(CreateFolderRequest {
                    name: "child".into(),
                    parent_id: Some(parent_id)
                })
            )
            .await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            list(&state, &foreign, None, Some(parent_id)).await,
            Err(AppError::NotFound(_))
        ));
        assert!(matches!(
            remove(&state, &foreign, parent_id, true).await,
            Err(AppError::NotFound(_))
        ));
    }
    let error = sqlx::query("INSERT INTO folders (owner_type, owner_id, name, parent_id) VALUES ($1, $2, 'foreign', $3)")
        .bind(&foreign.owner_type).bind(&foreign.owner_id).bind(parent.id).execute(&state.pool).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    let error = sqlx::query("UPDATE folders SET parent_id = id WHERE id = $1")
        .bind(parent.id)
        .execute(&state.pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    remove(&state, &owner, parent.id, true).await.unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_safe_delete_rejects_contents_and_legacy_delete_unlinks_files() {
    let (state, owner) = setup("safe-delete").await;
    let parent = create(&state, &owner, "parent", None).await;
    let child = create(&state, &owner, "child", Some(parent.id)).await;
    // Both APIs reject folders containing subfolders; neither recursively removes them.
    for require_empty in [false, true] {
        assert!(matches!(
            remove(&state, &owner, parent.id, require_empty).await,
            Err(AppError::Conflict(_))
        ));
    }
    let file: Uuid = sqlx::query_scalar("INSERT INTO files (owner_type, owner_id, folder_id, name, mime_type, size_bytes) VALUES ($1, $2, $3, 'f', 'text/plain', 0) RETURNING id")
        .bind(&owner.owner_type).bind(&owner.owner_id).bind(child.id).fetch_one(&state.pool).await.unwrap();
    assert!(matches!(
        remove(&state, &owner, child.id, true).await,
        Err(AppError::Conflict(_))
    ));
    remove(&state, &owner, child.id, false).await.unwrap();
    let folder_id: Option<Uuid> = sqlx::query_scalar("SELECT folder_id FROM files WHERE id = $1")
        .bind(file)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(folder_id.is_none());
    sqlx::query("DELETE FROM files WHERE id = $1")
        .bind(file)
        .execute(&state.pool)
        .await
        .unwrap();
    remove(&state, &owner, parent.id, true).await.unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_child_creation_and_safe_delete_have_atomic_outcomes() {
    let (state, owner) = setup("child-race").await;
    for round in 0..16 {
        let parent = create(&state, &owner, &format!("parent-{round}"), None).await;
        let (created, deleted) = tokio::join!(
            create_folder(
                context(&owner),
                State(state.clone()),
                Json(CreateFolderRequest {
                    name: "child".into(),
                    parent_id: Some(parent.id)
                })
            ),
            remove(&state, &owner, parent.id, true),
        );
        match (created, deleted) {
            (Ok((_, Json(child))), Err(AppError::Conflict(_))) => {
                remove(&state, &owner, child.id, true).await.unwrap();
                remove(&state, &owner, parent.id, true).await.unwrap();
            }
            (Err(AppError::NotFound(_)), Ok(StatusCode::NO_CONTENT)) => {}
            other => panic!("non-atomic child/delete result: {other:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_file_insertion_and_safe_delete_never_silently_move_files_to_root() {
    let (state, owner) = setup("file-race").await;
    for round in 0..16 {
        let parent = create(&state, &owner, &format!("parent-{round}"), None).await;
        // This INSERT acquires exactly the FK key-share lock used by upload persistence.
        let insert = sqlx::query_scalar::<_, Uuid>("INSERT INTO files (owner_type, owner_id, folder_id, name, mime_type, size_bytes) VALUES ($1, $2, $3, 'f', 'text/plain', 0) RETURNING id")
            .bind(&owner.owner_type).bind(&owner.owner_id).bind(parent.id).fetch_one(&state.pool);
        let (created, deleted) = tokio::join!(insert, remove(&state, &owner, parent.id, true));
        match (created, deleted) {
            (Ok(id), Err(AppError::Conflict(_))) => {
                let folder_id: Option<Uuid> =
                    sqlx::query_scalar("SELECT folder_id FROM files WHERE id = $1")
                        .bind(id)
                        .fetch_one(&state.pool)
                        .await
                        .unwrap();
                assert_eq!(folder_id, Some(parent.id));
                sqlx::query("DELETE FROM files WHERE id = $1")
                    .bind(id)
                    .execute(&state.pool)
                    .await
                    .unwrap();
                remove(&state, &owner, parent.id, true).await.unwrap();
            }
            (Err(error), Ok(StatusCode::NO_CONTENT)) => assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("23503")
            ),
            other => panic!("non-atomic file/delete result: {other:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_hierarchy_migration_preserves_existing_root_folders_and_files() {
    let (state, _) = setup("migration").await;
    let mut tx = state.pool.begin().await.unwrap();
    let schema = format!("migration_{}", Uuid::now_v7().simple());
    // Identifier contains only a fixed prefix and generated hexadecimal UUID.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {schema}; SET LOCAL search_path TO {schema}"
    )))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/00001_folders.sql"))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/00002_files.sql"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let folder: Uuid = sqlx::query_scalar("INSERT INTO folders (owner_type, owner_id, name) VALUES ('account', 'alice', 'before') RETURNING id").fetch_one(&mut *tx).await.unwrap();
    let file: Uuid = sqlx::query_scalar("INSERT INTO files (owner_type, owner_id, folder_id, name, mime_type, size_bytes) VALUES ('account', 'alice', $1, 'before.txt', 'text/plain', 12) RETURNING id").bind(folder).fetch_one(&mut *tx).await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/00008_folder_hierarchy.sql"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let row: (Option<Uuid>, String) =
        sqlx::query_as("SELECT parent_id, name FROM folders WHERE id = $1")
            .bind(folder)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(row, (None, "before".into()));
    let row: (Option<Uuid>, i64) =
        sqlx::query_as("SELECT folder_id, size_bytes FROM files WHERE id = $1")
            .bind(file)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(row, (Some(folder), 12));
    // The test schema and all fixtures disappear together.
    tx.rollback().await.unwrap();
}
