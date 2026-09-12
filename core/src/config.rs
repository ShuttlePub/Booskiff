// allow: SIZE_OK — one responsibility (loading one Config from env) and
// data-dense by nature: 24 fields each appear once in the struct, the
// dev-default literal, and the env override table; the inline tests are
// mandated by the Wave-1 contract. Splitting would scatter that mapping.
//! Service configuration loaded from environment variables.

use std::fmt::Display;

/// A trusted JWT issuer and the JWKS endpoint its keys are fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedIssuer {
    pub issuer: String,
    pub jwks_url: String,
}

/// Configuration loading/validation failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
}

/// How premium limits are assigned to owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PremiumMode {
    /// Every owner gets premium limits.
    Everyone,
    /// Only owners with a premium plan assignment get premium limits.
    Mirror,
}

/// Payment provider wiring (only the disabled variant exists so far).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentProviderCfg {
    Disabled,
}

/// Process configuration. Defaults match the dev docker compose stack
/// (`compose.yml`), so `cargo run` works with zero environment setup.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub s3_endpoint: String,
    /// Endpoint used to build externally reachable presigned URLs
    /// (`None` = same as `s3_endpoint`).
    pub s3_endpoint_public: Option<String>,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub jwt_trusted_issuers: Vec<TrustedIssuer>,
    pub jwt_audience: Option<String>,
    pub jwt_owner_type_claim: String,
    pub listen_addr: String,
    pub premium_mode: PremiumMode,
    pub billing_cache_ttl_secs: u64,
    pub payment_provider: PaymentProviderCfg,
    pub plan_default_storage_quota_bytes: i64,
    pub plan_default_max_file_bytes: i64,
    pub plan_default_rate_limit_rpm: u32,
    pub plan_premium_storage_quota_bytes: i64,
    pub plan_premium_max_file_bytes: i64,
    pub plan_premium_rate_limit_rpm: u32,
    pub presigned_get_ttl_secs: u64,
    pub public_base_url: String,
    pub public_rate_limit_rpm: u32,
    /// Raw admin token seeded into `admin_tokens` (name `bootstrap`) on
    /// startup when no active token with that hash exists yet.
    pub admin_bootstrap_token: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff".into(),
            s3_endpoint: "http://127.0.0.1:9000".into(),
            s3_endpoint_public: None,
            s3_region: "us-east-1".into(),
            s3_bucket: "booskiff-default".into(),
            s3_access_key: "booskiff".into(),
            s3_secret_key: "booskiff-secret".into(),
            jwt_trusted_issuers: Vec::new(),
            jwt_audience: None,
            jwt_owner_type_claim: "owner_type".into(),
            listen_addr: "0.0.0.0:3000".into(),
            premium_mode: PremiumMode::Everyone,
            billing_cache_ttl_secs: 60,
            payment_provider: PaymentProviderCfg::Disabled,
            plan_default_storage_quota_bytes: 1024 * 1024 * 1024,
            plan_default_max_file_bytes: 100 * 1024 * 1024,
            plan_default_rate_limit_rpm: 100,
            plan_premium_storage_quota_bytes: 10 * 1024 * 1024 * 1024,
            plan_premium_max_file_bytes: 500 * 1024 * 1024,
            plan_premium_rate_limit_rpm: 300,
            presigned_get_ttl_secs: 900,
            public_base_url: "http://localhost:3000".into(),
            public_rate_limit_rpm: 300,
            admin_bootstrap_token: None,
        }
    }
}

impl Config {
    /// Load the configuration: `.env` (if present), then real environment
    /// variables, then dev defaults.
    pub fn load() -> Result<Self, ConfigError> {
        dotenvy::dotenv().ok();
        let d = Self::default();
        Ok(Self {
            database_url: std::env::var("BOOSKIFF_DATABASE_URL")
                .or_else(|_| std::env::var("DATABASE_URL"))
                .unwrap_or(d.database_url),
            s3_endpoint: override_str(d.s3_endpoint, "BOOSKIFF_S3_ENDPOINT"),
            s3_endpoint_public: override_opt_str(
                d.s3_endpoint_public,
                "BOOSKIFF_S3_ENDPOINT_PUBLIC",
            ),
            s3_region: override_str(d.s3_region, "BOOSKIFF_S3_REGION"),
            s3_bucket: override_str(d.s3_bucket, "BOOSKIFF_S3_BUCKET"),
            s3_access_key: override_str(d.s3_access_key, "BOOSKIFF_S3_ACCESS_KEY"),
            s3_secret_key: override_str(d.s3_secret_key, "BOOSKIFF_S3_SECRET_KEY"),
            jwt_trusted_issuers: parse_trusted_issuers(
                "BOOSKIFF_JWT_TRUSTED_ISSUERS",
                std::env::var("BOOSKIFF_JWT_TRUSTED_ISSUERS")
                    .unwrap_or_default()
                    .as_str(),
            )?,
            jwt_audience: override_opt_str(d.jwt_audience, "BOOSKIFF_JWT_AUDIENCE"),
            jwt_owner_type_claim: override_str(
                d.jwt_owner_type_claim,
                "BOOSKIFF_JWT_OWNER_TYPE_CLAIM",
            ),
            listen_addr: override_str(d.listen_addr, "BOOSKIFF_LISTEN_ADDR"),
            billing_cache_ttl_secs: override_parse(
                d.billing_cache_ttl_secs,
                "BOOSKIFF_BILLING_CACHE_TTL_SECS",
                "billing_cache_ttl_secs",
            )?,
            premium_mode: parse_premium_mode(
                "BOOSKIFF_PREMIUM_MODE",
                std::env::var("BOOSKIFF_PREMIUM_MODE")
                    .unwrap_or_else(|_| "everyone".into())
                    .as_str(),
            )?,
            payment_provider: parse_payment_provider(
                "BOOSKIFF_PAYMENT_PROVIDER",
                std::env::var("BOOSKIFF_PAYMENT_PROVIDER")
                    .unwrap_or_default()
                    .as_str(),
            )?,
            plan_default_storage_quota_bytes: override_parse(
                d.plan_default_storage_quota_bytes,
                "BOOSKIFF_PLAN_DEFAULT_STORAGE_QUOTA_BYTES",
                "plan_default_storage_quota_bytes",
            )?,
            plan_default_max_file_bytes: override_parse(
                d.plan_default_max_file_bytes,
                "BOOSKIFF_PLAN_DEFAULT_MAX_FILE_BYTES",
                "plan_default_max_file_bytes",
            )?,
            plan_default_rate_limit_rpm: override_parse(
                d.plan_default_rate_limit_rpm,
                "BOOSKIFF_PLAN_DEFAULT_RATE_LIMIT_RPM",
                "plan_default_rate_limit_rpm",
            )?,
            plan_premium_storage_quota_bytes: override_parse(
                d.plan_premium_storage_quota_bytes,
                "BOOSKIFF_PLAN_PREMIUM_STORAGE_QUOTA_BYTES",
                "plan_premium_storage_quota_bytes",
            )?,
            plan_premium_max_file_bytes: override_parse(
                d.plan_premium_max_file_bytes,
                "BOOSKIFF_PLAN_PREMIUM_MAX_FILE_BYTES",
                "plan_premium_max_file_bytes",
            )?,
            plan_premium_rate_limit_rpm: override_parse(
                d.plan_premium_rate_limit_rpm,
                "BOOSKIFF_PLAN_PREMIUM_RATE_LIMIT_RPM",
                "plan_premium_rate_limit_rpm",
            )?,
            presigned_get_ttl_secs: override_parse(
                d.presigned_get_ttl_secs,
                "BOOSKIFF_PRESIGNED_GET_TTL_SECS",
                "presigned_get_ttl_secs",
            )?,
            public_base_url: override_str(d.public_base_url, "BOOSKIFF_PUBLIC_BASE_URL"),
            public_rate_limit_rpm: override_parse(
                d.public_rate_limit_rpm,
                "BOOSKIFF_PUBLIC_RATE_LIMIT_RPM",
                "public_rate_limit_rpm",
            )?,
            admin_bootstrap_token: override_opt_str(
                d.admin_bootstrap_token,
                "BOOSKIFF_ADMIN_BOOTSTRAP_TOKEN",
            ),
        })
    }
}

fn override_str(default: String, key: &str) -> String {
    std::env::var(key).unwrap_or(default)
}

fn override_opt_str(default: Option<String>, key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .and_then(|value| (!value.trim().is_empty()).then_some(value))
        .or(default)
}

fn override_parse<T>(default: T, key: &str, field: &'static str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: Display,
{
    match std::env::var(key) {
        Ok(value) => value.parse::<T>().map_err(|err| ConfigError::Invalid {
            field,
            reason: err.to_string(),
        }),
        Err(_) => Ok(default),
    }
}

/// `BOOSKIFF_JWT_TRUSTED_ISSUERS` = `iss1[|jwks_url],iss2[|jwks_url]`;
/// empty `|jwks_url` defaults to `{iss}/.well-known/jwks.json`.
fn parse_trusted_issuers(
    field: &'static str,
    raw: &str,
) -> Result<Vec<TrustedIssuer>, ConfigError> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (issuer, jwks_url) = match entry.split_once('|') {
                Some((issuer, jwks)) => (issuer.trim(), jwks.trim()),
                None => (entry, ""),
            };
            if issuer.is_empty() {
                return Err(ConfigError::Invalid {
                    field,
                    reason: format!("empty issuer in entry {entry:?}"),
                });
            }
            let jwks_url = if jwks_url.is_empty() {
                format!("{issuer}/.well-known/jwks.json")
            } else {
                jwks_url.to_owned()
            };
            Ok(TrustedIssuer {
                issuer: issuer.to_owned(),
                jwks_url,
            })
        })
        .collect()
}

fn parse_premium_mode(field: &'static str, raw: &str) -> Result<PremiumMode, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "everyone" => Ok(PremiumMode::Everyone),
        "mirror" => Ok(PremiumMode::Mirror),
        other => Err(ConfigError::Invalid {
            field,
            reason: format!("unknown premium mode {other:?} (expected 'everyone' or 'mirror')"),
        }),
    }
}

fn parse_payment_provider(
    field: &'static str,
    raw: &str,
) -> Result<PaymentProviderCfg, ConfigError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "none" | "disabled" => Ok(PaymentProviderCfg::Disabled),
        other => Err(ConfigError::Invalid {
            field,
            reason: format!(
                "unsupported payment provider {other:?} (only 'none'/'disabled' is available)"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(
        "https://id.example.com",
        "https://id.example.com/.well-known/jwks.json"
    )]
    #[case(
        " https://id.example.com ",
        "https://id.example.com/.well-known/jwks.json"
    )]
    fn issuer_without_jwks_defaults_to_well_known(#[case] issuer: &str, #[case] jwks_url: &str) {
        let parsed = parse_trusted_issuers("test", issuer).unwrap();
        assert_eq!(
            parsed,
            vec![TrustedIssuer {
                issuer: issuer.trim().to_owned(),
                jwks_url: jwks_url.to_owned()
            }]
        );
    }

    #[test]
    fn issuer_with_jwks_url_override_wins() {
        let parsed = parse_trusted_issuers(
            "test",
            "https://a.example.com|https://keys.example.com/jwks,https://b.example.com",
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                TrustedIssuer {
                    issuer: "https://a.example.com".into(),
                    jwks_url: "https://keys.example.com/jwks".into(),
                },
                TrustedIssuer {
                    issuer: "https://b.example.com".into(),
                    jwks_url: "https://b.example.com/.well-known/jwks.json".into(),
                },
            ]
        );
    }

    #[test]
    fn empty_issuer_entry_is_rejected() {
        assert!(parse_trusted_issuers("test", "|https://keys.example.com").is_err());
    }

    #[rstest]
    #[case("everyone", PremiumMode::Everyone)]
    #[case("MIRROR", PremiumMode::Mirror)]
    fn premium_mode_parses(#[case] raw: &str, #[case] expected: PremiumMode) {
        assert_eq!(parse_premium_mode("test", raw).unwrap(), expected);
    }

    #[test]
    fn premium_mode_rejects_unknown_value() {
        assert!(parse_premium_mode("test", "sometimes").is_err());
    }

    #[rstest]
    #[case("")]
    #[case("none")]
    #[case("disabled")]
    #[case("DISABLED")]
    fn payment_provider_accepts_disabled_forms(#[case] raw: &str) {
        assert_eq!(
            parse_payment_provider("test", raw).unwrap(),
            PaymentProviderCfg::Disabled
        );
    }

    #[test]
    fn payment_provider_rejects_unknown_provider() {
        let err = parse_payment_provider("test", "stripe").unwrap_err();
        assert!(err.to_string().contains("unsupported payment provider"));
    }
}
