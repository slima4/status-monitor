use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;

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

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    PayloadTooLarge(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message): (StatusCode, &'static str) = match &self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "not found"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad request"),
            AppError::PayloadTooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload too large"),
            AppError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::debug!(error = %self, status = %status, "request rejected");
        }
        (status, Json(ErrorBody { error: message })).into_response()
    }
}
