//! Core domain models shared across modules.

/// Polymorphic owner reference (`owner_type:owner_id`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, utoipa::ToSchema)]
pub struct Owner {
    pub owner_type: String,
    pub owner_id: String,
}

impl Owner {
    pub fn new(owner_type: impl Into<String>, owner_id: impl Into<String>) -> Self {
        Self {
            owner_type: owner_type.into(),
            owner_id: owner_id.into(),
        }
    }

    /// Composite key used for storage prefixes and metering rows.
    pub fn key(&self) -> String {
        format!("{}:{}", self.owner_type, self.owner_id)
    }
}

/// Object kind of the always-present original upload.
pub const OBJECT_KIND_ORIGINAL: &str = "original";

/// Billing plan tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
pub enum Plan {
    Default,
    Premium,
}

impl Plan {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Premium => "premium",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown plan: {0}")]
pub struct UnknownPlanError(pub String);

impl std::str::FromStr for Plan {
    type Err = UnknownPlanError;

    fn from_str(value: &str) -> Result<Self, UnknownPlanError> {
        match value {
            "default" => Ok(Self::Default),
            "premium" => Ok(Self::Premium),
            other => Err(UnknownPlanError(other.to_owned())),
        }
    }
}

/// Effective limits for an owner after plan/billing resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
pub struct Limits {
    pub storage_quota_bytes: i64,
    pub max_file_bytes: i64,
    pub rate_limit_rpm: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_key_joins_type_and_id() {
        let owner = Owner::new("account", "alice");
        assert_eq!(owner.key(), "account:alice");
    }

    #[test]
    fn plan_roundtrips_through_str() {
        use std::str::FromStr;
        for plan in [Plan::Default, Plan::Premium] {
            assert_eq!(Plan::from_str(plan.as_str()), Ok(plan));
        }
        assert!(Plan::from_str("unknown").is_err());
    }
}
