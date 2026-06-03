//! Discovery metadata: RFC 9728 (protected resource) + RFC 8414 (auth server).
//! Both are public, unauthenticated, cacheable GETs.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::app::AppState;

use super::{OAuthUrls, supported_scope_string};

/// RFC 9728 Protected Resource Metadata for `/mcp`. Served at the resource
/// origin's `/.well-known/oauth-protected-resource` (+ path-scoped variant).
pub async fn protected_resource(State(state): State<AppState>) -> Json<Value> {
    let urls = OAuthUrls::from_cfg(&state.cfg);
    Json(json!({
        "resource": urls.resource,
        "authorization_servers": [urls.issuer],
        "scopes_supported": super::ALLOWED_SCOPES
            .iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "bearer_methods_supported": ["header"],
    }))
}

/// RFC 8414 Authorization Server Metadata. PKCE S256 only; public clients
/// (`none` auth method); authorization-code grant only.
pub async fn authorization_server(State(state): State<AppState>) -> Json<Value> {
    let urls = OAuthUrls::from_cfg(&state.cfg);
    Json(json!({
        "issuer": urls.issuer,
        "authorization_endpoint": urls.authorize_endpoint(),
        "token_endpoint": urls.token_endpoint(),
        "registration_endpoint": urls.registration_endpoint(),
        "scopes_supported": super::ALLOWED_SCOPES
            .iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scope": supported_scope_string(),
    }))
}
