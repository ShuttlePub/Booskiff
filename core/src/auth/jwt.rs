//! JWT claim shapes and token verification. TODO(Wave2): verification
//! against the JWKS cache.

use crate::error::AppError;
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

/// Verify `token` and return its claims. TODO(Wave2): signature, issuer,
/// audience, and expiry checks.
pub async fn verify_token(_token: &str) -> Result<Claims, AppError> {
    Err(AppError::Internal("jwt verify not implemented".into()))
}
