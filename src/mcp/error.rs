//! Tool-execution errors.
//!
//! Input-validation and business errors are returned to the model as
//! `isError: true` structured data (`{code, message, retryable}`), not as
//! JSON-RPC protocol errors — so the model can read the failure and self-
//! correct. Protocol errors stay reserved for unknown-tool / malformed-request
//! / auth, which the transport + auth middleware handle before a tool ever
//! runs. The `Result<Json<T>, McpToolError>` return shape lets the `#[tool]`
//! macro derive the success `outputSchema` while routing `Err` through
//! `IntoCallToolResult` to a tool-execution error.

use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::CallToolResult;
use serde_json::json;

/// Stable machine codes the model (and our tests) can branch on.
pub mod codes {
    pub const INVALID_ARGUMENT: &str = "invalid_argument";
    pub const NOT_FOUND: &str = "not_found";
    pub const INSUFFICIENT_SCOPE: &str = "insufficient_scope";
    /// A write tool's elicitation was declined, cancelled, or unavailable.
    pub const NOT_CONFIRMED: &str = "not_confirmed";
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    pub const INTERNAL: &str = "internal";
}

/// A side-effect-free tool failure surfaced as structured `isError` content.
#[derive(Debug, Clone)]
pub struct McpToolError {
    pub code: &'static str,
    pub message: String,
    /// Hint to the caller: would an identical retry plausibly succeed later?
    pub retryable: bool,
}

impl McpToolError {
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_ARGUMENT, message, false)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, message, false)
    }

    pub fn insufficient_scope(scope: &str) -> Self {
        Self::new(
            codes::INSUFFICIENT_SCOPE,
            format!("the connector's token is missing the required scope `{scope}`"),
            false,
        )
    }

    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(codes::UNAUTHENTICATED, message, false)
    }

    /// A server-side fault (DB down, serialization). Retryable — the caller's
    /// arguments were fine. The detail is logged, not leaked.
    pub fn internal(context: impl AsRef<str>) -> Self {
        tracing::warn!(target: "mcp", detail = context.as_ref(), "mcp tool internal error");
        Self::new(
            codes::INTERNAL,
            "an internal error occurred; the request can be retried",
            true,
        )
    }
}

impl IntoCallToolResult for McpToolError {
    fn into_call_tool_result(self) -> Result<CallToolResult, rmcp::ErrorData> {
        // `structured_error` sets `is_error: true`; the Result glue in rmcp
        // keeps it that way, producing a tool-execution error rather than a
        // protocol error.
        Ok(CallToolResult::structured_error(json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "retryable": self.retryable,
            }
        })))
    }
}
