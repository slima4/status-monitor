//! Authentication primitives: OAuth flow, sessions, login audit, fingerprint
//! hashing. Endpoints sit in `api::handlers::auth` / `api::handlers::me` and
//! use these helpers.
//!
//! API tokens, invitations and magic-link verification are scaffolded by the
//! schema in `007_auth.up.sql` and the `EmailSender` trait in `crate::email`
//! but their flows arrive in later phases.

pub mod api_tokens;
pub mod email_norm;
pub mod fingerprint;
pub mod github;
pub mod invitations;
pub mod invitations_cleanup;
pub mod login_audit;
pub mod magic_link;
pub mod magic_link_cleanup;
pub mod oauth_state;
pub mod oauth_state_cleanup;
pub mod session;
pub mod token_hash;
pub mod url;

pub use fingerprint::{ensure_fingerprint_salt, hash_fingerprint};
