//! Billing rule resolution. TODO(Wave5/Wave2): `effective_limits` loads
//! plan assignment + billing rules from the database.

use crate::model::Limits;

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
}
