//! Plan assignment repos (consumed in `premium_mode = "mirror"`).

use crate::error::AppError;
use crate::model::{Owner, Plan};

/// Assigned plan for `owner`; `None` when no row exists.
///
/// Unknown plan strings downgrade to `None` with a warning: a newer
/// deployment may have written plan names this build does not know, and
/// resolution must fall back to the default plan rather than fail.
pub async fn get_plan(pool: &sqlx::PgPool, owner: &Owner) -> Result<Option<Plan>, AppError> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT plan FROM plan_assignments WHERE owner_type = $1 AND owner_id = $2",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|err| AppError::Internal(format!("load plan assignment failed: {err}")))?;
    Ok(stored.and_then(|plan| match plan.parse::<Plan>() {
        Ok(plan) => Some(plan),
        Err(err) => {
            tracing::warn!(plan = %err.0, "unknown plan in plan_assignments; treating as unassigned");
            None
        }
    }))
}

/// Assign `plan` to `owner`, replacing any previous assignment.
pub async fn set_plan(pool: &sqlx::PgPool, owner: &Owner, plan: Plan) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO plan_assignments (owner_type, owner_id, plan, assigned_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (owner_type, owner_id) \
         DO UPDATE SET plan = EXCLUDED.plan, assigned_at = now()",
    )
    .bind(&owner.owner_type)
    .bind(&owner.owner_id)
    .bind(plan.as_str())
    .execute(pool)
    .await
    .map_err(|err| AppError::Internal(format!("set plan assignment failed: {err}")))?;
    Ok(())
}

/// Returns whether an assignment was removed.
pub async fn delete_plan(pool: &sqlx::PgPool, owner: &Owner) -> Result<bool, AppError> {
    sqlx::query("DELETE FROM plan_assignments WHERE owner_type = $1 AND owner_id = $2")
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|err| AppError::Internal(format!("delete plan assignment failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compose-postgres-backed integration test; run manually via
    /// `cargo test -p core billing -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn pg_plan_assignment_roundtrip() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-assignments", uuid::Uuid::now_v7().to_string());
        cleanup(&pool, &owner).await;

        assert_eq!(get_plan(&pool, &owner).await.expect("empty"), None);

        set_plan(&pool, &owner, Plan::Premium)
            .await
            .expect("assign premium");
        assert_eq!(
            get_plan(&pool, &owner).await.expect("premium"),
            Some(Plan::Premium)
        );

        // Reassignment overwrites in place (upsert path).
        set_plan(&pool, &owner, Plan::Default)
            .await
            .expect("reassign default");
        assert_eq!(
            get_plan(&pool, &owner).await.expect("default"),
            Some(Plan::Default)
        );

        assert!(delete_plan(&pool, &owner).await.expect("delete"));
        assert!(!delete_plan(&pool, &owner).await.expect("re-delete"));
        assert_eq!(get_plan(&pool, &owner).await.expect("after delete"), None);

        // Unknown plan names degrade to unassigned instead of failing.
        sqlx::query(
            "INSERT INTO plan_assignments (owner_type, owner_id, plan) \
             VALUES ($1, $2, 'galaxy-brain') \
             ON CONFLICT (owner_type, owner_id) DO UPDATE SET plan = EXCLUDED.plan",
        )
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .execute(&pool)
        .await
        .expect("seed unknown plan");
        assert_eq!(get_plan(&pool, &owner).await.expect("unknown plan"), None);

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
        sqlx::query("DELETE FROM plan_assignments WHERE owner_type = $1 AND owner_id = $2")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(pool)
            .await
            .expect("clear plan assignments");
    }
}
