use std::path::PathBuf;

use config::{Config, Environment, File};
use serde::{Deserialize, Serialize};

use crate::error::Result;

const ENV_PREFIX: &str = "STATUS_MONITOR";
const ENV_SEPARATOR: &str = "__";
const DEFAULT_CONFIG_PATH: &str = "config/default.toml";
const CONFIG_PATH_ENV: &str = "STATUS_MONITOR_CONFIG_PATH";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub runtime: RuntimeConfig,
    pub checker: CheckerConfig,
    pub http_client: HttpClientConfig,
    pub dns: DnsConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub observability: ObservabilityConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub api_bind: String,
    pub metrics_bind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CheckerConfig {
    pub max_concurrent_checks: usize,
    pub default_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub default_check_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpClientConfig {
    pub pool_max_idle_per_host: usize,
    pub pool_idle_timeout_secs: u64,
    pub tcp_keepalive_secs: u64,
    pub http2_keep_alive_interval_secs: u64,
    pub http2_keep_alive_timeout_secs: u64,
    pub http2_keep_alive_while_idle: bool,
    pub user_agent: String,
    #[serde(default)]
    pub http2_prior_knowledge: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DnsConfig {
    pub cache_size: usize,
    pub positive_ttl_secs: u64,
    pub negative_ttl_secs: u64,
    pub servers: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,
    pub success_threshold: u32,
    pub open_duration_secs: u64,
    pub half_open_max_calls: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageConfig {
    pub postgres: PostgresConfig,
    pub clickhouse: ClickhouseConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostgresConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClickhouseConfig {
    pub url: String,
    pub database: String,
    pub user: String,
    pub password: String,
    pub batch_size: usize,
    pub batch_timeout_ms: u64,
    pub buffer_size: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SchedulerConfig {
    pub target_refresh_interval_secs: u64,
    pub jitter_pct: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObservabilityConfig {
    pub log_level: String,
    pub log_format: LogFormat,
    pub metrics_enabled: bool,
    pub tracing_enabled: bool,
    pub otlp_endpoint: String,
    #[serde(default = "default_gauge_sample_interval_ms")]
    pub gauge_sample_interval_ms: u64,
}

fn default_gauge_sample_interval_ms() -> u64 {
    1000
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Pretty,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let primary = std::env::var(CONFIG_PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_PATH));

        let builder = Config::builder()
            .add_source(File::from(primary).required(false))
            .add_source(
                Environment::with_prefix(ENV_PREFIX)
                    .prefix_separator("_")
                    .separator(ENV_SEPARATOR)
                    .try_parsing(true)
                    .list_separator(",")
                    .with_list_parse_key("dns.servers"),
            );

        let cfg = builder.build()?;
        Ok(cfg.try_deserialize()?)
    }
}
