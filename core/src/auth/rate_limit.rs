//! Per-owner and public per-IP rate limiting backed by governor.

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Process-local public budgets; replicas do not share their counters.
/// Every 10,000 checks evict fully replenished entries so a spray of distinct
/// IPs does not accumulate stale DashMap entries indefinitely. Active entries
/// remain until replenished; this is periodic reclamation, not a hard size cap.
pub struct PublicRateLimiter {
    limiter: governor::DefaultKeyedRateLimiter<IpAddr>,
    checks_since_sweep: AtomicUsize,
}

impl PublicRateLimiter {
    /// `rpm` of 0 is clamped to 1, matching the owner limiter.
    pub fn new(rpm: u32) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(rpm).unwrap_or(NonZeroU32::MIN));
        Self {
            limiter: RateLimiter::keyed(quota),
            checks_since_sweep: AtomicUsize::new(0),
        }
    }

    pub fn check(&self, ip: IpAddr) -> bool {
        // This standalone sweep counter does not publish other shared state.
        if self.checks_since_sweep.fetch_add(1, Ordering::Relaxed) % 10_000 == 9_999 {
            self.limiter.retain_recent();
        }
        self.limiter.check_key(&ip).is_ok()
    }
}

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
    fn public_limiter_allows_burst_then_denies_when_budget_exhausted() {
        // Given a two-request budget for one IP.
        let limiter = PublicRateLimiter::new(2);
        let ip = std::net::Ipv4Addr::LOCALHOST.into();
        // When a burst exceeds that budget.
        let admitted = [limiter.check(ip), limiter.check(ip), limiter.check(ip)];
        // Then only the first two requests are admitted.
        assert_eq!(admitted, [true, true, false]);
    }

    #[test]
    fn public_limiter_clamps_quota_when_rpm_is_zero() {
        // Given a zero configured quota.
        let limiter = PublicRateLimiter::new(0);
        let ip = std::net::Ipv6Addr::LOCALHOST.into();
        // When two requests arrive.
        let admitted = [limiter.check(ip), limiter.check(ip)];
        // Then the effective quota is one.
        assert_eq!(admitted, [true, false]);
    }

    #[test]
    fn limiter_allows_exactly_burst_then_denies() {
        let limiter = OwnerRateLimiter::new(2);
        assert!(limiter.check());
        assert!(limiter.check());
        assert!(!limiter.check());
    }
}
