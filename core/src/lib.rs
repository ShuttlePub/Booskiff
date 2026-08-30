//! Booskiff core service — drive/storage API foundation.
//!
//! Wave-1 scaffolding: frozen module contracts with minimal compiling
//! bodies; later waves fill in handlers, extractors, and repos.
//!
//! NOTE: runtime queries only (no `sqlx::query!` macros). All database
//! access goes through runtime-checked queries so the crate builds
//! without a live database.

pub mod admin;
pub mod api_doc;
pub mod auth;
pub mod billing;
pub mod config;
pub mod drive;
pub mod error;
pub mod health;
pub mod model;
pub mod public;
pub mod state;
pub mod storage;
