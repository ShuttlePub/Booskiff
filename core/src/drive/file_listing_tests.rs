use super::*;
use crate::drive::test_support::{context, setup};

async fn list(
    state: &AppState,
    owner: &Owner,
    query: ListQuery,
) -> Result<Vec<FileResponse>, AppError> {
    list_files(context(owner), State(state.clone()), Query(query))
        .await
        .map(|response| response.0.items)
}

#[tokio::test]
#[ignore = "requires PostgreSQL (BOOSKIFF_DATABASE_URL)"]
async fn pg_file_root_and_child_filters_paginate_without_dropping_equal_timestamp_rows() {
    let (state, owner) = setup("root-files").await;
    let folder: Uuid = sqlx::query_scalar(
        "INSERT INTO folders (owner_type, owner_id, name) VALUES ($1, $2, 'folder') RETURNING id",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let child: Uuid = sqlx::query_scalar("INSERT INTO folders (owner_type, owner_id, name, parent_id) VALUES ($1, $2, 'child', $3) RETURNING id")
        .bind(&owner.owner_type).bind(&owner.owner_id).bind(folder).fetch_one(&state.pool).await.unwrap();
    // One timestamp for every row exercises the id tie-breaker across both pages.
    let timestamp = time::OffsetDateTime::now_utc();
    let mut root_ids = Vec::new();
    for i in 0..205 {
        let id: Uuid = sqlx::query_scalar("INSERT INTO files (owner_type, owner_id, name, mime_type, size_bytes, created_at) VALUES ($1, $2, $3, 'text/plain', 1, $4) RETURNING id")
            .bind(&owner.owner_type).bind(&owner.owner_id).bind(format!("root-{i}"))
            .bind(timestamp).fetch_one(&state.pool).await.unwrap();
        root_ids.push(id);
    }
    for folder_id in [folder, child] {
        sqlx::query("INSERT INTO files (owner_type, owner_id, folder_id, name, mime_type, size_bytes, created_at) VALUES ($1, $2, $3, 'nested', 'text/plain', 1, $4)")
            .bind(&owner.owner_type).bind(&owner.owner_id).bind(folder_id).bind(timestamp)
            .execute(&state.pool).await.unwrap();
    }
    let first = list(
        &state,
        &owner,
        ListQuery {
            root: Some(true),
            limit: Some(200),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let second = list(
        &state,
        &owner,
        ListQuery {
            root: Some(true),
            limit: Some(200),
            offset: Some(200),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(first.len(), 200);
    assert_eq!(second.len(), 5);
    assert!(first.iter().chain(&second).all(|f| f.folder_id.is_none()));
    root_ids.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        first
            .iter()
            .chain(&second)
            .map(|f| f.id)
            .collect::<Vec<_>>(),
        root_ids
    );
    let direct = list(
        &state,
        &owner,
        ListQuery {
            folder_id: Some(folder),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].folder_id, Some(folder));
    let legacy = list(&state, &owner, ListQuery::default()).await.unwrap();
    assert_eq!(legacy.len(), 50);
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM files WHERE owner_id = $1")
        .bind(&owner.owner_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(total, 207);
    let legacy_tail = list(
        &state,
        &owner,
        ListQuery {
            offset: Some(200),
            limit: Some(200),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(legacy_tail.len(), 7);
    let foreign = Owner::new(&owner.owner_type, Uuid::now_v7().to_string());
    assert!(
        list(
            &state,
            &foreign,
            ListQuery {
                root: Some(true),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .is_empty()
    );
    for id in [folder, Uuid::now_v7()] {
        assert!(matches!(
            list(
                &state,
                &foreign,
                ListQuery {
                    folder_id: Some(id),
                    ..Default::default()
                }
            )
            .await,
            Err(AppError::NotFound(_))
        ));
    }
    for query in [
        ListQuery {
            root: Some(true),
            folder_id: Some(folder),
            ..Default::default()
        },
        ListQuery {
            limit: Some(0),
            ..Default::default()
        },
        ListQuery {
            limit: Some(201),
            ..Default::default()
        },
        ListQuery {
            offset: Some(-1),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            list(&state, &owner, query).await,
            Err(AppError::Validation(_))
        ));
    }
    let last = first.last().unwrap();
    // A deletion in an already consumed page shifts legacy offsets, but the
    // immutable (created_at, id) boundary must still return every remaining file.
    sqlx::query("DELETE FROM files WHERE id = $1")
        .bind(first[0].id)
        .execute(&state.pool)
        .await
        .unwrap();
    let after_delete = list(
        &state,
        &owner,
        ListQuery {
            root: Some(true),
            limit: Some(200),
            before_created_at: Some(last.created_at.clone()),
            before_id: Some(last.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        after_delete.iter().map(|f| f.id).collect::<Vec<_>>(),
        second.iter().map(|f| f.id).collect::<Vec<_>>()
    );
    for query in [
        ListQuery {
            before_created_at: Some(last.created_at.clone()),
            ..Default::default()
        },
        ListQuery {
            before_id: Some(last.id),
            ..Default::default()
        },
        ListQuery {
            before_created_at: Some("invalid".into()),
            before_id: Some(last.id),
            ..Default::default()
        },
        ListQuery {
            before_created_at: Some(last.created_at.clone()),
            before_id: Some(last.id),
            offset: Some(1),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            list(&state, &owner, query).await,
            Err(AppError::Validation(_))
        ));
    }
    sqlx::query("DELETE FROM files WHERE owner_id = $1")
        .bind(&owner.owner_id)
        .execute(&state.pool)
        .await
        .unwrap();
    for id in [child, folder] {
        sqlx::query("DELETE FROM folders WHERE id = $1")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }
}
