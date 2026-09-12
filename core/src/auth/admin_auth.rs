//! Admin token principal extraction.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Authenticated admin identity.
pub struct AdminPrincipal {
    pub token_id: Uuid,
    pub name: String,
    pub role: Role,
}

/// Admin roles. Only `Admin` exists so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
}

/// One active row of `admin_tokens`.
#[derive(Debug, sqlx::FromRow)]
pub struct AdminTokenRow {
    pub id: Uuid,
    pub name: String,
}

/// Lowercase SHA-256 hex digest of a raw admin token. The database only
/// ever stores the digest; the raw token never persists anywhere.
pub fn hash_admin_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(2 * digest.len());
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The non-revoked `admin_tokens` row for `token_hash`, if any.
pub async fn find_active_by_hash(
    pool: &sqlx::PgPool,
    token_hash: &str,
) -> Result<Option<AdminTokenRow>, AppError> {
    sqlx::query_as::<_, AdminTokenRow>(
        "SELECT id, name FROM admin_tokens WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
    .map_err(|err| AppError::Internal(format!("admin token lookup failed: {err}")))
}

impl FromRequestParts<AppState> for AdminPrincipal {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = admin_token(parts).map_err(|err| err.into_response())?;
        let token_hash = hash_admin_token(&token);
        let row = find_active_by_hash(&state.pool, &token_hash)
            .await
            .map_err(|err| err.into_response())?
            .ok_or_else(|| AppError::Unauthorized("invalid admin token".into()).into_response())?;
        // Admin principals are rate-limited under the default-plan rpm:
        // admin endpoints never inspect plans or owners (D18), so the
        // default plan's limit is the natural shared budget for admin
        // traffic.
        if !state.rate_limiters.check(
            &format!("admin:{}", row.id),
            state.config.plan_default_rate_limit_rpm,
        ) {
            return Err(AppError::RateLimited.into_response());
        }
        Ok(AdminPrincipal {
            token_id: row.id,
            name: row.name,
            role: Role::Admin,
        })
    }
}

/// The `X-Admin-Token` header value for the request.
fn admin_token(parts: &Parts) -> Result<String, AppError> {
    parts
        .headers
        .get(axum::http::header::HeaderName::from_static("x-admin-token"))
        .and_then(|value| value.to_str().ok())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::Unauthorized("admin token required".into()))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::Json;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::auth::jwks::JwksCache;
    use crate::config::Config;
    use crate::state::RateLimiters;
    use crate::storage::Storage;

    /// Compose postgres (compose.yml), used only by `#[ignore]` tests.
    const PUBLISH_DB_URL: &str = "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff";

    #[test]
    fn hash_matches_known_sha256_digests() {
        // sha256("test-admin-token")
        assert_eq!(
            hash_admin_token("test-admin-token"),
            "17d6bfe05d1b1fb7bc499f8e3f639c7b3eda4c40f321eef8887a0c04c89a99c5"
        );
        // sha256("")
        assert_eq!(
            hash_admin_token(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hash_admin_token("test-admin-token").len(), 64);
    }

    /// Router state whose pool is lazy and never connects (the
    /// missing-header path returns before any database access).
    async fn no_db_app_state() -> AppState {
        let config = Config::default();
        AppState {
            pool: sqlx::PgPool::connect_lazy("postgres://127.0.0.1:9/never")
                .expect("lazy pool never connects"),
            s3: Storage::build(&config).await.unwrap(),
            jwks_cache: JwksCache::new(Vec::new()),
            billing_cache: Arc::new(crate::billing::cache::BillingCache::new(
                config.billing_cache_ttl_secs,
            )),
            config,
            rate_limiters: Arc::new(RateLimiters::default()),
        }
    }

    fn admin_router(state: AppState) -> axum::Router {
        async fn echo_admin(principal: AdminPrincipal) -> (StatusCode, Json<serde_json::Value>) {
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "name": principal.name,
                    "is_admin": principal.role == Role::Admin,
                })),
            )
        }
        axum::Router::new()
            .route("/admin/test", get(echo_admin))
            .with_state(state)
    }

    fn request(token: Option<&str>) -> Request<axum::body::Body> {
        let mut request = Request::builder().uri("/admin/test");
        if let Some(token) = token {
            request = request.header("x-admin-token", token);
        }
        request.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn missing_admin_token_header_is_unauthorized() {
        let state = no_db_app_state().await;
        let router = admin_router(state);
        let response = router.oneshot(request(None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "admin token required");
    }

    #[tokio::test]
    #[ignore = "requires the compose postgres stack"]
    async fn active_token_authenticates_and_revoked_token_is_rejected() {
        let pool = sqlx::PgPool::connect(PUBLISH_DB_URL).await.unwrap();
        let name = "wave2-t3-admin-auth-test";
        let token = "wave2-t3-admin-auth-test-token";
        let token_hash = hash_admin_token(token);
        sqlx::query("DELETE FROM admin_tokens WHERE name = $1")
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO admin_tokens (name, token_hash) VALUES ($1, $2)")
            .bind(name)
            .bind(&token_hash)
            .execute(&pool)
            .await
            .unwrap();

        let config = Config::default();
        let state = AppState {
            s3: Storage::build(&config).await.unwrap(),
            pool,
            jwks_cache: JwksCache::new(Vec::new()),
            billing_cache: Arc::new(crate::billing::cache::BillingCache::new(
                config.billing_cache_ttl_secs,
            )),
            config,
            rate_limiters: Arc::new(RateLimiters::default()),
        };
        let router = admin_router(state.clone());

        let response = router.clone().oneshot(request(Some(token))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["name"], name);
        assert_eq!(body["is_admin"], true);

        sqlx::query("UPDATE admin_tokens SET revoked_at = now() WHERE name = $1")
            .bind(name)
            .execute(&state.pool)
            .await
            .unwrap();
        let response = router.clone().oneshot(request(Some(token))).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "invalid admin token");

        sqlx::query("DELETE FROM admin_tokens WHERE name = $1")
            .bind(name)
            .execute(&state.pool)
            .await
            .unwrap();
    }
}
