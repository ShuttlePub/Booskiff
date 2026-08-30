//! Admin API surface. TODO(Wave3): token-authenticated management routes
//! (admin users, billing rules, plan assignments).

use axum::Router;

/// Admin router — placeholder until Wave-3 adds routes.
pub fn admin_router() -> Router<()> {
    Router::new()
}
