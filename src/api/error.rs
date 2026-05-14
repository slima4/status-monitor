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
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    pub const FORBIDDEN: &str = "FORBIDDEN";
    // Maintenance + incident narration (operator surface).
    pub const INVALID_TIME_RANGE: &str = "INVALID_TIME_RANGE";
    pub const INVALID_COMPONENT_ID: &str = "INVALID_COMPONENT_ID";
    pub const INVALID_DURATION: &str = "INVALID_DURATION";
    pub const EMPTY_TITLE: &str = "EMPTY_TITLE";
    pub const TITLE_TOO_LONG: &str = "TITLE_TOO_LONG";
    pub const DESCRIPTION_TOO_LONG: &str = "DESCRIPTION_TOO_LONG";
    pub const EMPTY_MESSAGE: &str = "EMPTY_MESSAGE";
    pub const MESSAGE_TOO_LONG: &str = "MESSAGE_TOO_LONG";
    pub const INVALID_PHASE: &str = "INVALID_PHASE";
    pub const MAINTENANCE_COMPLETED: &str = "MAINTENANCE_COMPLETED";
    pub const MAINTENANCE_NOT_FOUND: &str = "MAINTENANCE_NOT_FOUND";
    pub const INCIDENT_NOT_FOUND: &str = "INCIDENT_NOT_FOUND";
    // Org management.
    pub const ORG_NOT_FOUND: &str = "ORG_NOT_FOUND";
    pub const ORG_DELETED: &str = "ORG_DELETED";
    pub const SLUG_TAKEN: &str = "SLUG_TAKEN";
    pub const SLUG_INVALID: &str = "SLUG_INVALID";
    pub const OWNER_ORG_LIMIT: &str = "OWNER_ORG_LIMIT";
    pub const NOT_AN_OWNER: &str = "NOT_AN_OWNER";
    pub const MEMBER_NOT_FOUND: &str = "MEMBER_NOT_FOUND";
    pub const LAST_OWNER: &str = "LAST_OWNER";
    pub const RESTORE_WINDOW_EXPIRED: &str = "RESTORE_WINDOW_EXPIRED";
    // Auth.
    pub const INVALID_STATE: &str = "INVALID_STATE";
    pub const SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
    pub const INVALID_TOKEN: &str = "INVALID_TOKEN";
    pub const ORG_REQUIRED: &str = "ORG_REQUIRED";
    pub const ORG_HEADER_INVALID: &str = "ORG_HEADER_INVALID";
    pub const EMAIL_NOT_VERIFIED: &str = "EMAIL_NOT_VERIFIED";
    pub const TOKEN_LIMIT: &str = "TOKEN_LIMIT";
    pub const TOKEN_NAME_INVALID: &str = "TOKEN_NAME_INVALID";
    pub const TOKEN_NOT_FOUND: &str = "TOKEN_NOT_FOUND";
    // Invitations.
    pub const ALREADY_MEMBER: &str = "ALREADY_MEMBER";
    pub const ALREADY_INVITED: &str = "ALREADY_INVITED";
    pub const INVITATION_INVALID: &str = "INVITATION_INVALID";
    pub const INVITATION_EMAIL_MISMATCH: &str = "INVITATION_EMAIL_MISMATCH";
    pub const INVITATIONS_LIMIT: &str = "INVITATIONS_LIMIT";
    pub const INVALID_ROLE: &str = "INVALID_ROLE";
    pub const INVALID_EMAIL: &str = "INVALID_EMAIL";
    pub const CSRF_PROTECTION: &str = "CSRF_PROTECTION";
    pub const FINGERPRINT_SALT_ROTATED: &str = "FINGERPRINT_SALT_ROTATED";
    // Magic-link sign-in (v1.1 — gated by auth.enabled_methods).
    pub const MAGIC_LINK_INVALID: &str = "MAGIC_LINK_INVALID";
}
