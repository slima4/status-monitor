use serde::Serialize;
use utoipa::ToSchema;

/// Top-level envelope returned for every 4xx/5xx response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiErrorBody {
    /// Stable machine-readable code (UPPER_SNAKE_CASE).
    #[schema(example = "INVALID_URL_SCHEME")]
    pub code: &'static str,
    /// Human-readable, safe to display.
    #[schema(example = "URL scheme must be http or https.")]
    pub message: String,
    /// JSON pointer to the offending field for 400s.
    #[schema(example = "check.url", nullable = true)]
    pub field: Option<String>,
    /// Optional structured context.
    #[schema(nullable = true)]
    pub details: Option<serde_json::Value>,
    /// W3C traceparent for support.
    #[schema(example = "00-7c3a4f...-01", nullable = true)]
    pub trace_id: Option<String>,
}

impl ApiErrorBody {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            field: None,
            details: None,
            trace_id: None,
        }
    }

    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

/// Stable error codes returned by the API. Adding new codes is non-breaking;
/// renaming or repurposing existing ones is breaking.
pub mod codes {
    pub const INVALID_JSON: &str = "INVALID_JSON";
    pub const INVALID_CONTENT_TYPE: &str = "INVALID_CONTENT_TYPE";
    pub const MISSING_FIELD: &str = "MISSING_FIELD";
    pub const INVALID_URL_SCHEME: &str = "INVALID_URL_SCHEME";
    pub const INVALID_URL_FORMAT: &str = "INVALID_URL_FORMAT";
    pub const SSRF_BLOCKED: &str = "SSRF_BLOCKED";
    pub const INVALID_INTERVAL: &str = "INVALID_INTERVAL";
    pub const INVALID_TIMEOUT: &str = "INVALID_TIMEOUT";
    pub const INVALID_TCP_PORT: &str = "INVALID_TCP_PORT";
    pub const INVALID_TCP_HOST: &str = "INVALID_TCP_HOST";
    pub const INVALID_STATUS_RANGE: &str = "INVALID_STATUS_RANGE";
    pub const INVALID_HTTP_METHOD: &str = "INVALID_HTTP_METHOD";
    pub const INVALID_TLS_CERT_PARAMS: &str = "INVALID_TLS_CERT_PARAMS";
    pub const INVALID_DOMAIN_PARAMS: &str = "INVALID_DOMAIN_PARAMS";
    pub const INVALID_TLS_CRED_COMBO: &str = "INVALID_TLS_CRED_COMBO";
    pub const INVALID_ALERT_CONFIG: &str = "INVALID_ALERT_CONFIG";
    pub const INVALID_CONFIG: &str = "INVALID_CONFIG";
    pub const EMPTY_NAME: &str = "EMPTY_NAME";
    pub const REDACTION_SENTINEL: &str = "REDACTION_SENTINEL";
    pub const BULK_EMPTY: &str = "BULK_EMPTY";
    pub const BULK_VALIDATION: &str = "BULK_VALIDATION";
    pub const BULK_TOO_LARGE: &str = "BULK_TOO_LARGE";
    pub const BAD_TIME_RANGE: &str = "BAD_TIME_RANGE";
    pub const TARGET_NOT_FOUND: &str = "TARGET_NOT_FOUND";
    pub const TARGET_DUPLICATE: &str = "TARGET_DUPLICATE";
    pub const CIRCUIT_OPEN: &str = "CIRCUIT_OPEN";
    pub const DEPENDENCY_DOWN: &str = "DEPENDENCY_DOWN";
    pub const RATE_LIMITED: &str = "RATE_LIMITED";
    pub const INTERNAL: &str = "INTERNAL";
}
