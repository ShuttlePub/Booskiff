//! JWKS fetching and key caching. TODO(Wave2): fetch, cache, and rotate
//! keys per trusted issuer.

use std::sync::Arc;

use crate::config::TrustedIssuer;
use crate::error::AppError;

/// Cache of JWKS keys per trusted issuer. Cheap to clone; later waves
/// share the inner cache across requests.
#[derive(Clone)]
pub struct JwksCache {
    issuers: Arc<[TrustedIssuer]>,
}

impl JwksCache {
    pub fn new(issuers: Vec<TrustedIssuer>) -> Self {
        Self {
            issuers: Arc::from(issuers),
        }
    }

    pub fn issuers(&self) -> &[TrustedIssuer] {
        &self.issuers
    }

    /// Return the decoding key for `issuer`. TODO(Wave2): real fetch with
    /// caching and refresh.
    pub async fn fetch(&self, _issuer: &str) -> Result<jsonwebtoken::DecodingKey, AppError> {
        Err(AppError::Internal("jwks fetch not implemented".into()))
    }
}
