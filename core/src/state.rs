//! Shared application state passed to handlers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::jwks::JwksCache;
use crate::auth::rate_limit::{OwnerRateLimiter, PublicRateLimiter};
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

    /// Admit one request for `key` under `rpm`, `true` when it fits.
    ///
    /// A stored entry whose `rpm` differs is rebuilt from scratch:
    /// billing rule edits change an owner's rpm, and a fresh limiter is
    /// cheaper than reconciling existing token buckets with a new quota.
    ///
    /// Memory bound: at 50k tracked owners, a request from an untracked
    /// key clears the whole map first — limiters refill cheaply (one
    /// token per 60/rpm seconds), so an owner whose limiter was wiped
    /// merely regains a small burst allowance instead of the map growing
    /// without bound across the owner population.
    pub fn check(&self, key: &str, rpm: u32) -> bool {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.len() >= MAX_TRACKED_OWNERS && !map.contains_key(key) {
            map.clear();
        }
        let limiter = match map.get(key) {
            Some(existing) if existing.rpm == rpm => Arc::clone(existing),
            _ => {
                let limiter = Arc::new(OwnerRateLimiter::new(rpm));
                map.insert(key.to_owned(), Arc::clone(&limiter));
                limiter
            }
        };
        limiter.check()
    }
}

/// After how many tracked owners a fresh key wipes the limiter map.
const MAX_TRACKED_OWNERS: usize = 50_000;

/// Cheaply cloneable state shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub s3: Storage,
    pub config: Config,
    pub jwks_cache: JwksCache,
    pub rate_limiters: Arc<RateLimiters>,
    pub public_rate_limiter: Arc<PublicRateLimiter>,
    pub billing_cache: Arc<crate::billing::cache::BillingCache>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_allows_burst_then_denies() {
        let limiters = RateLimiters::default();
        assert!(limiters.check("account:alice", 2));
        assert!(limiters.check("account:alice", 2));
        assert!(!limiters.check("account:alice", 2));
    }

    #[test]
    fn check_rebuilds_limiter_when_rpm_changes() {
        let limiters = RateLimiters::default();
        assert!(limiters.check("account:alice", 1));
        assert!(!limiters.check("account:alice", 1));
        // A new rpm gets a freshly built limiter, not the spent old one.
        assert!(limiters.check("account:alice", 3));
    }

    #[test]
    fn check_ignores_unchanged_rpm_entry() {
        let limiters = RateLimiters::default();
        let stored = limiters.get_or_insert("account:alice", 2);
        // Exhaust the burst through the stored Arc; if `check` rebuilt the
        // entry instead of reusing it, the final call would pass again.
        assert!(stored.check());
        assert!(stored.check());
        assert!(!limiters.check("account:alice", 2));
    }

    #[test]
    fn check_clears_map_when_beyond_bound() {
        let limiters = RateLimiters::default();
        assert!(limiters.check("account:bob", 1));
        assert!(!limiters.check("account:bob", 1));
        for i in 0..MAX_TRACKED_OWNERS {
            limiters.get_or_insert(&format!("owner:{i}"), 1);
        }
        // A request from an untracked key beyond the bound wipes the map...
        assert!(limiters.check("account:carol", 1));
        // ...including bob's exhausted limiter, which rebuilds fresh.
        assert!(limiters.check("account:bob", 1));
    }
}
