//! Shared PostgreSQL fixtures for drive handler tests; no network S3 operations.
use crate::auth::extractor::AccountContext;
use crate::config::Config;
use crate::model::{Limits, Owner};
use crate::state::AppState;
use crate::storage::Storage;
use uuid::Uuid;

pub(super) fn context(owner: &Owner) -> AccountContext {
    AccountContext {
        owner: owner.clone(),
        limits: Limits {
            storage_quota_bytes: 1024 * 1024,
            max_file_bytes: 1024 * 1024,
            rate_limit_rpm: 100,
        },
    }
}

pub(super) async fn setup(label: &str) -> (AppState, Owner) {
    let url = std::env::var("BOOSKIFF_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff".into());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let config = Config::default();
    let state = AppState {
        billing_cache: std::sync::Arc::new(crate::billing::cache::BillingCache::new(
            config.billing_cache_ttl_secs,
        )),
        jwks_cache: crate::auth::jwks::JwksCache::new(config.jwt_trusted_issuers.clone()),
        s3: Storage::build(&config).await.unwrap(),
        pool,
        config,
        rate_limiters: std::sync::Arc::new(crate::state::RateLimiters::default()),
        public_rate_limiter: std::sync::Arc::new(crate::auth::rate_limit::PublicRateLimiter::new(
            300,
        )),
    };
    (
        state,
        Owner::new(format!("test-{label}"), Uuid::now_v7().to_string()),
    )
}
