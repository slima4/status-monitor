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
    pub security: SecurityConfig,
    pub circuit_breaker: CircuitBreakerConfig,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub observability: ObservabilityConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub tenancy: TenancyConfig,
    #[serde(default)]
    pub email: TransactionalEmailConfig,
    #[serde(default)]
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TransactionalEmailConfig {
    /// Backend: "resend" (HTTP API), "log" (tracing only, dev default), or
    /// "memory" (in-process buffer for tests).
    pub provider: String,
    pub from_name: String,
    pub from_address: String,
    pub resend: ResendConfig,
}

impl Default for TransactionalEmailConfig {
    fn default() -> Self {
        Self {
            provider: "log".into(),
            from_name: "Status Monitor".into(),
            from_address: "no-reply@example.invalid".into(),
            resend: ResendConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ResendConfig {
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    pub enabled_methods: Vec<String>,
    pub fingerprint_salt: String,
    /// External base URL (scheme + host + optional port) used to build links
    /// the user sees in emails — invitation accept/decline, magic-link verify.
    /// Trailing slashes are tolerated. Required in production; dev defaults to
    /// `http://localhost:8080`.
    pub public_base_url: String,
    pub session: SessionConfig,
    pub github: GithubOauthConfig,
    pub invitations: InvitationsConfig,
    pub api_tokens: ApiTokensConfig,
    pub magic_link: MagicLinkConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled_methods: vec!["github_oauth".into()],
            fingerprint_salt: String::new(),
            public_base_url: "http://localhost:8080".into(),
            session: SessionConfig::default(),
            github: GithubOauthConfig::default(),
            invitations: InvitationsConfig::default(),
            api_tokens: ApiTokensConfig::default(),
            magic_link: MagicLinkConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SessionConfig {
    pub idle_timeout_days: u32,
    pub absolute_timeout_days: u32,
    pub cookie_name: String,
    pub cookie_secure: bool,
    pub cookie_domain: String,
    pub renew_on_use: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            idle_timeout_days: 30,
            absolute_timeout_days: 90,
            cookie_name: "_sm_session".into(),
            cookie_secure: true,
            cookie_domain: String::new(),
            renew_on_use: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GithubOauthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub scopes: Vec<String>,
    pub http_connect_timeout_ms: u64,
    pub http_request_timeout_ms: u64,
}

impl Default for GithubOauthConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            redirect_url: String::new(),
            scopes: vec!["user:email".into(), "read:user".into()],
            http_connect_timeout_ms: 5000,
            http_request_timeout_ms: 10000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct InvitationsConfig {
    pub expiry_hours: u32,
    pub max_pending_per_org: u32,
}

impl Default for InvitationsConfig {
    fn default() -> Self {
        Self {
            expiry_hours: 168,
            max_pending_per_org: 50,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ApiTokensConfig {
    pub max_per_user: u32,
    /// First N chars of every token surfaced in UI + used as a lookup-narrowing
    /// index. Single source of truth at INSERT and at lookup. Floor of 16 gives
    /// 48 bits of entropy in the prefix (collision-safe to ~16M tokens); a
    /// startup assertion refuses to boot below that.
    pub prefix_visible_chars: u32,
}

impl Default for ApiTokensConfig {
    fn default() -> Self {
        Self {
            max_per_user: 25,
            prefix_visible_chars: 16,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MagicLinkConfig {
    pub expiry_minutes: u32,
    /// Reserved for per-email throttling on `/auth/magic-link/request`.
    /// Currently parsed but not enforced — landing the gate logic without
    /// dropping anti-enum constant-time properties needs its own design pass.
    pub rate_limit_seconds: u32,
}

impl Default for MagicLinkConfig {
    fn default() -> Self {
        Self {
            expiry_minutes: 15,
            rate_limit_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TenancyConfig {
    /// When false, every request resolves to the default org provisioned at
    /// startup. When true, the active org is taken from the authenticated
    /// session (SaaS mode).
    pub enabled: bool,
    /// Slug used to find or create the default org at startup. The actual UUID
    /// is generated by Postgres and persisted; nothing about the id is
    /// configurable.
    pub default_org_slug: String,
    /// When `enabled = true`, the public status routes only respond when this
    /// is also true. Self-hosters get the public page (one org); SaaS gets it
    /// only after per-org status page routing lands.
    pub public_routes_enabled: bool,
    /// Free-tier cap on the number of orgs a single user can own.
    pub free_tier_owner_org_limit: u32,
    /// Grace period before soft-deleted orgs are purged.
    pub deletion_grace_period_days: u32,
    /// How often the purge worker wakes. Defaults to 24h; lower values are
    /// only useful for tests that don't want to wait a day for a tick.
    pub purge_interval_secs: u64,
}

impl Default for TenancyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_org_slug: "default".into(),
            public_routes_enabled: false,
            free_tier_owner_org_limit: 3,
            deletion_grace_period_days: 30,
            purge_interval_secs: 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct NotificationsConfig {
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub email: EmailConfig,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SlackConfig {
    pub enabled: bool,
    pub webhook_url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EmailConfig {
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_password: String,
    pub from: String,
    pub starttls: bool,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_password: String::new(),
            from: String::new(),
            starttls: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ApiConfig {
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub cors: CorsConfig,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub per_second: u32,
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            per_second: 50,
            burst: 100,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CorsConfig {
    pub enabled: bool,
    /// Origins allowed when `allow_any_origin` is false. Each entry must be a
    /// full origin (`https://app.example.com`) — wildcards are not parsed here.
    pub allowed_origins: Vec<String>,
    /// HTTP methods returned in `Access-Control-Allow-Methods`.
    pub allowed_methods: Vec<String>,
    /// When true, returns `Access-Control-Allow-Origin: *`. Mutually exclusive
    /// with `allowed_origins`.
    pub allow_any_origin: bool,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    pub allow_private_targets: bool,
    #[serde(default)]
    pub credentials_kek_base64: String,
}

impl SecurityConfig {
    /// Returns Some(trimmed KEK string) if a non-empty value is configured, None otherwise.
    pub fn kek(&self) -> Option<&str> {
        let t = self.credentials_kek_base64.trim();
        (!t.is_empty()).then_some(t)
    }
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
