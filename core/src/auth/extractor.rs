//! Request extractor for the authenticated account.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use crate::auth::jwt::{owner_from_claims, verify_token};
use crate::billing::resolve::effective_limits;
use crate::error::AppError;
use crate::model::{Limits, Owner};
use crate::state::AppState;

/// The account identity attached to an authenticated request, together
/// with the effective limits used for the rate-limit decision.
pub struct AccountContext {
    pub owner: Owner,
    pub limits: Limits,
}

impl FromRequestParts<AppState> for AccountContext {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts).map_err(|err| err.into_response())?;
        let claims = verify_token(&state.jwks_cache, &state.config, token)
            .await
            .map_err(|err| err.into_response())?;
        let owner = owner_from_claims(&claims, &state.config.jwt_owner_type_claim)
            .map_err(|err| err.into_response())?;
        let limits = effective_limits(&state.pool, &state.config, &owner)
            .await
            .map_err(|err| err.into_response())?;
        if !state
            .rate_limiters
            .check(&owner.key(), limits.rate_limit_rpm)
        {
            return Err(AppError::RateLimited.into_response());
        }
        Ok(AccountContext { owner, limits })
    }
}

/// `Authorization: Bearer <token>` value for the request.
fn bearer_token(parts: &Parts) -> Result<&str, AppError> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::Json;
    use axum::http::{Request, StatusCode, header};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::auth::jwks::JwksCache;
    use crate::config::{Config, TrustedIssuer};
    use crate::state::RateLimiters;
    use crate::storage::Storage;

    const TEST_ISSUER: &str = "https://issuer.test";
    const JWKS_FIXTURE: &str = include_str!("../../tests/fixtures/test_only_jwks.json");
    const RSA_PRIVATE_PEM: &str = include_str!("../../tests/fixtures/test_only_rsa_private.pem");

    fn unix_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn mint_token(sub: &str) -> String {
        let header = jsonwebtoken::Header {
            alg: jsonwebtoken::Algorithm::RS256,
            kid: Some("test-only-key-1".into()),
            ..Default::default()
        };
        let claims = serde_json::json!({
            "iss": TEST_ISSUER,
            "sub": sub,
            "exp": unix_now() + 3600,
            "owner_type": "account",
        });
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(RSA_PRIVATE_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
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

    /// Compose postgres (compose.yml), required by the DB-backed tests:
    /// the billing `effective_limits` resolution queries `billing_rules`
    /// on every request, so the 200/429 paths need a live database.
    const PUBLISH_DB_URL: &str = "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff";

    /// Router state for the DB-free paths (header parsing and token
    /// verification both reject before any database access): the pool is
    /// lazy and never connects. `premium_rpm` is only consumed by the
    /// DB-backed tests.
    async fn no_db_app_state() -> AppState {
        let (jwks_url, _hits) = jwks_server().await;
        let config = Config {
            jwt_trusted_issuers: vec![TrustedIssuer {
                issuer: TEST_ISSUER.into(),
                jwks_url,
            }],
            ..Config::default()
        };
        AppState {
            pool: sqlx::PgPool::connect_lazy("postgres://127.0.0.1:9/never")
                .expect("lazy pool never connects"),
            s3: Storage::build(&config).await.unwrap(),
            jwks_cache: JwksCache::new(config.jwt_trusted_issuers.clone()),
            config,
            rate_limiters: Arc::new(RateLimiters::default()),
        }
    }

    /// Router state backed by the compose postgres so `effective_limits`
    /// resolves: `PremiumMode::Everyone` routes owners to premium plan
    /// limits, whose tiny injected rpm makes the limiter trip cheap to
    /// observe. The compose `billing_rules` table stays empty, so no rule
    /// layer overrides the injected rpm.
    async fn db_app_state(premium_rpm: u32) -> AppState {
        let (jwks_url, _hits) = jwks_server().await;
        let config = Config {
            jwt_trusted_issuers: vec![TrustedIssuer {
                issuer: TEST_ISSUER.into(),
                jwks_url,
            }],
            plan_premium_rate_limit_rpm: premium_rpm,
            ..Config::default()
        };
        AppState {
            pool: sqlx::PgPool::connect(PUBLISH_DB_URL).await.unwrap(),
            s3: Storage::build(&config).await.unwrap(),
            jwks_cache: JwksCache::new(config.jwt_trusted_issuers.clone()),
            config,
            rate_limiters: Arc::new(RateLimiters::default()),
        }
    }

    /// Router with one account-authenticated route echoing the owner key.
    fn account_router(state: AppState) -> axum::Router {
        async fn echo_owner(context: AccountContext) -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::OK,
                Json(serde_json::json!({ "owner": context.owner.key() })),
            )
        }
        axum::Router::new()
            .route("/test", get(echo_owner))
            .with_state(state)
    }

    fn request(authorization: Option<String>) -> Request<axum::body::Body> {
        let mut request = Request::builder().uri("/test");
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        request.body(axum::body::Body::empty()).unwrap()
    }

    async fn response_body_json(
        response: axum::response::Response,
    ) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn missing_authorization_header_is_unauthorized() {
        let state = no_db_app_state().await;
        let router = account_router(state);
        let (status, body) = response_body_json(router.oneshot(request(None)).await.unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "missing bearer token");
    }

    #[tokio::test]
    async fn malformed_authorization_header_is_unauthorized() {
        let state = no_db_app_state().await;
        let router = account_router(state);
        for header_value in ["", "Basic abc", "Bearer", "bearer no-capitals"] {
            let (status, body) = response_body_json(
                router
                    .clone()
                    .oneshot(request(Some(header_value.into())))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "header {header_value:?} must be rejected"
            );
            assert_eq!(body["error"]["code"], "unauthorized");
        }
    }

    #[tokio::test]
    async fn invalid_bearer_token_is_unauthorized() {
        let state = no_db_app_state().await;
        let router = account_router(state);
        let (status, body) = response_body_json(
            router
                .oneshot(request(Some("Bearer not-a-jwt".into())))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "malformed token");
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn valid_bearer_token_extracts_owner() {
        let state = db_app_state(100).await;
        let router = account_router(state);
        let token = mint_token("alice");
        let (status, body) = response_body_json(
            router
                .oneshot(request(Some(format!("Bearer {token}"))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["owner"], "account:alice");
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn requests_beyond_rpm_are_rate_limited() {
        let state = db_app_state(2).await;
        // Governor refills one token per 60/rpm seconds, so within a test
        // run only the initial burst of `rpm` requests passes.
        let router = account_router(state);
        let token = mint_token("alice");
        for expected in [StatusCode::OK, StatusCode::OK] {
            let (status, _body) = response_body_json(
                router
                    .clone()
                    .oneshot(request(Some(format!("Bearer {token}"))))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, expected);
        }
        let (status, body) = response_body_json(
            router
                .oneshot(request(Some(format!("Bearer {token}"))))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "rate_limited");
    }
}
