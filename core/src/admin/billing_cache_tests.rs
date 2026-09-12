use super::*;
use crate::billing::cache::{BillingCache, OwnerEntry};
use crate::config::Config;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
#[ignore = "requires compose postgres"]
async fn pg_admin_billing_writes_invalidate_cache() {
    let config = Config::default();
    let pool = sqlx::PgPool::connect(&config.database_url).await.unwrap();
    let mut guard = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(0x0062_6969_i64)
        .execute(&mut *guard)
        .await
        .unwrap();
    let token = uuid::Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO admin_tokens (name, token_hash) VALUES ($1, $2)")
        .bind(&token)
        .bind(hash_admin_token(&token))
        .execute(&pool)
        .await
        .unwrap();
    let owner = Owner::new("test-admin-cache", uuid::Uuid::now_v7().to_string());
    let cache = Arc::new(BillingCache::new(config.billing_cache_ttl_secs));
    let state = AppState {
        s3: crate::storage::Storage::build(&config).await.unwrap(),
        jwks_cache: crate::auth::jwks::JwksCache::new(Vec::new()),
        pool: pool.clone(),
        config,
        rate_limiters: Arc::new(crate::state::RateLimiters::default()),
        public_rate_limiter: Arc::new(crate::auth::rate_limit::PublicRateLimiter::new(300)),
        billing_cache: Arc::clone(&cache),
    };
    let router = admin_router().with_state(state);
    let plan_uri = format!("/owners/{}/{}/plan", owner.owner_type, owner.owner_id);
    let owner_rule = serde_json::json!({"owner_type": owner.owner_type, "owner_id": owner.owner_id,
        "key": "max_file_bytes", "value": 71, "enabled": true});
    let mut rule_id = String::new();
    for (method, uri, body, all) in [
        (
            "PUT",
            plan_uri.clone(),
            serde_json::json!({"plan":"premium"}),
            false,
        ),
        ("DELETE", plan_uri, serde_json::Value::Null, false),
        ("POST", "/billing/rules".into(), owner_rule, false),
        ("DELETE", String::new(), serde_json::Value::Null, true),
        (
            "POST",
            "/billing/rules".into(),
            serde_json::json!({"key":"max_file_bytes", "value":72, "enabled":true}),
            true,
        ),
        ("DELETE", String::new(), serde_json::Value::Null, true),
    ] {
        cache.store_global(vec![]);
        for key in [owner.key(), "other".into()] {
            cache.store_owner(
                &key,
                OwnerEntry {
                    assignment: None,
                    rules: vec![],
                },
            );
        }
        let uri = if uri.is_empty() {
            format!("/billing/rules/{rule_id}")
        } else {
            uri
        };
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-admin-token", &token)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        assert!(
            response.status().is_success(),
            "{method}: {}",
            response.status()
        );
        assert!(
            cache.get_owner(&owner.key()).is_none(),
            "{method} must invalidate owner"
        );
        assert_eq!(cache.get_owner("other").is_none(), all);
        assert_eq!(cache.get_global().is_none(), all);
        if method == "POST" {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let item: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            rule_id = item["id"].as_str().unwrap().to_owned();
        }
    }
    sqlx::query("DELETE FROM admin_tokens WHERE name = $1")
        .bind(&token)
        .execute(&pool)
        .await
        .unwrap();
}
