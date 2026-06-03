//! OAuth 2.1 Authorization Server backing the one-click MCP connector.
//!
//! Topology: this AS runs on the **app** host (where the user's session cookie
//! lives, so `/oauth/authorize` can authenticate them); the protected resource
//! is `/mcp` on its own host. The flow is the standard MCP one:
//!
//! ```text
//!  /mcp 401 + WWW-Authenticate(resource_metadata)
//!    → GET .well-known/oauth-protected-resource   (RFC 9728, mcp module)
//!    → GET .well-known/oauth-authorization-server  (RFC 8414, here)
//!    → POST /oauth/register                         (RFC 7591 DCR)
//!    → GET  /oauth/authorize  → login + consent     (PKCE S256, RFC 8707 resource)
//!    → POST /oauth/token      → audience-bound sm_live_ token
//! ```
//!
//! The access token is the existing read-only, org-bound, expiring scoped token,
//! now stamped with this MCP endpoint as its `audience`. Nothing here mints
//! write scopes.

mod authorize;
mod error;
mod metadata;
mod pkce;
mod register;
mod store;
mod token;

use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::app::AppState;
use crate::auth::scope::{Scope, ScopeSet};
use crate::config::AppConfig;

/// How often the sweeper purges expired authorization codes + refresh tokens.
const SWEEP_INTERVAL_SECS: u64 = 3600;

/// The only scopes an OAuth connector may be granted. Read-only by construction
/// — the consent flow never mints a write scope.
const ALLOWED_SCOPES: &[Scope] = &[Scope::TargetsRead, Scope::StatusPageRead];

/// Authorization-code lifetime. Short — it's redeemed immediately.
const CODE_TTL_SECS: i64 = 60;

/// Default + ceiling for the user-chosen access-token lifetime on the consent
/// screen. No "never" option: an automated, leak-prone connector token with no
/// expiry (and no refresh) is the one lifetime worth disallowing.
const DEFAULT_TOKEN_TTL_DAYS: u32 = 90;
const MAX_TOKEN_TTL_DAYS: u32 = 365;

/// Canonical OAuth URLs derived from config. `issuer` is the app origin; the
/// metadata + endpoint URLs hang off it. `resource` is the MCP audience.
struct OAuthUrls {
    issuer: String,
    resource: String,
}

impl OAuthUrls {
    fn from_cfg(cfg: &AppConfig) -> Self {
        Self {
            issuer: cfg.auth.public_base_url.trim_end_matches('/').to_string(),
            resource: cfg.mcp.resource_uri.trim_end_matches('/').to_string(),
        }
    }
    fn authorize_endpoint(&self) -> String {
        format!("{}/oauth/authorize", self.issuer)
    }
    fn token_endpoint(&self) -> String {
        format!("{}/oauth/token", self.issuer)
    }
    fn registration_endpoint(&self) -> String {
        format!("{}/oauth/register", self.issuer)
    }
    /// URL of the protected-resource metadata, for the `WWW-Authenticate`
    /// `resource_metadata` hint. Anchored on the resource's own origin.
    fn resource_metadata_url(&self) -> String {
        // The resource URI ends in `/mcp`; RFC 9728 puts the doc at the
        // origin root + `/.well-known/oauth-protected-resource`.
        match resource_origin(&self.resource) {
            Some(origin) => format!("{origin}/.well-known/oauth-protected-resource"),
            None => format!("{}/.well-known/oauth-protected-resource", self.issuer),
        }
    }
}

/// Acceptable redirect-URI shapes for registration + authorize. HTTPS for web
/// connectors; loopback HTTP for local tooling (mcp-remote). Everything else —
/// custom/native schemes, non-loopback HTTP, URIs with a fragment — is rejected.
/// Matching is always exact-string elsewhere; this only gates *registration*.
fn is_acceptable_redirect_uri(uri: &str) -> bool {
    let Ok(u) = url::Url::parse(uri) else {
        return false;
    };
    // No fragment (RFC 6749 §3.1.2) and no userinfo — `https://attacker@host/`
    // confuses the displayed authority and serves no legitimate purpose here.
    if u.fragment().is_some() || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.scheme() {
        "https" => u.host_str().is_some(),
        "http" => matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "::1")),
        _ => false,
    }
}

/// Scheme+host(+port) of a URL, no path. Used to anchor metadata on the
/// resource origin.
fn resource_origin(uri: &str) -> Option<String> {
    let u = url::Url::parse(uri).ok()?;
    let host = u.host_str()?;
    Some(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// Space-delimited scope string supported by the connector (for metadata).
fn supported_scope_string() -> String {
    ALLOWED_SCOPES
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Clamp a requested `scope` to the allowed read set. An empty/garbage request
/// grants the full read set; anything else grants requested ∩ allowed. Never
/// returns a write scope. Output is a canonical space-delimited string.
fn grant_scope(requested: Option<&str>) -> String {
    let allowed: ScopeSet = ScopeSet::from_strs(ALLOWED_SCOPES.iter().map(|s| s.as_str()));
    let mut granted: Vec<&'static str> = Vec::new();
    if let Some(req) = requested {
        for tok in req.split_whitespace() {
            if let Some(s) = Scope::parse(tok)
                && allowed.allows(s)
                && ALLOWED_SCOPES.contains(&s)
                && !granted.contains(&s.as_str())
            {
                granted.push(s.as_str());
            }
        }
    }
    if granted.is_empty() {
        return supported_scope_string();
    }
    granted.join(" ")
}

/// Periodic cleanup of dead OAuth rows: authorization codes past their (~60s)
/// TTL and refresh tokens past their family deadline. Bounds table growth from
/// abandoned consents + rotation churn. Mirrors the idempotency pruner / rate-
/// limit janitor — bound to the shutdown token so it can't outlive the process.
/// Expired refresh rows are safe to drop wholesale: once `expires_at` passes,
/// the whole family is dead, so removing its (used + current) rows costs no
/// replay-detection coverage.
pub fn spawn_sweeper(pool: PgPool, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tick.tick() => sweep(&pool).await,
            }
        }
    });
}

async fn sweep(pool: &PgPool) {
    for sql in [
        "DELETE FROM oauth_authorization_codes WHERE expires_at < now()",
        "DELETE FROM oauth_refresh_tokens WHERE expires_at < now()",
    ] {
        if let Err(e) = sqlx::query(sql).execute(pool).await {
            tracing::warn!(target: "oauth", error = %e, "oauth sweep failed");
        }
    }
}

/// The `WWW-Authenticate: Bearer …` value the MCP resource server returns on a
/// 401, pointing clients at the RFC 9728 resource metadata so they can discover
/// the AS. `None` when OAuth isn't configured (caller falls back to plain
/// `Bearer`).
pub fn www_authenticate_value(cfg: &AppConfig) -> Option<String> {
    if !cfg.mcp.oauth_enabled || cfg.mcp.resource_uri.is_empty() {
        return None;
    }
    let urls = OAuthUrls::from_cfg(cfg);
    Some(format!(
        "Bearer resource_metadata=\"{}\", scope=\"{}\"",
        urls.resource_metadata_url(),
        supported_scope_string()
    ))
}

/// Mount the OAuth endpoints. The RFC 9728 protected-resource metadata is
/// served whenever the MCP resource is configured; the AS endpoints require
/// `oauth_enabled`. Returns unstated routes to merge into the web router so
/// they inherit cookies + CSRF + the cross-cutting layers.
pub fn routes(cfg: &AppConfig) -> Router<AppState> {
    let mut r = Router::new();
    if cfg.mcp.enabled && !cfg.mcp.resource_uri.is_empty() {
        r = r
            .route(
                "/.well-known/oauth-protected-resource",
                get(metadata::protected_resource),
            )
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(metadata::protected_resource),
            );
    }
    if cfg.mcp.oauth_enabled {
        r = r
            .route(
                "/.well-known/oauth-authorization-server",
                get(metadata::authorization_server),
            )
            .route("/oauth/authorize", get(authorize::authorize_page))
            .route("/oauth/authorize/decision", post(authorize::decision))
            .route("/oauth/token", post(token::token))
            .route("/oauth/register", post(register::register));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_scope_defaults_to_full_read_set() {
        assert_eq!(grant_scope(None), "targets:read status_page:read");
        assert_eq!(grant_scope(Some("")), "targets:read status_page:read");
    }

    #[test]
    fn grant_scope_intersects_with_allowed() {
        assert_eq!(grant_scope(Some("targets:read")), "targets:read");
        // Write + unknown scopes are dropped; never granted.
        assert_eq!(grant_scope(Some("targets:write")), "targets:read status_page:read");
        assert_eq!(grant_scope(Some("targets:read targets:write")), "targets:read");
        assert_eq!(grant_scope(Some("bogus")), "targets:read status_page:read");
    }

    #[test]
    fn redirect_uri_acceptance() {
        // HTTPS with a host — accepted.
        assert!(is_acceptable_redirect_uri("https://claude.ai/api/mcp/callback"));
        // Loopback HTTP (mcp-remote) — accepted.
        assert!(is_acceptable_redirect_uri("http://localhost:8976/callback"));
        assert!(is_acceptable_redirect_uri("http://127.0.0.1:5000/cb"));
        // Non-loopback HTTP — rejected (no cleartext over the network).
        assert!(!is_acceptable_redirect_uri("http://evil.example.com/cb"));
        // Custom/native schemes — rejected for this web connector.
        assert!(!is_acceptable_redirect_uri("com.evil.app:/cb"));
        // Userinfo in the authority — rejected (authority-confusion).
        assert!(!is_acceptable_redirect_uri("https://attacker@victim.example/cb"));
        assert!(!is_acceptable_redirect_uri("https://u:p@victim.example/cb"));
        // Fragment — rejected (RFC 6749 forbids it on redirect URIs).
        assert!(!is_acceptable_redirect_uri("https://claude.ai/cb#frag"));
        // Garbage — rejected.
        assert!(!is_acceptable_redirect_uri("not a url"));
        assert!(!is_acceptable_redirect_uri("https://"));
    }

    #[test]
    fn resource_origin_strips_path() {
        assert_eq!(
            resource_origin("https://mcp.example.com/mcp").as_deref(),
            Some("https://mcp.example.com")
        );
        assert_eq!(
            resource_origin("http://localhost:8080/mcp").as_deref(),
            Some("http://localhost:8080")
        );
    }
}
