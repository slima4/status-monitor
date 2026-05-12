use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::config::{LogFormat, ObservabilityConfig};

pub fn init(cfg: &ObservabilityConfig) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    let registry = tracing_subscriber::registry().with(filter);

    match cfg.log_format {
        LogFormat::Json => registry.with(fmt::layer().json()).init(),
        LogFormat::Pretty => registry.with(fmt::layer().pretty()).init(),
    }
}
