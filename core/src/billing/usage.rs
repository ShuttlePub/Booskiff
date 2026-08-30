//! Storage usage metering repos.

use crate::error::AppError;
use crate::model::Owner;

/// One `storage_usage` metering row.
#[derive(Debug, sqlx::FromRow)]
pub struct UsageRow {
    pub owner_type: String,
    pub owner_id: String,
    pub used_bytes: i64,
    pub updated_at: time::OffsetDateTime,
}

/// Add `delta` (must be non-negative) received bytes to `owner`'s usage in
/// one atomic statement; returns the post-update total, which T6 uses for
/// quota enforcement inside the same DB transaction. Call with
/// `&mut *tx` where `tx: sqlx::Transaction<'_, sqlx::Postgres>`.
pub async fn add_used_bytes(
    conn: &mut sqlx::PgConnection,
    owner: &Owner,
    delta: i64,
) -> Result<i64, AppError> {
    if delta < 0 {
        return Err(AppError::Validation(format!(
            "usage delta must be non-negative, got {delta}"
        )));
    }
    sqlx::query_scalar(
        "INSERT INTO storage_usage (owner_type, owner_id, used_bytes) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (owner_type, owner_id) DO UPDATE \
         SET used_bytes = storage_usage.used_bytes + EXCLUDED.used_bytes, \
             updated_at = now() \
         RETURNING used_bytes",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .bind(delta)
    .fetch_one(conn)
    .await
    .map_err(|err| AppError::Internal(format!("add usage bytes failed: {err}")))
}

/// Subtract `delta` from `owner`'s usage, clamped at zero. A missing row
/// (rows_affected 0) is success.
pub async fn subtract_used_bytes(
    conn: &mut sqlx::PgConnection,
    owner: &Owner,
    delta: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE storage_usage \
         SET used_bytes = GREATEST(used_bytes - $3, 0), updated_at = now() \
         WHERE owner_type = $1 AND owner_id = $2",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .bind(delta)
    .execute(conn)
    .await
    .map(|_| ())
    .map_err(|err| AppError::Internal(format!("subtract usage bytes failed: {err}")))
}

/// Received bytes currently metered for `owner`; a missing row reads as 0.
pub async fn used_bytes(pool: &sqlx::PgPool, owner: &Owner) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT used_bytes FROM storage_usage WHERE owner_type = $1 AND owner_id = $2",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| AppError::Internal(format!("load usage bytes failed: {err}")))
    .map(|used| used.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compose-postgres-backed integration test; run manually via
    /// `cargo test -p core billing -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn pg_add_used_bytes_in_transaction_returns_running_total() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-usage", uuid::Uuid::now_v7().to_string());
        cleanup(&pool, &owner).await;

        let mut tx = pool.begin().await.expect("begin tx");
        let first = add_used_bytes(&mut tx, &owner, 87).await.expect("add 87");
        assert_eq!(first, 87);
        let second = add_used_bytes(&mut tx, &owner, 13).await.expect("add 13");
        assert_eq!(second, 100);

        // Uncommitted rows stay invisible to other connections.
        assert_eq!(used_bytes(&pool, &owner).await.expect("pre-commit"), 0);

        tx.commit().await.expect("commit tx");
        assert_eq!(used_bytes(&pool, &owner).await.expect("post-commit"), 100);

        let err = add_used_bytes(&mut pool.acquire().await.expect("acquire"), &owner, -1)
            .await
            .expect_err("negative delta");
        assert!(matches!(err, AppError::Validation(_)));

        // The metering row round-trips through UsageRow with a timestamp.
        let row: UsageRow = sqlx::query_as(
            "SELECT owner_type, owner_id, used_bytes, updated_at \
             FROM storage_usage WHERE owner_type = $1 AND owner_id = $2",
        )
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .fetch_one(&pool)
        .await
        .expect("fetch usage row");
        assert_eq!(row.used_bytes, 100);
        assert!(row.updated_at <= time::OffsetDateTime::now_utc());

        cleanup(&pool, &owner).await;
    }

    #[tokio::test]
    #[ignore]
    async fn pg_subtract_used_bytes_clamps_at_zero() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-usage", uuid::Uuid::now_v7().to_string());
        cleanup(&pool, &owner).await;

        let mut conn = pool.acquire().await.expect("acquire conn");
        add_used_bytes(&mut conn, &owner, 5).await.expect("add 5");

        subtract_used_bytes(&mut conn, &owner, 100)
            .await
            .expect("overshoot subtract");
        assert_eq!(used_bytes(&pool, &owner).await.expect("clamped"), 0);

        // Subtracting from a missing row is a no-op, not an error.
        let missing = Owner::new("test-billing-usage", uuid::Uuid::now_v7().to_string());
        subtract_used_bytes(&mut conn, &missing, 10)
            .await
            .expect("subtract on missing row");
        assert_eq!(used_bytes(&pool, &missing).await.expect("missing is 0"), 0);

        cleanup(&pool, &owner).await;
    }

    async fn test_pool() -> sqlx::PgPool {
        let url = std::env::var("BOOSKIFF_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff".to_owned());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("PG test needs a running postgres at {url} (docker compose up -d): {err}")
            });
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        pool
    }

    async fn cleanup(pool: &sqlx::PgPool, owner: &Owner) {
        sqlx::query("DELETE FROM storage_usage WHERE owner_type = $1 AND owner_id = $2")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(pool)
            .await
            .expect("clear usage rows");
    }
}
