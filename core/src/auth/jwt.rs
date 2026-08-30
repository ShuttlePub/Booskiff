// allow: SIZE_OK — one responsibility (JWT verification + owner mapping);
// the line count is dominated by spec-mandated test volume (the accept/
// reject matrix needs an in-test JWKS server plus token minting, and the
// owner-claim matrix is required coverage), and this task's file-ownership
// boundary forbids splitting into new files or submodules.
//! JWT claim shapes and token verification.

use crate::auth::jwks::JwksCache;
use crate::config::Config;
use crate::error::AppError;
use crate::model::Owner;
use serde::Deserialize;

/// Verified JWT claims. Token extensions (including the owner claim named
/// by `Config::jwt_owner_type_claim`) land in `extra`.
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub exp: i64,
    #[serde(default)]
    pub aud: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `iss` claim read without any validation, purely to route a token to
/// its issuer's JWKS before the fully validated decode.
#[derive(Deserialize)]
struct UnverifiedIssuer {
    iss: String,
}

/// Verify `token` against the JWKS of its (unverified) issuer and return
/// the decoded claims. Only RS256 tokens signed by a trusted issuer's
/// published key are accepted.
pub async fn verify_token(
    cache: &JwksCache,
    config: &Config,
    token: &str,
) -> Result<Claims, AppError> {
    let header = jsonwebtoken::decode_header(token)
        .map_err(|_| AppError::Unauthorized("malformed token".into()))?;
    let Some(kid) = header.kid else {
        return Err(AppError::Unauthorized("token header missing kid".into()));
    };
    let unverified = jsonwebtoken::dangerous::insecure_decode::<UnverifiedIssuer>(token)
        .map_err(|_| AppError::Unauthorized("malformed token".into()))?;
    let iss = unverified.claims.iss;
    if !cache.issuers().iter().any(|trusted| trusted.issuer == iss) {
        return Err(AppError::Unauthorized("untrusted issuer".into()));
    }
    let key = cache.key_for(&iss, &kid).await?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_issuer(&[iss]);
    match &config.jwt_audience {
        Some(audience) => validation.set_audience(&[audience]),
        None => validation.validate_aud = false,
    }
    // Leeway 0: exact expiry, no slack. `exp` is a required claim by
    // default in jsonwebtoken 11.
    validation.leeway = 0;
    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)
        .map_err(|_| AppError::Unauthorized("invalid token".into()))?;
    Ok(data.claims)
}

/// Owner identity derived from verified claims: `sub` is the owner id and
/// the claim named `claim` (config: `jwt_owner_type_claim`) the owner
/// type, defaulting to `account` when absent or non-string. An empty id
/// or empty owner type is an invalid identity.
pub fn owner_from_claims(claims: &Claims, claim: &str) -> Result<Owner, AppError> {
    let owner_type = match claims.extra.get(claim).and_then(serde_json::Value::as_str) {
        // A present-but-empty owner type is an invalid identity, not a
        // missing one — it must not fall through to the "account" default.
        Some("") => return Err(AppError::Unauthorized("invalid subject".into())),
        Some(owner_type) => owner_type,
        None => "account",
    };
    if claims.sub.is_empty() {
        return Err(AppError::Unauthorized("invalid subject".into()));
    }
    Ok(Owner::new(owner_type, claims.sub.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::routing::get;
    use rstest::rstest;

    use crate::config::TrustedIssuer;

    const TEST_ISSUER: &str = "https://issuer.test";
    const JWKS_FIXTURE: &str = include_str!("../../tests/fixtures/test_only_jwks.json");
    const RSA_PRIVATE_PEM: &str = include_str!("../../tests/fixtures/test_only_rsa_private.pem");

    fn unix_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn mint_token(claims: &serde_json::Value) -> String {
        mint_token_with(claims, "test-only-key-1")
    }

    fn mint_token_with(claims: &serde_json::Value, kid: &str) -> String {
        let header = jsonwebtoken::Header {
            alg: jsonwebtoken::Algorithm::RS256,
            kid: Some(kid.into()),
            ..Default::default()
        };
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(RSA_PRIVATE_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, claims, &key).unwrap()
    }

    fn valid_claims() -> serde_json::Value {
        serde_json::json!({
            "iss": TEST_ISSUER,
            "sub": "alice",
            "exp": unix_now() + 3600,
            "owner_type": "drive",
        })
    }

    /// Serve the fixture JWKS, counting requests, on a random local port.
    async fn jwks_server() -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let app = axum::Router::new().route(
            "/jwks.json",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    JWKS_FIXTURE
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/jwks.json"), hits)
    }

    async fn cache_and_config() -> (JwksCache, Config) {
        let (url, _hits) = jwks_server().await;
        let cache = JwksCache::new(vec![TrustedIssuer {
            issuer: TEST_ISSUER.into(),
            jwks_url: url,
        }]);
        (cache, Config::default())
    }

    #[tokio::test]
    async fn valid_token_verifies_and_yields_claims() {
        let (cache, config) = cache_and_config().await;
        let claims = verify_token(&cache, &config, &mint_token(&valid_claims()))
            .await
            .unwrap();
        assert_eq!(claims.iss, TEST_ISSUER);
        assert_eq!(claims.sub, "alice");
        assert_eq!(
            claims
                .extra
                .get("owner_type")
                .and_then(serde_json::Value::as_str),
            Some("drive")
        );
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let (cache, config) = cache_and_config().await;
        let mut claims = valid_claims();
        claims["exp"] = (unix_now() - 10).into();
        let err = verify_token(&cache, &config, &mint_token(&claims))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn untrusted_issuer_is_rejected() {
        let (cache, config) = cache_and_config().await;
        let mut claims = valid_claims();
        claims["iss"] = "https://evil.test".into();
        let err = verify_token(&cache, &config, &mint_token(&claims))
            .await
            .unwrap_err();
        assert!(
            matches!(err, AppError::Unauthorized(message) if message.contains("untrusted issuer"))
        );
    }

    #[tokio::test]
    async fn token_without_kid_is_rejected() {
        let (cache, config) = cache_and_config().await;
        let header = jsonwebtoken::Header {
            alg: jsonwebtoken::Algorithm::RS256,
            kid: None,
            ..Default::default()
        };
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(RSA_PRIVATE_PEM.as_bytes()).unwrap();
        let token = jsonwebtoken::encode(&header, &valid_claims(), &key).unwrap();
        let err = verify_token(&cache, &config, &token).await.unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(message) if message.contains("missing kid")));
    }

    #[tokio::test]
    async fn unknown_kid_forces_single_refresh_then_rejects() {
        let (url, hits) = jwks_server().await;
        let cache = JwksCache::new(vec![TrustedIssuer {
            issuer: TEST_ISSUER.into(),
            jwks_url: url,
        }]);
        let config = Config::default();
        // Prime the cache: exactly one JWKS fetch.
        verify_token(&cache, &config, &mint_token(&valid_claims()))
            .await
            .unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // A token with an unknown kid triggers exactly one forced refresh...
        let err = verify_token(
            &cache,
            &config,
            &mint_token_with(&valid_claims(), "missing-kid"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        // ...and further misses inside the cooldown fetch nothing.
        let err = verify_token(
            &cache,
            &config,
            &mint_token_with(&valid_claims(), "missing-kid"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn hs256_signed_token_is_rejected() {
        let (cache, config) = cache_and_config().await;
        let header = jsonwebtoken::Header {
            alg: jsonwebtoken::Algorithm::HS256,
            kid: Some("test-only-key-1".into()),
            ..Default::default()
        };
        let key = jsonwebtoken::EncodingKey::from_secret(b"attacker-controlled-secret");
        let token = jsonwebtoken::encode(&header, &valid_claims(), &key).unwrap();
        let err = verify_token(&cache, &config, &token).await.unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn wrong_audience_is_rejected() {
        let (url, _hits) = jwks_server().await;
        let cache = JwksCache::new(vec![TrustedIssuer {
            issuer: TEST_ISSUER.into(),
            jwks_url: url,
        }]);
        let config = Config {
            jwt_audience: Some("booskiff".into()),
            ..Config::default()
        };
        let mut claims = valid_claims();
        claims["aud"] = "someone-else".into();
        let err = verify_token(&cache, &config, &mint_token(&claims))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Unauthorized(_)));
        // The configured audience accepts the matching token.
        let mut claims = valid_claims();
        claims["aud"] = "booskiff".into();
        let verified = verify_token(&cache, &config, &mint_token(&claims))
            .await
            .unwrap();
        assert_eq!(verified.sub, "alice");
    }

    fn claims_with(sub: &str, owner_type: Option<serde_json::Value>) -> Claims {
        let mut extra = serde_json::Map::new();
        if let Some(owner_type) = owner_type {
            extra.insert("owner_type".into(), owner_type);
        }
        Claims {
            iss: TEST_ISSUER.into(),
            sub: sub.into(),
            exp: unix_now() + 3600,
            aud: None,
            extra,
        }
    }

    #[rstest]
    #[case(Some(serde_json::json!("drive")), "drive")]
    #[case(None, "account")]
    #[case(Some(serde_json::json!(5)), "account")]
    fn owner_type_resolves_from_claim(
        #[case] owner_type: Option<serde_json::Value>,
        #[case] expected: &str,
    ) {
        let claims = claims_with("alice", owner_type);
        assert_eq!(
            owner_from_claims(&claims, "owner_type").unwrap(),
            Owner::new(expected, "alice")
        );
    }

    #[rstest]
    #[case("", Some(serde_json::json!("drive")))]
    #[case("alice", Some(serde_json::json!("")))]
    fn owner_from_claims_rejects_invalid_identities(
        #[case] sub: &str,
        #[case] owner_type: Option<serde_json::Value>,
    ) {
        let claims = claims_with(sub, owner_type);
        let err = owner_from_claims(&claims, "owner_type").unwrap_err();
        assert!(
            matches!(err, AppError::Unauthorized(message) if message.contains("invalid subject"))
        );
    }
}
