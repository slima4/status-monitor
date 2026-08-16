//! Where results and state are persisted.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::{empty_secret, secret_str};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageConfig {
    pub postgres: PostgresConfig,
    pub clickhouse: ClickhouseConfig,
    /// Opt-in for a local stack running on the shipped `monitor` credentials.
    #[serde(default)]
    pub allow_default_credentials: bool,
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
    #[serde(default = "empty_secret", with = "secret_str")]
    pub password: SecretString,
    pub batch_size: usize,
    pub batch_timeout_ms: u64,
    pub buffer_size: usize,
    /// Coalesce the batcher's small inserts into larger parts server-side so a
    /// single-node server doesn't drown in tiny parts. `wait_for_async_insert`
    /// stays on, so the durability ack the retry/dedup path relies on holds.
    #[serde(default = "default_async_insert")]
    pub async_insert: bool,
}

fn default_async_insert() -> bool {
    true
}
