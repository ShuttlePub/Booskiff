// allow: SIZE_OK — the frozen `merge_limits` and its frozen tests are
// mandated to stay in this file (append-only contract) and the PG
// integration test doubles the size; the production half is 96 LOC.
//! Billing rule resolution: plan assignment → global rules → owner rules.

use crate::config::{Config, PremiumMode};
use crate::error::AppError;
use crate::model::{Limits, Owner, Plan};
use sqlx::Row;

/// Global rule layer: enabled rows with NULL owner columns.
const GLOBAL_RULES_SQL: &str = "SELECT key, value FROM billing_rules \
     WHERE enabled AND owner_type IS NULL AND owner_id IS NULL";

/// Owner rule layer: enabled rows scoped to one owner.
const OWNER_RULES_SQL: &str =
    "SELECT key, value FROM billing_rules WHERE enabled AND owner_type = $1 AND owner_id = $2";

/// Effective limits for `owner`. Consumed by the auth extractor (rate
/// limiting) and the upload handler (quota checks).
///
/// Resolution order: plan (config premium mode + `plan_assignments`) →
/// global `billing_rules` → owner-scoped `billing_rules`, merged via
/// [`merge_limits`] (later layers win).
pub async fn effective_limits(
    pool: &sqlx::PgPool,
    config: &Config,
    owner: &Owner,
) -> Result<Limits, crate::error::AppError> {
    // `everyone` mode skips the plan_assignments table entirely.
    let assignment = match config.premium_mode {
        PremiumMode::Everyone => None,
        PremiumMode::Mirror => crate::billing::assignments::get_plan(pool, owner).await?,
    };
    let global_rules = load_rules(pool, GLOBAL_RULES_SQL, None).await?;
    let owner_rules = load_rules(pool, OWNER_RULES_SQL, Some(owner)).await?;
    Ok(resolve_from_parts(
        config,
        assignment,
        global_rules,
        owner_rules,
    ))
}

pub async fn effective_limits_cached(
    cache: &crate::billing::cache::BillingCache,
    pool: &sqlx::PgPool,
    config: &Config,
    owner: &Owner,
) -> Result<Limits, AppError> {
    let generation = cache.generation();
    let global = match cache.get_global() {
        Some(rules) => rules,
        None => {
            let rules = load_rules(pool, GLOBAL_RULES_SQL, None).await?;
            cache.store_global_if_current(rules.clone(), &generation);
            rules
        }
    };
    let key = owner.key();
    let entry = match cache.get_owner(&key) {
        Some(entry) => entry,
        None => {
            let assignment = match config.premium_mode {
                PremiumMode::Everyone => None,
                PremiumMode::Mirror => crate::billing::assignments::get_plan(pool, owner).await?,
            };
            let rules = load_rules(pool, OWNER_RULES_SQL, Some(owner)).await?;
            let entry = crate::billing::cache::OwnerEntry { assignment, rules };
            cache.store_owner_if_current(&key, entry.clone(), &generation);
            entry
        }
    };
    Ok(resolve_from_parts(
        config,
        entry.assignment,
        global,
        entry.rules,
    ))
}

/// Pure layer composition mirroring [`effective_limits`] without the DB so
/// the resolution-order contract is unit-testable: plan base limits →
/// global rules → owner rules (later layers win per key).
fn resolve_from_parts(
    config: &Config,
    assignment: Option<Plan>,
    global_rules: Vec<(String, serde_json::Value)>,
    owner_rules: Vec<(String, serde_json::Value)>,
) -> Limits {
    let plan = match config.premium_mode {
        PremiumMode::Everyone => Plan::Premium,
        PremiumMode::Mirror => assignment.unwrap_or(Plan::Default),
    };
    merge_limits(
        merge_limits(
            crate::billing::plans::plan_limits(config, plan),
            global_rules.into_iter(),
        ),
        owner_rules.into_iter(),
    )
}

/// Load one enabled rule layer as `(key, value)` pairs. `owner = None`
/// selects the global layer (NULL owner columns, matched by the query
/// itself); `Some(owner)` binds the owner scope.
async fn load_rules(
    pool: &sqlx::PgPool,
    sql: &'static str,
    owner: Option<&Owner>,
) -> Result<Vec<(String, serde_json::Value)>, AppError> {
    let mut query = sqlx::query(sql);
    if let Some(owner) = owner {
        query = query.bind(&owner.owner_type).bind(&owner.owner_id);
    }
    query
        .try_map(|row: sqlx::postgres::PgRow| {
            Ok((
                row.try_get::<String, _>("key")?,
                row.try_get::<serde_json::Value, _>("value")?,
            ))
        })
        .fetch_all(pool)
        .await
        .map_err(|err| AppError::Internal(format!("load billing rules failed: {err}")))
}

/// Apply rule overrides onto `base`. Rules are applied in order; a later
/// rule targeting the same key wins. Unknown keys and value shapes that
/// do not fit the limit type are ignored.
pub fn merge_limits(
    base: Limits,
    rules: impl Iterator<Item = (String, serde_json::Value)>,
) -> Limits {
    let mut limits = base;
    for (key, value) in rules {
        match key.as_str() {
            "storage_quota_bytes" => {
                if let Some(bytes) = as_i64(&value) {
                    limits.storage_quota_bytes = bytes;
                }
            }
            "max_file_bytes" => {
                if let Some(bytes) = as_i64(&value) {
                    limits.max_file_bytes = bytes;
                }
            }
            "rate_limit_rpm" => {
                if let Some(rpm) = as_i64(&value).and_then(|rpm| u32::try_from(rpm).ok()) {
                    limits.rate_limit_rpm = rpm;
                }
            }
            _ => {}
        }
    }
    limits
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Limits {
        Limits {
            storage_quota_bytes: 100,
            max_file_bytes: 10,
            rate_limit_rpm: 5,
        }
    }

    fn rule(key: &str, value: serde_json::Value) -> (String, serde_json::Value) {
        (key.to_owned(), value)
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let merged = merge_limits(
            base(),
            [rule("future_setting", serde_json::json!(999))].into_iter(),
        );
        assert_eq!(merged, base());
    }

    #[test]
    fn later_rule_wins_for_same_key() {
        let merged = merge_limits(
            base(),
            [
                rule("max_file_bytes", serde_json::json!(20)),
                rule("max_file_bytes", serde_json::json!(30)),
            ]
            .into_iter(),
        );
        assert_eq!(merged.max_file_bytes, 30);
    }

    #[test]
    fn applies_all_three_known_keys() {
        let merged = merge_limits(
            base(),
            [
                rule("storage_quota_bytes", serde_json::json!(200)),
                rule("max_file_bytes", serde_json::json!("40")),
                rule("rate_limit_rpm", serde_json::json!(15)),
            ]
            .into_iter(),
        );
        assert_eq!(
            merged,
            Limits {
                storage_quota_bytes: 200,
                max_file_bytes: 40,
                rate_limit_rpm: 15,
            }
        );
    }

    #[test]
    fn ill_fitting_values_are_ignored() {
        let merged = merge_limits(
            base(),
            [
                rule("rate_limit_rpm", serde_json::json!(-3)),
                rule("max_file_bytes", serde_json::json!("not-a-number")),
                rule("storage_quota_bytes", serde_json::json!(true)),
            ]
            .into_iter(),
        );
        assert_eq!(merged, base());
    }

    fn mirror_config() -> Config {
        Config {
            premium_mode: PremiumMode::Mirror,
            ..Config::default()
        }
    }

    fn everyone_config() -> Config {
        Config {
            premium_mode: PremiumMode::Everyone,
            ..Config::default()
        }
    }

    #[test]
    fn everyone_mode_ignores_assignment() {
        // Even a "default" assignment must not downgrade everyone mode.
        let limits = resolve_from_parts(
            &everyone_config(),
            Some(Plan::Default),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            limits,
            crate::billing::plans::plan_limits(&everyone_config(), Plan::Premium)
        );
    }

    #[test]
    fn mirror_without_assignment_falls_back_to_default_plan() {
        let limits = resolve_from_parts(&mirror_config(), None, Vec::new(), Vec::new());
        assert_eq!(
            limits,
            crate::billing::plans::plan_limits(&mirror_config(), Plan::Default)
        );
    }

    #[test]
    fn mirror_with_premium_assignment_upgrades_plan() {
        let limits = resolve_from_parts(
            &mirror_config(),
            Some(Plan::Premium),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            limits.storage_quota_bytes,
            crate::billing::plans::plan_limits(&mirror_config(), Plan::Premium).storage_quota_bytes
        );
    }

    #[test]
    fn global_rule_beats_plan_default() {
        let limits = resolve_from_parts(
            &mirror_config(),
            None,
            vec![rule("storage_quota_bytes", serde_json::json!(123))],
            Vec::new(),
        );
        assert_eq!(limits.storage_quota_bytes, 123);
        assert_eq!(
            limits.max_file_bytes,
            crate::billing::plans::plan_limits(&mirror_config(), Plan::Default).max_file_bytes
        );
    }

    #[test]
    fn owner_rule_beats_global_rule() {
        let limits = resolve_from_parts(
            &mirror_config(),
            None,
            vec![
                rule("max_file_bytes", serde_json::json!(30)),
                rule("rate_limit_rpm", serde_json::json!(7)),
            ],
            vec![rule("max_file_bytes", serde_json::json!(70))],
        );
        // Owner layer wins the contested key…
        assert_eq!(limits.max_file_bytes, 70);
        // …and the global layer still fills keys the owner layer omits.
        assert_eq!(limits.rate_limit_rpm, 7);
    }

    #[test]
    fn numeric_string_owner_rule_overrides_global() {
        let limits = resolve_from_parts(
            &mirror_config(),
            None,
            vec![rule("storage_quota_bytes", serde_json::json!(50))],
            vec![rule("storage_quota_bytes", serde_json::json!("60"))],
        );
        assert_eq!(limits.storage_quota_bytes, 60);
    }

    /// Compose-postgres-backed integration test; run manually via
    /// `cargo test -p core billing -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn pg_effective_limits_applies_all_layers() {
        let pool = test_pool().await;
        let owner = Owner::new("test-billing-resolve", uuid::Uuid::now_v7().to_string());
        let _guard = global_rules_guard(&pool).await;
        cleanup(&pool, &owner).await;

        // Premium assignment (mirror mode) + global and owner rules.
        crate::billing::assignments::set_plan(&pool, &owner, Plan::Premium)
            .await
            .expect("set plan");
        let seed = r#"
            INSERT INTO billing_rules (owner_type, owner_id, key, value)
            VALUES
                (NULL, NULL, 'storage_quota_bytes', '42'),
                (NULL, NULL, 'max_file_bytes', '55'),
                ($1, $2, 'max_file_bytes', '77')
        "#;
        sqlx::query(seed)
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(&pool)
            .await
            .expect("seed rules");

        let mirror = mirror_config();
        let limits = effective_limits(&pool, &mirror, &owner)
            .await
            .expect("mirror");
        // Owner layer wins max_file_bytes; global fills storage quota;
        // untouched keys keep the premium plan value.
        assert_eq!(limits.max_file_bytes, 77);
        assert_eq!(limits.storage_quota_bytes, 42);
        assert_eq!(limits.rate_limit_rpm, mirror.plan_premium_rate_limit_rpm);

        // Everyone mode ignores the assignment but still applies rules.
        let everyone = everyone_config();
        let limits = effective_limits(&pool, &everyone, &owner)
            .await
            .expect("everyone");
        assert_eq!(limits.max_file_bytes, 77);
        assert_eq!(limits.storage_quota_bytes, 42);

        // No assignment + no owner rules → default plan + global only.
        sqlx::query("DELETE FROM billing_rules WHERE owner_type = $1 AND owner_id = $2")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(&pool)
            .await
            .expect("clear owner rules");
        crate::billing::assignments::delete_plan(&pool, &owner)
            .await
            .expect("delete plan");
        let limits = effective_limits(&pool, &mirror, &owner)
            .await
            .expect("default");
        assert_eq!(limits.max_file_bytes, 55);
        assert_eq!(limits.storage_quota_bytes, 42);
        // rate_limit_rpm carries no rule, so it exposes the plan fallback.
        assert_eq!(limits.rate_limit_rpm, mirror.plan_default_rate_limit_rpm);

        cleanup(&pool, &owner).await;
    }

    #[tokio::test]
    #[ignore]
    async fn pg_cached_limits_stale_until_invalidated() {
        use crate::billing::cache::BillingCache;
        let pool = test_pool().await;
        let _guard = global_rules_guard(&pool).await;
        let owner = Owner::new("test-billing-cache", uuid::Uuid::now_v7().to_string());
        let other = Owner::new("test-billing-cache", uuid::Uuid::now_v7().to_string());
        cleanup(&pool, &owner).await;
        let config = mirror_config();
        let cache = BillingCache::new(60);
        for target in [&owner, &other] {
            sqlx::query("INSERT INTO billing_rules (owner_type, owner_id, key, value) VALUES ($1, $2, 'max_file_bytes', '71')")
                .bind(&target.owner_type).bind(&target.owner_id).execute(&pool).await.unwrap();
            assert_eq!(
                effective_limits_cached(&cache, &pool, &config, target)
                    .await
                    .unwrap()
                    .max_file_bytes,
                71
            );
            sqlx::query(
                "UPDATE billing_rules SET value = '82' WHERE owner_type = $1 AND owner_id = $2",
            )
            .bind(&target.owner_type)
            .bind(&target.owner_id)
            .execute(&pool)
            .await
            .unwrap();
            crate::billing::assignments::set_plan(&pool, target, Plan::Premium)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO billing_rules (key, value) VALUES ('storage_quota_bytes', '93')")
            .execute(&pool)
            .await
            .unwrap();
        let stale = effective_limits_cached(&cache, &pool, &config, &owner)
            .await
            .unwrap();
        assert_eq!(stale.max_file_bytes, 71);
        assert_eq!(stale.rate_limit_rpm, config.plan_default_rate_limit_rpm);
        assert_eq!(
            stale.storage_quota_bytes,
            config.plan_default_storage_quota_bytes
        );
        cache.invalidate_owner(&owner.key());
        let fresh = effective_limits_cached(&cache, &pool, &config, &owner)
            .await
            .unwrap();
        assert_eq!(fresh.max_file_bytes, 82);
        assert_eq!(fresh.rate_limit_rpm, config.plan_premium_rate_limit_rpm);
        assert_eq!(
            fresh.storage_quota_bytes,
            config.plan_premium_storage_quota_bytes
        );
        assert_eq!(
            effective_limits_cached(&cache, &pool, &config, &other)
                .await
                .unwrap()
                .max_file_bytes,
            71
        );
        cache.invalidate_all();
        let fresh = effective_limits_cached(&cache, &pool, &config, &other)
            .await
            .unwrap();
        assert_eq!(fresh.max_file_bytes, 82);
        assert_eq!(fresh.storage_quota_bytes, 93);
        // A closed pool proves a fully warm hit issues no SQL at all.
        let closed = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(&config.database_url)
            .unwrap();
        closed.close().await;
        assert_eq!(
            effective_limits_cached(&cache, &closed, &config, &other)
                .await
                .unwrap()
                .max_file_bytes,
            82
        );
        cleanup(&pool, &owner).await;
        cleanup(&pool, &other).await;
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

    /// Serializes PG tests that seed global billing_rules rows; the test
    /// harness runs #[ignore] tests in parallel by default. Dropping the
    /// transaction (commit or unwind) releases the lock.
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
        sqlx::query(
            "DELETE FROM billing_rules \
             WHERE owner_type IS NULL AND owner_id IS NULL \
               AND key IN ('max_file_bytes', 'storage_quota_bytes')",
        )
        .execute(pool)
        .await
        .expect("clear seeded global rules");
        sqlx::query("DELETE FROM plan_assignments WHERE owner_type = $1 AND owner_id = $2")
            .bind(&owner.owner_type)
            .bind(&owner.owner_id)
            .execute(pool)
            .await
            .expect("clear plan assignments");
    }
}
