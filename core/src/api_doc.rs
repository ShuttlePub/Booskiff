//! OpenAPI document generation and Swagger UI wiring.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Booskiff",
        version = "0.1.0",
        description = "Booskiff drive API: per-owner file and folder storage with \
                       streaming uploads, publishing, and download URLs, built on the \
                       billing policy foundation (plans, rules, quotas) that governs \
                       storage limits.",
    ),
    paths(
        crate::drive::files::upload_file,
        crate::drive::files::list_files,
        crate::drive::files::get_file,
        crate::drive::files::delete_file,
        crate::drive::files::download_url,
        crate::drive::files::publish_file,
        crate::drive::files::unpublish_file,
        crate::drive::folders::create_folder,
        crate::drive::folders::list_folders,
        crate::drive::folders::get_folder,
        crate::drive::folders::rename_folder,
        crate::drive::folders::delete_folder,
        crate::billing::status_handler::get_billing_status,
        crate::admin::handlers::create_token,
        crate::admin::handlers::list_tokens,
        crate::admin::handlers::revoke_token,
        crate::admin::handlers::list_billing_rules,
        crate::admin::handlers::create_billing_rule,
        crate::admin::handlers::delete_billing_rule,
        crate::admin::handlers::set_plan,
        crate::admin::handlers::get_plan,
        crate::admin::handlers::delete_plan,
        crate::admin::handlers::get_usage,
        crate::public::get_public_file,
        crate::health::healthz,
        crate::health::ready,
    ),
    components(
        schemas(
            crate::drive::files::FileResponse,
            crate::drive::files::FileListResponse,
            crate::drive::files::UrlResponse,
            crate::drive::folders::FolderResponse,
            crate::drive::folders::FolderListResponse,
            crate::drive::folders::CreateFolderRequest,
            crate::drive::folders::RenameFolderRequest,
            crate::billing::status_handler::BillingStatusResponse,
            crate::admin::handlers::CreateTokenRequest,
            crate::admin::handlers::CreatedAdminToken,
            crate::admin::handlers::AdminTokenItem,
            crate::admin::handlers::AdminTokenList,
            crate::admin::handlers::CreateRuleRequest,
            crate::admin::handlers::BillingRuleItem,
            crate::admin::handlers::BillingRuleList,
            crate::admin::handlers::SetPlanRequest,
            crate::admin::handlers::PlanResponse,
            crate::admin::handlers::UsageResponse,
            crate::model::Owner,
            crate::model::Limits,
            crate::model::Plan,
            crate::error::ErrorBody,
            crate::error::ErrorDetail,
        ),
    ),
    modifiers(&SecurityAddon),
)]
pub struct ApiDoc;

/// Registers the two authentication schemes referenced by the per-path
/// `security(...)` annotations.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer_auth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
        components.add_security_scheme(
            "admin_token",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "x-admin-token",
                "Admin API token created via /v1/admin/tokens",
            ))),
        );
    }
}

/// The OpenAPI document.
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_openapi_json_matches_generated_spec() {
        // Given: the checked-in root spec and the freshly generated one.
        let checked_in: serde_json::Value =
            serde_json::from_str(include_str!("../../openapi.json"))
                .expect("checked-in openapi.json must parse");
        let generated: serde_json::Value =
            serde_json::from_str(&openapi_spec().to_json().expect("serialize generated spec"))
                .expect("generated spec must parse");

        // Then: they are equal regardless of key order.
        assert_eq!(checked_in, generated);
    }
}
