//! OAuth 2.1 error responses (RFC 6749 §5.2 / RFC 8707).
//!
//! Token + registration endpoints return a JSON `{error, error_description}`
//! with the right status. The authorize endpoint either renders a safe error
//! page (when the client/redirect_uri can't be trusted) or redirects the error
//! back to the validated redirect_uri — that logic lives in `authorize.rs`;
//! this type just carries the code.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthError {
    InvalidRequest,
    InvalidGrant,
    UnsupportedGrantType,
    UnsupportedResponseType,
    AccessDenied,
    /// RFC 8707 — the requested `resource` is not this server's.
    InvalidTarget,
    /// RFC 7591 registration error.
    InvalidRedirectUri,
    ServerError,
}

impl OAuthError {
    /// The stable `error` code string.
    pub const fn code(self) -> &'static str {
        match self {
            OAuthError::InvalidRequest => "invalid_request",
            OAuthError::InvalidGrant => "invalid_grant",
            OAuthError::UnsupportedGrantType => "unsupported_grant_type",
            OAuthError::UnsupportedResponseType => "unsupported_response_type",
            OAuthError::AccessDenied => "access_denied",
            OAuthError::InvalidTarget => "invalid_target",
            OAuthError::InvalidRedirectUri => "invalid_redirect_uri",
            OAuthError::ServerError => "server_error",
        }
    }

    const fn status(self) -> StatusCode {
        match self {
            OAuthError::ServerError => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

/// JSON error body for the token + registration endpoints. `description` is a
/// safe, non-sensitive hint — never echoes secrets or internal detail.
pub struct OAuthErrorResponse {
    pub error: OAuthError,
    pub description: &'static str,
}

impl OAuthError {
    pub fn with(self, description: &'static str) -> OAuthErrorResponse {
        OAuthErrorResponse {
            error: self,
            description,
        }
    }
}

impl IntoResponse for OAuthErrorResponse {
    fn into_response(self) -> Response {
        (
            self.error.status(),
            Json(json!({
                "error": self.error.code(),
                "error_description": self.description,
            })),
        )
            .into_response()
    }
}
