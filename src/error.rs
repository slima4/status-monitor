use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

use crate::api::error::{ApiError, ApiErrorBody, codes};

pub type Result<T, E = AppError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid bind address {addr}: {source}")]
    BindAddr {
        addr: String,
        #[source]
        source: std::net::AddrParseError,
    },

    #[error("{message}")]
    NotFound { code: &'static str, message: String },

    #[error("{message}")]
    BadRequest {
        code: &'static str,
        message: String,
        field: Option<String>,
    },

    #[error("{message}")]
    PayloadTooLarge { code: &'static str, message: String },

    #[error("{message}")]
    Conflict { code: &'static str, message: String },

    #[error("{message}")]
    Unprocessable { code: &'static str, message: String },

    #[error("authentication required")]
    Unauthorized,

    #[error("access denied")]
    Forbidden,

    /// Coded forbidden — same HTTP 403 as [`Self::Forbidden`] but with a
    /// stable error code and message so handlers can carry context (e.g.
    /// `EMAIL_NOT_VERIFIED`).
    #[error("{message}")]
    ForbiddenCoded { code: &'static str, message: String },

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest {
            code,
            message: message.into(),
            field: None,
        }
    }

    pub fn bad_request_field(
        code: &'static str,
        message: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        Self::BadRequest {
            code,
            message: message.into(),
            field: Some(field.into()),
        }
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::NotFound {
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    pub fn payload_too_large(code: &'static str, message: impl Into<String>) -> Self {
        Self::PayloadTooLarge {
            code,
            message: message.into(),
        }
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::Unprocessable {
            code,
            message: message.into(),
        }
    }

    pub fn forbidden_code(code: &'static str, message: impl Into<String>) -> Self {
        Self::ForbiddenCoded {
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AppError::NotFound { code, message } => {
                (StatusCode::NOT_FOUND, ApiErrorBody::new(code, message))
            }
            AppError::BadRequest {
                code,
                message,
                field,
            } => (
                StatusCode::BAD_REQUEST,
                ApiErrorBody {
                    code,
                    message,
                    field,
                    details: None,
                    trace_id: None,
                },
            ),
            AppError::PayloadTooLarge { code, message } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                ApiErrorBody::new(code, message),
            ),
            AppError::Conflict { code, message } => {
                (StatusCode::CONFLICT, ApiErrorBody::new(code, message))
            }
            AppError::Unprocessable { code, message } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ApiErrorBody::new(code, message),
            ),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ApiErrorBody::new(codes::UNAUTHORIZED, "authentication required"),
            ),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                ApiErrorBody::new(codes::FORBIDDEN, "access denied"),
            ),
            AppError::ForbiddenCoded { code, message } => {
                (StatusCode::FORBIDDEN, ApiErrorBody::new(code, message))
            }
            ref err @ (AppError::Config(_)
            | AppError::Io(_)
            | AppError::BindAddr { .. }
            | AppError::Other(_)) => {
                tracing::error!(error = %err, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ApiErrorBody::new(codes::INTERNAL, "internal error"),
                )
            }
        };
        if status.is_client_error() {
            tracing::debug!(code = body.code, status = %status, "request rejected");
        }
        (status, Json(ApiError { error: body })).into_response()
    }
}
