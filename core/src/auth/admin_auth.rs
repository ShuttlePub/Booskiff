//! Admin token principal extraction. TODO(Wave2/Wave3): bearer token
//! hashing (SHA-256) and lookup against `admin_tokens`.

/// Authenticated admin identity.
pub struct AdminPrincipal {
    pub token_id: uuid::Uuid,
    pub name: String,
    pub role: Role,
}

/// Admin roles. Only `Admin` exists so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
}
