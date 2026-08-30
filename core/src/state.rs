//! Shared application state passed to handlers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::jwks::JwksCache;
use crate::auth::rate_limit::OwnerRateLimiter;
use crate::config::Config;
use crate::storage::Storage;

/// Per-owner rate limiter registry, shared behind `AppState::rate_limiters`.
pub struct RateLimiters {
    inner: Mutex<HashMap<String, Arc<OwnerRateLimiter>>>,
}

impl Default for RateLimiters {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl RateLimiters {
    /// Return the limiter for `key`, creating it with `rpm` on first use.
    pub fn get_or_insert(&self, key: &str, rpm: u32) -> Arc<OwnerRateLimiter> {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(key.to_owned())
            .or_insert_with(|| Arc::new(OwnerRateLimiter::new(rpm)))
            .clone()
    }
}

/// Cheaply cloneable state shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub s3: Storage,
    pub config: Config,
    pub jwks_cache: JwksCache,
    pub rate_limiters: Arc<RateLimiters>,
}
