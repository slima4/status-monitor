//! MCP authentication.
//!
//! Two halves:
//!  1. [`middleware`] — runs in front of the transport on every `/mcp` request.
//!     A credentialed request is fully validated: it resolves the `sm_live_`
//!     Bearer token, **requires** an org binding and a live membership, then
//!     injects [`AuthContext::ApiToken`] into request extensions. Reusing that
//!     type means the shared rate limiter's `CurrentOrg`/`CurrentUser`
//!     extractors work unchanged. A non-bound token is rejected: the single-org
//!     connector takes its org from the credential, never a tool argument or
//!     header. With no credential, only protocol discovery (initialize,
//!     `*_list`, ping) is allowed through, so MCP directories and clients can
//!     read the public tool catalog; tool execution always needs a token.
//!  2. [`McpAuth`] — what a tool reads back from its `RequestContext` to get the
//!     org + scopes and to scope-gate itself.
//!
//! The OAuth 2.1 resource-server path (Phase 3) plugs in here later; this static
//! token path is the dev/bring-up front door.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::api_tokens;
use crate::auth::scope::{Scope, ScopeSet};
use crate::domain::{OrgId, UserId};
use crate::storage::orgs::is_active_member;
use crate::web::auth::{AuthContext, bearer_from_headers};

use super::error::McpToolError;

/// Resolved MCP caller, read from request extensions inside a tool handler.
pub struct McpAuth {
    pub org: OrgId,
    pub scopes: ScopeSet,
    pub user_id: UserId,
    pub token_id: Uuid,
}

impl McpAuth {
    /// Pull the auth context the [`middleware`] injected. The transport forwards
    /// the HTTP request `Parts` into `RequestContext.extensions`; the
    /// middleware-inserted [`AuthContext`] lives in *those* parts' extensions.
    pub fn from_ctx(ctx: &RequestContext<RoleServer>) -> Result<Self, McpToolError> {
        let parts = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .ok_or_else(|| McpToolError::internal("request parts missing from tool context"))?;
        match parts.extensions.get::<AuthContext>() {
            Some(AuthContext::ApiToken {
                user_id,
                token_id,
                scopes,
                org: Some(org),
                ..
            }) => Ok(Self {
                org: *org,
                scopes: scopes.clone(),
                user_id: *user_id,
                token_id: *token_id,
            }),
            // The middleware guarantees a bound ApiToken before any tool runs;
            // anything else is a server bug, not a caller error.
            _ => Err(McpToolError::unauthenticated(
                "no authenticated, org-bound token on this request",
            )),
        }
    }

    /// Gate the calling tool on `required`; surfaces a tool-execution error the
    /// model can read rather than a bare protocol failure.
    pub fn require(&self, required: Scope) -> Result<(), McpToolError> {
        if self.scopes.allows(required) {
            Ok(())
        } else {
            Err(McpToolError::insufficient_scope(required.as_str()))
        }
    }
}

/// 401 with a `WWW-Authenticate: Bearer` challenge. When OAuth is configured it
/// carries the RFC 9728 `resource_metadata` pointer + `scope`, so a conformant
/// client can discover the authorization server and start the flow; otherwise a
/// plain `Bearer` (static-token mode).
fn challenge(state: &AppState) -> Response {
    let mut resp = (StatusCode::UNAUTHORIZED, "authentication required").into_response();
    let value = crate::oauth::www_authenticate_value(&state.cfg)
        .and_then(|v| HeaderValue::from_str(&v).ok())
        .unwrap_or_else(|| HeaderValue::from_static("Bearer"));
    resp.headers_mut().insert(header::WWW_AUTHENTICATE, value);
    resp
}

fn forbidden(message: &'static str) -> Response {
    (StatusCode::FORBIDDEN, message).into_response()
}

/// Max body we'll buffer to classify an *unauthenticated* request's JSON-RPC
/// method. Discovery requests are tiny; a larger body with no credential is
/// rejected rather than buffered.
const MAX_DISCOVERY_BODY: usize = 64 * 1024;

/// MCP methods that expose only the handshake and the public tool catalog (no
/// org data), so they're safe to serve without a credential.
fn is_public_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "notifications/initialized"
            | "ping"
            | "tools/list"
            | "resources/list"
            | "resources/templates/list"
            | "prompts/list"
    )
}

/// True only when every JSON-RPC message in `body` is a public discovery method.
/// A single object or a batch array are both accepted; an empty batch, a
/// missing/non-string method, or any non-discovery method fails closed.
fn all_methods_public(body: &[u8]) -> bool {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let msgs = match &json {
        serde_json::Value::Array(a) => a.iter().collect::<Vec<_>>(),
        other => vec![other],
    };
    !msgs.is_empty()
        && msgs.iter().all(|m| {
            m.get("method")
                .and_then(serde_json::Value::as_str)
                .is_some_and(is_public_method)
        })
}

/// Front of the transport on every `/mcp` request. A credentialed request is
/// fully authenticated; an uncredentialed one is allowed through only for
/// protocol discovery so directories and clients can read the public catalog.
pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if bearer_from_headers(req.headers()).is_none() {
        return discovery_or_challenge(&state, req, next).await;
    }
    authenticate(state, req, next).await
}

/// Let an uncredentialed request through only when its POST body is pure MCP
/// discovery; otherwise return the 401 challenge. The downstream rate limiter
/// sees no org and passes through, so Caddy's per-IP tier bounds this surface.
async fn discovery_or_challenge(state: &AppState, req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::POST {
        return challenge(state);
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_DISCOVERY_BODY).await else {
        return challenge(state);
    };
    if !all_methods_public(&bytes) {
        return challenge(state);
    }
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

/// Validate the Bearer credential, inject [`AuthContext`], lazily bump
/// `last_used_at`, then run the request. A missing, invalid, or non-bound token
/// is rejected here.
async fn authenticate(state: AppState, mut req: Request, next: Next) -> Response {
    let Some(raw) = bearer_from_headers(req.headers()) else {
        return challenge(&state);
    };
    if !raw.starts_with(api_tokens::TOKEN_PREFIX) {
        return challenge(&state);
    }
    let Some(pool) = state.db.as_ref() else {
        return challenge(&state);
    };

    let prefix_len = state.cfg.auth.api_tokens.prefix_visible_chars as usize;
    let row = match api_tokens::lookup_by_raw(pool, raw, prefix_len).await {
        Ok(api_tokens::LookupOutcome::Active(row)) => row,
        Ok(api_tokens::LookupOutcome::Invalid) => return challenge(&state),
        Err(err) => {
            tracing::warn!(target: "mcp", error = %err, "mcp token lookup failed");
            return challenge(&state);
        }
    };

    // Audience binding (RFC 8707): an OAuth-minted token carries the MCP
    // resource URI; it must match ours. A token minted for any other resource
    // is rejected — we never honour a token issued for a different audience. A
    // `None` audience is a manually-minted static token (the documented non-
    // OAuth convenience), accepted as before.
    if let Some(aud) = row.audience.as_deref() {
        let resource = state.cfg.mcp.resource_uri.trim_end_matches('/');
        if resource.is_empty() || aud.trim_end_matches('/') != resource {
            return challenge(&state);
        }
    }

    // The connector is single-org: the org comes from the token binding only.
    let Some(org) = row.org else {
        return forbidden("this token is not bound to an organization; mint an org-scoped token");
    };

    // Re-check membership on every request (defense vs revoked access). The
    // rate limiter's CurrentOrg will re-verify too; one extra indexed lookup on
    // a low-volume, LLM-driven path is an acceptable cost for failing closed.
    match is_active_member(pool, row.user_id, org).await {
        Ok(true) => {}
        Ok(false) => {
            return forbidden("the token owner is no longer a member of this organization");
        }
        Err(err) => {
            tracing::warn!(target: "mcp", error = %err, "mcp membership check failed");
            return challenge(&state);
        }
    }

    let token_id = row.id;
    req.extensions_mut().insert(AuthContext::ApiToken {
        user_id: row.user_id,
        token_id,
        scopes: row.scopes,
        org: Some(org),
    });

    if api_tokens::should_touch(&state.api_token_debounce, token_id) {
        let cache = state.api_token_debounce.clone();
        let pool = pool.clone();
        tokio::spawn(async move {
            if let Err(err) = api_tokens::touch_last_used_debounced(&pool, &cache, token_id).await {
                tracing::warn!(target: "mcp", error = %err, "mcp last_used bump failed");
            }
        });
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_with(scopes: &[&str]) -> McpAuth {
        McpAuth {
            org: OrgId(uuid::Uuid::nil()),
            scopes: ScopeSet::from_strs(scopes.iter().copied()),
            user_id: UserId(uuid::Uuid::nil()),
            token_id: uuid::Uuid::nil(),
        }
    }

    #[test]
    fn require_allows_granted_scope() {
        assert!(
            auth_with(&["targets:read"])
                .require(Scope::TargetsRead)
                .is_ok()
        );
        // write implies read.
        assert!(
            auth_with(&["targets:write"])
                .require(Scope::TargetsRead)
                .is_ok()
        );
        assert!(
            auth_with(&["full_access"])
                .require(Scope::TargetsRead)
                .is_ok()
        );
    }

    #[test]
    fn require_denies_missing_scope() {
        let err = auth_with(&["status_page:read"])
            .require(Scope::TargetsRead)
            .unwrap_err();
        assert_eq!(err.code, super::super::error::codes::INSUFFICIENT_SCOPE);
    }

    #[test]
    fn public_methods_are_discovery_only() {
        for m in [
            "initialize",
            "notifications/initialized",
            "ping",
            "tools/list",
            "resources/list",
            "prompts/list",
        ] {
            assert!(is_public_method(m), "{m} should be public");
        }
        for m in [
            "tools/call",
            "resources/read",
            "prompts/get",
            "completion/complete",
        ] {
            assert!(!is_public_method(m), "{m} must require a credential");
        }
    }

    #[test]
    fn discovery_classifier_fails_closed() {
        assert!(all_methods_public(br#"{"method":"tools/list"}"#));
        assert!(all_methods_public(
            br#"[{"method":"initialize"},{"method":"notifications/initialized"}]"#
        ));
        assert!(!all_methods_public(br#"{"method":"tools/call"}"#));
        assert!(!all_methods_public(
            br#"[{"method":"tools/list"},{"method":"tools/call"}]"#
        ));
        assert!(!all_methods_public(br#"{"id":1}"#));
        assert!(!all_methods_public(b"[]"));
        assert!(!all_methods_public(b"not json"));
    }
}
