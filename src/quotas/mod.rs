//! Resource quotas + request rate limiting.
//!
//! - [`service::QuotaService`] resolves an org's effective plan (cached) and
//!   provides the friendly-error quota checks; the race-safe guarantee is in
//!   the store INSERTs that take the same limit number.
//! - [`ratelimit::RateLimitService`] is the per-org / per-user limiter.
//! - [`middleware::rate_limit_middleware`] wires the limiter into `/api/v1`.

pub mod middleware;
pub mod ratelimit;
pub mod service;

pub use middleware::rate_limit_middleware;
pub use ratelimit::{RateLimitCategory, RateLimitKey, RateLimitService};
pub use service::QuotaService;
