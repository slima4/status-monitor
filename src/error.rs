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
    Other(#[from] anyhow::Error),
}
