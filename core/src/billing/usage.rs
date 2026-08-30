//! Storage usage metering repos. TODO(Wave5): SQL implementations.

use crate::error::AppError;
use crate::model::Owner;

/// Received bytes currently metered for `owner`.
pub async fn used_bytes(_pool: &sqlx::PgPool, _owner: &Owner) -> Result<i64, AppError> {
    Err(AppError::Internal("usage repo not implemented yet".into()))
}

/// Add `delta` received bytes to `owner`'s usage (insert-or-increment).
pub async fn increment_used_bytes(
    _pool: &sqlx::PgPool,
    _owner: &Owner,
    _delta: i64,
) -> Result<(), AppError> {
    Err(AppError::Internal("usage repo not implemented yet".into()))
}

/// Subtract `delta` from `owner`'s usage, clamped at zero.
pub async fn decrement_used_bytes(
    _pool: &sqlx::PgPool,
    _owner: &Owner,
    _delta: i64,
) -> Result<(), AppError> {
    Err(AppError::Internal("usage repo not implemented yet".into()))
}
