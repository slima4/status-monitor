//! Resource quotas + request rate limiting.
//!
//! - [`service::QuotaService`] resolves an org's account and that account's
//!   effective plan (cached) and provides the friendly-error quota checks;
//!   every count spans the account's live orgs, and the race-safe guarantee is
//!   in the store INSERTs that pool the same way and take the same limit.
//! - [`ratelimit::RateLimitService`] is the per-account / per-user limiter.
//! - [`middleware::rate_limit_middleware`] wires the limiter into `/api/v1`.

pub mod middleware;
pub mod ratelimit;
pub mod service;

pub use middleware::rate_limit_middleware;
pub use ratelimit::{RateLimitCategory, RateLimitKey, RateLimitService};
pub use service::QuotaService;
