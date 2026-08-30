// allow: SIZE_OK — this file owns the complete admin HTTP resource; the task
// contract pins one handlers file requiring ten handlers, their DTOs, OpenAPI
// annotations, the router, and the unit + ignored PG lifecycle tests.
//! Admin API handlers: token lifecycle, billing rules, plan assignments,
//! and owner usage. Every route authenticates through the `AdminPrincipal`
//! extractor (`X-Admin-Token`, revocation, rate limiting happen there).

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::auth::admin_auth::{AdminPrincipal, hash_admin_token};
use crate::billing::{assignments, rules, usage};
use crate::error::AppError;
use crate::model::{Owner, Plan};
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateTokenRequest {
    pub name: String,
}

/// Response of `POST /v1/admin/tokens`. `token` is the raw secret, returned
/// exactly once; only its SHA-256 digest is ever persisted.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CreatedAdminToken {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub created_at: String,
}

/// One row of `GET /v1/admin/tokens`; carries no secret material.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminTokenItem {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminTokenList {
    pub items: Vec<AdminTokenItem>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateRuleRequest {
    pub owner_type: Option<String>,
    pub owner_id: Option<String>,
    pub key: String,
    pub value: serde_json::Value,
    pub enabled: bool,
}

/// One billing rule as served by the admin API.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BillingRuleItem {
    pub id: Uuid,
    pub owner_type: Option<String>,
    pub owner_id: Option<String>,
    pub key: String,
    pub value: serde_json::Value,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BillingRuleList {
    pub items: Vec<BillingRuleItem>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SetPlanRequest {
    pub plan: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PlanResponse {
    pub plan: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UsageResponse {
    pub used_bytes: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct AdminTokenCreatedRow {
    id: Uuid,
    name: String,
    created_at: OffsetDateTime,
}

#[derive(Debug, sqlx::FromRow)]
struct AdminTokenRow {
    id: Uuid,
    name: String,
    created_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

/// 32 random bytes, base64 URL-safe without padding (43 ASCII chars).
fn generate_admin_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC3339 rendering of a database timestamp.
fn rfc3339(timestamp: OffsetDateTime) -> Result<String, AppError> {
    timestamp
        .format(&Rfc3339)
        .map_err(|err| AppError::Internal(format!("format timestamp: {err}")))
}

/// Owner scope of a rule request: `Some` only when both halves are given;
/// exactly one half is a validation error, both missing targets the global
/// layer.
fn owner_scope(
    owner_type: Option<String>,
    owner_id: Option<String>,
) -> Result<Option<Owner>, AppError> {
    match (owner_type, owner_id) {
        (None, None) => Ok(None),
        (Some(owner_type), Some(owner_id)) => Ok(Some(Owner::new(owner_type, owner_id))),
        (Some(_), None) | (None, Some(_)) => Err(AppError::Validation(
            "owner_type and owner_id must be given together; omit both for a global rule"
                .to_owned(),
        )),
    }
}

/// Parse the plan name of a plan-assignment request.
fn parse_plan(value: &str) -> Result<Plan, AppError> {
    Plan::from_str(value).map_err(|err| AppError::Validation(err.to_string()))
}

#[utoipa::path(
    post,
    path = "/v1/admin/tokens",
    tag = "admin",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Token created; the raw token is returned exactly once", body = CreatedAdminToken),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn create_token(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Json(request): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedAdminToken>), AppError> {
    let token = generate_admin_token();
    let row = sqlx::query_as::<_, AdminTokenCreatedRow>(
        "INSERT INTO admin_tokens (id, name, token_hash) VALUES ($1, $2, $3) \
         RETURNING id, name, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(&request.name)
    .bind(hash_admin_token(&token))
    .fetch_one(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("create admin token: {err}")))?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedAdminToken {
            id: row.id,
            name: row.name,
            token,
            created_at: rfc3339(row.created_at)?,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/v1/admin/tokens",
    tag = "admin",
    responses(
        (status = 200, description = "All admin tokens without secret material", body = AdminTokenList),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn list_tokens(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
) -> Result<Json<AdminTokenList>, AppError> {
    let rows = sqlx::query_as::<_, AdminTokenRow>(
        "SELECT id, name, created_at, revoked_at FROM admin_tokens ORDER BY created_at, id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|err| AppError::Internal(format!("list admin tokens: {err}")))?;
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(AdminTokenItem {
                id: row.id,
                name: row.name,
                created_at: rfc3339(row.created_at)?,
                revoked_at: row.revoked_at.map(rfc3339).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(AdminTokenList { items }))
}

#[utoipa::path(
    delete,
    path = "/v1/admin/tokens/{id}",
    tag = "admin",
    params(("id" = Uuid, Path, description = "Admin token id")),
    responses(
        (status = 204, description = "Revoked; the token authenticates no longer"),
        (status = 404, description = "Unknown token id"),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn revoke_token(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result = sqlx::query("UPDATE admin_tokens SET revoked_at = now() WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|err| AppError::Internal(format!("revoke admin token: {err}")))?;
    if result.rows_affected() > 0 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!("admin token {id} not found")))
    }
}

#[utoipa::path(
    get,
    path = "/v1/admin/billing/rules",
    tag = "admin",
    responses(
        (status = 200, description = "All billing rules, global layer first", body = BillingRuleList),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn list_billing_rules(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
) -> Result<Json<BillingRuleList>, AppError> {
    let rules = rules::list_rules(&state.pool).await?;
    let items = rules
        .into_iter()
        .map(billing_rule_item)
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(BillingRuleList { items }))
}

#[utoipa::path(
    post,
    path = "/v1/admin/billing/rules",
    tag = "admin",
    request_body = CreateRuleRequest,
    responses(
        (status = 200, description = "Rule inserted or updated", body = BillingRuleItem),
        (status = 400, description = "Partial owner scope, unknown key, or invalid value"),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn create_billing_rule(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Json(request): Json<CreateRuleRequest>,
) -> Result<Json<BillingRuleItem>, AppError> {
    let scope = owner_scope(request.owner_type, request.owner_id)?;
    let rule = rules::upsert_rule(
        &state.pool,
        scope.as_ref(),
        &request.key,
        request.value,
        request.enabled,
    )
    .await?;
    Ok(Json(billing_rule_item(rule)?))
}

#[utoipa::path(
    delete,
    path = "/v1/admin/billing/rules/{id}",
    tag = "admin",
    params(("id" = Uuid, Path, description = "Billing rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 404, description = "Unknown rule id"),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn delete_billing_rule(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if rules::delete_rule(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!("billing rule {id} not found")))
    }
}

#[utoipa::path(
    put,
    path = "/v1/admin/owners/{owner_type}/{owner_id}/plan",
    tag = "admin",
    params(
        ("owner_type" = String, Path, description = "Owner type, e.g. account"),
        ("owner_id" = String, Path, description = "Owner id"),
    ),
    request_body = SetPlanRequest,
    responses(
        (status = 204, description = "Plan assigned, replacing any previous assignment"),
        (status = 400, description = "Unknown plan name"),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn set_plan(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path((owner_type, owner_id)): Path<(String, String)>,
    Json(request): Json<SetPlanRequest>,
) -> Result<StatusCode, AppError> {
    let plan = parse_plan(&request.plan)?;
    assignments::set_plan(&state.pool, &Owner::new(owner_type, owner_id), plan).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/v1/admin/owners/{owner_type}/{owner_id}/plan",
    tag = "admin",
    params(
        ("owner_type" = String, Path, description = "Owner type, e.g. account"),
        ("owner_id" = String, Path, description = "Owner id"),
    ),
    responses(
        (status = 200, description = "Assigned plan; default when unassigned", body = PlanResponse),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn get_plan(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path((owner_type, owner_id)): Path<(String, String)>,
) -> Result<Json<PlanResponse>, AppError> {
    let owner = Owner::new(owner_type, owner_id);
    let plan = assignments::get_plan(&state.pool, &owner).await?;
    Ok(Json(PlanResponse {
        plan: plan.unwrap_or(Plan::Default).as_str().to_owned(),
    }))
}

#[utoipa::path(
    delete,
    path = "/v1/admin/owners/{owner_type}/{owner_id}/plan",
    tag = "admin",
    params(
        ("owner_type" = String, Path, description = "Owner type, e.g. account"),
        ("owner_id" = String, Path, description = "Owner id"),
    ),
    responses(
        (status = 204, description = "Assignment removed"),
        (status = 404, description = "No assignment for this owner"),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn delete_plan(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path((owner_type, owner_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let owner = Owner::new(owner_type, owner_id);
    if assignments::delete_plan(&state.pool, &owner).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!(
            "plan assignment for {} not found",
            owner.key()
        )))
    }
}

#[utoipa::path(
    get,
    path = "/v1/admin/owners/{owner_type}/{owner_id}/usage",
    tag = "admin",
    params(
        ("owner_type" = String, Path, description = "Owner type, e.g. account"),
        ("owner_id" = String, Path, description = "Owner id"),
    ),
    responses(
        (status = 200, description = "Metered received bytes; 0 when never uploaded", body = UsageResponse),
        (status = 401, description = "Missing, unknown, or revoked admin token"),
    )
)]
async fn get_usage(
    State(state): State<AppState>,
    _principal: AdminPrincipal,
    Path((owner_type, owner_id)): Path<(String, String)>,
) -> Result<Json<UsageResponse>, AppError> {
    let used = usage::used_bytes(&state.pool, &Owner::new(owner_type, owner_id)).await?;
    Ok(Json(UsageResponse { used_bytes: used }))
}

fn billing_rule_item(rule: rules::BillingRule) -> Result<BillingRuleItem, AppError> {
    Ok(BillingRuleItem {
        id: rule.id,
        owner_type: rule.owner_type,
        owner_id: rule.owner_id,
        key: rule.key,
        value: rule.value,
        enabled: rule.enabled,
        created_at: rfc3339(rule.created_at)?,
    })
}

/// Routes relative to `/v1/admin`; the mount point adds the prefix.
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/tokens", post(create_token).get(list_tokens))
        .route("/tokens/{id}", delete(revoke_token))
        .route(
            "/billing/rules",
            get(list_billing_rules).post(create_billing_rule),
        )
        .route("/billing/rules/{id}", delete(delete_billing_rule))
        .route(
            "/owners/{owner_type}/{owner_id}/plan",
            put(set_plan).get(get_plan).delete(delete_plan),
        )
        .route("/owners/{owner_type}/{owner_id}/usage", get(get_usage))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use rstest::rstest;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::admin_auth::find_active_by_hash;
    use crate::auth::jwks::JwksCache;
    use crate::config::Config;
    use crate::state::RateLimiters;
    use crate::storage::Storage;

    /// Compose postgres (compose.yml), used only by `#[ignore]` tests.
    const PUBLISH_DB_URL: &str = "postgres://booskiff:booskiff@127.0.0.1:5432/booskiff";

    #[test]
    fn generated_token_is_43_url_safe_chars_decoding_to_32_bytes() {
        let token = generate_admin_token();
        assert_eq!(token.len(), 43, "32 bytes base64 URL_SAFE_NO_PAD");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token must be URL-safe: {token}"
        );
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&token)
            .expect("token decodes as URL_SAFE_NO_PAD");
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn generated_tokens_are_unique_across_samples() {
        let tokens: HashSet<String> = (0..64).map(|_| generate_admin_token()).collect();
        assert_eq!(tokens.len(), 64, "256-bit draws must not collide");
    }

    #[test]
    fn owner_scope_is_global_when_both_halves_missing() {
        assert_eq!(owner_scope(None, None).unwrap(), None);
    }

    #[test]
    fn owner_scope_builds_owner_when_both_halves_present() {
        let scope = owner_scope(Some("account".to_owned()), Some("alice".to_owned())).unwrap();
        assert_eq!(scope, Some(Owner::new("account", "alice")));
    }

    #[rstest]
    #[case(Some("account".to_owned()), None)]
    #[case(None, Some("alice".to_owned()))]
    fn owner_scope_rejects_partial_scope(
        #[case] owner_type: Option<String>,
        #[case] owner_id: Option<String>,
    ) {
        let err = owner_scope(owner_type, owner_id).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[rstest]
    #[case("default", Plan::Default)]
    #[case("premium", Plan::Premium)]
    fn parse_plan_accepts_known_plans(#[case] raw: &str, #[case] plan: Plan) {
        assert_eq!(parse_plan(raw).unwrap(), plan);
    }

    #[rstest]
    #[case("")]
    #[case("Default")]
    #[case("galaxy-brain")]
    fn parse_plan_rejects_unknown_names(#[case] raw: &str) {
        let err = parse_plan(raw).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn rfc3339_renders_utc_epoch() {
        assert_eq!(
            rfc3339(OffsetDateTime::UNIX_EPOCH).unwrap(),
            "1970-01-01T00:00:00Z"
        );
    }

    fn request_with_token(
        method: &str,
        uri: &str,
        token: &str,
        json_body: Option<String>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-admin-token", token);
        match json_body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                builder.body(Body::from(body)).unwrap()
            }
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn test_pool() -> sqlx::PgPool {
        let url =
            std::env::var("BOOSKIFF_DATABASE_URL").unwrap_or_else(|_| PUBLISH_DB_URL.to_owned());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap_or_else(|err| {
                panic!("PG test needs a running postgres at {url} (docker compose up -d): {err}")
            });
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        pool
    }

    /// Compose-postgres-backed lifecycle; run manually via
    /// `cargo test -p core admin:: -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn pg_token_lifecycle_hashes_at_rest_and_revocation_blocks_auth() {
        let pool = test_pool().await;
        let bootstrap_name = format!("wave3-t8-bootstrap-{}", Uuid::now_v7());
        let created_name = format!("wave3-t8-created-{}", Uuid::now_v7());
        // Random per run: token_hash is UNIQUE, so a fixed secret would
        // collide with residue from any earlier failed run.
        let bootstrap_raw = format!("wave3-t8-bootstrap-{}", Uuid::now_v7());
        sqlx::query("INSERT INTO admin_tokens (name, token_hash) VALUES ($1, $2)")
            .bind(&bootstrap_name)
            .bind(hash_admin_token(&bootstrap_raw))
            .execute(&pool)
            .await
            .expect("seed bootstrap token");

        let config = Config::default();
        let state = AppState {
            pool: pool.clone(),
            s3: Storage::build(&config).await.unwrap(),
            jwks_cache: JwksCache::new(Vec::new()),
            config,
            rate_limiters: std::sync::Arc::new(RateLimiters::default()),
        };
        let router = axum::Router::new()
            .nest("/v1/admin", admin_router())
            .with_state(state);

        // Create through the API, authenticated by the bootstrap token.
        let response = router
            .clone()
            .oneshot(request_with_token(
                "POST",
                "/v1/admin/tokens",
                &bootstrap_raw,
                Some(format!(r#"{{"name":"{created_name}"}}"#)),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let created: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let raw_token = created["token"].as_str().expect("raw token").to_owned();
        let token_id: Uuid = created["id"].as_str().expect("id").parse().unwrap();
        assert_eq!(created["name"], created_name.as_str());
        assert!(created["created_at"].as_str().is_some());

        // At rest only the digest: not the raw token, but its SHA-256.
        let stored_hash: String =
            sqlx::query_scalar("SELECT token_hash FROM admin_tokens WHERE id = $1")
                .bind(token_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(stored_hash, raw_token);
        assert_eq!(stored_hash, hash_admin_token(&raw_token));

        // The list never carries secret material.
        let response = router
            .clone()
            .oneshot(request_with_token(
                "GET",
                "/v1/admin/tokens",
                &bootstrap_raw,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let items = listed["items"].as_array().expect("items array");
        assert!(items.iter().any(|item| item["id"] == created["id"]));
        for item in items {
            assert!(item.get("token").is_none(), "{item}");
            assert!(item.get("token_hash").is_none(), "{item}");
        }

        // Revoke: 204, and the digest stops authenticating immediately.
        let response = router
            .clone()
            .oneshot(request_with_token(
                "DELETE",
                &format!("/v1/admin/tokens/{token_id}"),
                &bootstrap_raw,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let active = find_active_by_hash(&pool, &hash_admin_token(&raw_token))
            .await
            .unwrap();
        assert!(active.is_none());

        // Unknown id is a 404, not a silent success.
        let response = router
            .oneshot(request_with_token(
                "DELETE",
                &format!("/v1/admin/tokens/{}", Uuid::now_v7()),
                &bootstrap_raw,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        for name in [&bootstrap_name, &created_name] {
            sqlx::query("DELETE FROM admin_tokens WHERE name = $1")
                .bind(name)
                .execute(&pool)
                .await
                .expect("cleanup token row");
        }
    }
}
