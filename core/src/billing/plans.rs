//! Config-driven plan limits.

use crate::config::Config;
use crate::model::{Limits, Plan};

/// Limits for `plan`, straight from static configuration.
pub fn plan_limits(config: &Config, plan: Plan) -> Limits {
    match plan {
        Plan::Default => Limits {
            storage_quota_bytes: config.plan_default_storage_quota_bytes,
            max_file_bytes: config.plan_default_max_file_bytes,
            rate_limit_rpm: config.plan_default_rate_limit_rpm,
        },
        Plan::Premium => Limits {
            storage_quota_bytes: config.plan_premium_storage_quota_bytes,
            max_file_bytes: config.plan_premium_max_file_bytes,
            rate_limit_rpm: config.plan_premium_rate_limit_rpm,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premium_plan_splits_higher_limits_than_default() {
        let config = Config::default();
        let base = plan_limits(&config, Plan::Default);
        let premium = plan_limits(&config, Plan::Premium);
        assert!(premium.storage_quota_bytes > base.storage_quota_bytes);
        assert!(premium.max_file_bytes > base.max_file_bytes);
        assert!(premium.rate_limit_rpm > base.rate_limit_rpm);
    }
}
