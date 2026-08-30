//! OpenAPI document generation and Swagger UI wiring.

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(info(title = "Booskiff", version = "0.1.0"))]
struct ApiDoc {}

/// The OpenAPI document; endpoint annotations are attached to handlers in
/// later waves.
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
