//! Server-rendered UI module.
//!
//! Thin presentation layer that consumes existing `/api/v1/*` JSON endpoints
//! and renders askama templates. It owns no domain logic and no mutation
//! routes — every UI mutation hits an existing API endpoint directly.

pub mod assets;
pub mod auth;
pub mod error;
pub mod routes;
pub mod views;

pub use routes::routes;
