//! Admin API surface: token lifecycle, billing rules, plan assignments,
//! and owner usage, authenticated by `X-Admin-Token` (see `auth::admin_auth`).

pub mod handlers;

pub use handlers::admin_router;
