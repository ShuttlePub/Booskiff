//! Billing rule CRUD repos (global and owner-scoped limit overrides).

use crate::error::AppError;
use crate::model::Owner;
use uuid::Uuid;

/// Limit keys the resolver understands.
pub const KNOWN_KEYS: [&str; 3] = ["storage_quota_bytes", "max_file_bytes", "rate_limit_rpm"];

/// One `billing_rules` row; NULL owner columns mean a global rule.
///
/// Kept as a plain `Serialize` DTO for admin-API reuse.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct BillingRule {
    pub id: Uuid,
    pub owner_type: Option<String>,
    pub owner_id: Option<String>,
    pub key: String,
    pub value: serde_json::Value,
    pub enabled: bool,
    pub created_at: time::OffsetDateTime,
}

/// Reject rule keys the resolver would silently ignore.
pub fn validate_key(key: &str) -> Result<(), AppError> {
    if KNOWN_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "unknown billing rule key {key:?} (accepted keys: {})",
            KNOWN_KEYS.join(", ")
        )))
    }
}

/// All rules, global (NULL owner) layer first.
pub async fn list_rules(pool: &sqlx::PgPool) -> Result<Vec<BillingRule>, AppError> {
    sqlx::query_as(
        "SELECT id, owner_type, owner_id, key, value, enabled, created_at \
         FROM billing_rules \
         ORDER BY owner_type NULLS FIRST, key",
    )
    .fetch_all(pool)
    .await
    .map_err(|err| AppError::Internal(format!("list billing rules failed: {err}")))
}

/// Insert or update the rule for (`scope`, `key`). `scope = None` targets
/// the global layer.
///
/// Upserts a rule using `billing_rules_scope_key_uq` as an expression-index
/// arbiter; the double parentheses enable PostgreSQL expression inference.
pub async fn upsert_rule(
    pool: &sqlx::PgPool,
    scope: Option<&Owner>,
    key: &str,
    value: serde_json::Value,
    enabled: bool,
) -> Result<BillingRule, AppError> {
    validate_key(key)?;
    let parsed = value
        .as_i64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()));
    if !parsed.is_some_and(|bytes| bytes >= 0) {
        return Err(AppError::Validation(format!(
            "billing rule {key:?} needs a non-negative integer or numeric string value, got {value}"
        )));
    }

    let (owner_type, owner_id) = match scope {
        Some(owner) => (Some(owner.owner_type.clone()), Some(owner.owner_id.clone())),
        None => (None, None),
    };

    sqlx::query_as::<_, BillingRule>(
        "INSERT INTO billing_rules (owner_type, owner_id, key, value, enabled) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT ((COALESCE(owner_type, '')), (COALESCE(owner_id, '')), key) \
         DO UPDATE SET value = EXCLUDED.value, enabled = EXCLUDED.enabled \
         RETURNING id, owner_type, owner_id, key, value, enabled, created_at",
    )
    .bind(owner_type)
    .bind(owner_id)
    .bind(key)
    .bind(&value)
    .bind(enabled)
    .fetch_one(pool)
    .await
    .map_err(|err| AppError::Internal(format!("insert billing rule failed: {err}")))
}

/// Returns whether a row was removed.
pub async fn delete_rule(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, AppError> {
    sqlx::query("DELETE FROM billing_rules WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|err| AppError::Internal(format!("delete billing rule failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case("storage_quota_bytes")]
    #[case("max_file_bytes")]
    #[case("rate_limit_rpm")]
    fn validate_key_accepts_known_keys(#[case] key: &str) {
        assert!(validate_key(key).is_ok());
    }

    #[test]
    fn validate_key_rejects_unknown_key() {
        for key in ["", "Storage_Quota_Bytes", "future_setting"] {
            let err = validate_key(key).unwrap_err();
            assert!(matches!(err, AppError::Validation(_)), "{key}");
            assert!(err.to_string().contains("accepted keys"));
        }
    }

    /// Compose-postgres-backed integration test; run manually via
    /// `cargo test -p core billing -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn pg_upsert_delete_and_list_rules() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-rules", uuid::Uuid::now_v7().to_string());
        let _guard = global_rules_guard(&pool).await;
        cleanup(&pool, &owner).await;

        let created = upsert_rule(
            &pool,
            Some(&owner),
            "max_file_bytes",
            serde_json::json!(100),
            true,
        )
        .await
        .expect("create owner rule");
        assert_eq!(created.owner_type.as_deref(), Some("test-billing-rules"));
        assert_eq!(created.value, serde_json::json!(100));
        assert!(created.enabled);

        // Update path rewrites value + enabled on the same row.
        let updated = upsert_rule(
            &pool,
            Some(&owner),
            "max_file_bytes",
            serde_json::json!(222),
            false,
        )
        .await
        .expect("update owner rule");
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.value, serde_json::json!(222));
        assert!(!updated.enabled);

        // Global and owner scopes are independent rows for the same key.
        let global = upsert_rule(&pool, None, "max_file_bytes", serde_json::json!(55), true)
            .await
            .expect("create global rule");
        assert_eq!(global.owner_type, None);
        assert_eq!(global.owner_id, None);
        let still_owner = upsert_rule(
            &pool,
            Some(&owner),
            "max_file_bytes",
            serde_json::json!(333),
            true,
        )
        .await
        .expect("owner rule untouched");
        assert_eq!(still_owner.id, created.id);

        let listed = list_rules(&pool).await.expect("list rules");
        let ids: Vec<Uuid> = listed.iter().map(|rule| rule.id).collect();
        assert!(ids.contains(&created.id));
        assert!(ids.contains(&global.id));

        assert!(delete_rule(&pool, created.id).await.expect("delete"));
        assert!(!delete_rule(&pool, created.id).await.expect("re-delete"));

        // Invalid values and keys are rejected before touching the table.
        let err = upsert_rule(
            &pool,
            Some(&owner),
            "max_file_bytes",
            serde_json::json!(-1),
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        let err = upsert_rule(
            &pool,
            Some(&owner),
            "max_file_bytes",
            serde_json::json!("not-a-number"),
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        let err = upsert_rule(
            &pool,
            Some(&owner),
            "future_setting",
            serde_json::json!(1),
            true,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));

        // Numeric strings are accepted, mirroring merge_limits parsing.
        let numeric = upsert_rule(
            &pool,
            Some(&owner),
            "storage_quota_bytes",
            serde_json::json!("4096"),
            true,
        )
        .await
        .expect("numeric string rule");
        assert_eq!(numeric.value, serde_json::json!("4096"));

        cleanup(&pool, &owner).await;
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn pg_concurrent_upserts_do_not_conflict() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-rules", Uuid::now_v7().to_string());
        cleanup(&pool, &owner).await;

        let mut tasks = tokio::task::JoinSet::new();
        for value in 0..8 {
            let pool = pool.clone();
            let owner = owner.clone();
            tasks.spawn(async move {
                upsert_rule(
                    &pool,
                    Some(&owner),
                    "max_file_bytes",
                    serde_json::json!(value),
                    true,
                )
                .await
            });
        }

        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            results.push(result.expect("upsert task panicked"));
        }

        assert!(results.iter().all(Result::is_ok));
        let rules: Vec<BillingRule> = results
            .into_iter()
            .map(|result| result.expect("concurrent upsert failed"))
            .collect();
        let first_id = rules[0].id;
        assert!(rules.iter().all(|rule| rule.id == first_id));
        assert!(
            rules
                .iter()
                .all(|rule| (0..8).any(|value| rule.value == serde_json::json!(value)))
        );
        let stored_value: serde_json::Value = sqlx::query_scalar(
            "SELECT value FROM billing_rules WHERE owner_type = $1 AND owner_id = $2 AND key = $3",
        )
        .bind(&owner.owner_type)
        .bind(&owner.owner_id)
        .bind("max_file_bytes")
        .fetch_one(&pool)
        .await
        .expect("stored concurrent upsert value");
        assert!((0..8).any(|value| stored_value == serde_json::json!(value)));

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

    /// Same lock (and rationale) as resolve.rs's PG test: parallel global
    /// billing_rules seeding would violate billing_rules_scope_key_uq.
    async fn global_rules_guard(pool: &sqlx::PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut guard = pool.begin().await.expect("begin guard tx");
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(PG_TEST_LOCK)
            .execute(&mut *guard)
            .await
            .expect("acquire billing test lock");
        guard
    }

    const PG_TEST_LOCK: i64 = 0x0062_6969;

    async fn cleanup(pool: &sqlx::PgPool, owner: &Owner) {
        sqlx::query("DELETE FROM billing_rules WHERE owner_type = $1 AND owner_id = $2")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(pool)
            .await
            .expect("clear owner rules");
        sqlx::query("DELETE FROM billing_rules WHERE owner_type IS NULL AND owner_id IS NULL AND key = 'max_file_bytes'")
            .execute(pool)
            .await
            .expect("clear seeded global rule");
    }
}
