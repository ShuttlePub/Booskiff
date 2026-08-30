//! Per-owner rate limiting backed by governor.

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::num::NonZeroU32;

/// A fixed-window-ish limiter allowing `rpm` requests per minute with full
/// burst capacity available immediately.
pub struct OwnerRateLimiter {
    pub rpm: u32,
    limiter: DefaultDirectRateLimiter,
}

impl OwnerRateLimiter {
    /// `rpm` of 0 is clamped to 1 (governor requires a non-zero quota).
    pub fn new(rpm: u32) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(rpm).unwrap_or(NonZeroU32::MIN));
        Self {
            rpm,
            limiter: RateLimiter::direct(quota),
        }
    }

    /// `true` if one request fits under the current quota.
    pub fn check(&self) -> bool {
        self.limiter.check().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_exactly_burst_then_denies() {
        let limiter = OwnerRateLimiter::new(2);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
    }
}
