//! Request extractor for the authenticated account. TODO(Wave2):
//! `FromRequestParts<AppState>` implementation verifying the bearer token
//! and resolving the owner.

use crate::model::Owner;

/// The account identity attached to an authenticated request.
pub struct AccountContext {
    pub owner: Owner,
}
