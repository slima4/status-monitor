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
    pub public_status: PublicStatusConfig,
    #[serde(default)]
    pub email: TransactionalEmailConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub quotas: QuotasConfig,
    #[serde(default)]
    pub rate_limits: RateLimitsConfig,
    #[serde(default)]
    pub abuse: AbuseConfig,
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
    // The pending-invitation cap moved to `plans.max_pending_invitations`
    // (one source of truth). A CI guard rejects re-reading the old key.
}

impl Default for InvitationsConfig {
    fn default() -> Self {
        Self { expiry_hours: 168 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ApiTokensConfig {
    // The per-user token cap moved to `plans.max_api_tokens_per_user` (one
    // source of truth). A CI guard rejects re-reading the old key.
    /// First N chars of every token surfaced in UI + used as a lookup-narrowing
    /// index. Single source of truth at INSERT and at lookup. Floor of 16 gives
    /// 48 bits of entropy in the prefix (collision-safe to ~16M tokens); a
    /// startup assertion refuses to boot below that.
    pub prefix_visible_chars: u32,
}

impl Default for ApiTokensConfig {
    fn default() -> Self {
        Self {
            prefix_visible_chars: 16,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MagicLinkConfig {
    pub expiry_minutes: u32,
    /// Per-email send throttle on `/auth/magic-link/request`: at most one
    /// real email per address per window, regardless of source IP. Enforced
    /// inside `tokio::spawn` so the response time stays anti-enum-safe.
    /// Set to `0` to disable the throttle.
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
    /// Path-based public surface (`/status`, `/api/public/v1/*` on the
    /// operator host). Defaults to `!tenancy.enabled` — self-host serves the
    /// single org's status page at the operator host; SaaS does not, because
    /// the path-based surface would expose the default org's data to every
    /// tenant. A startup assertion refuses to boot with this flag and
    /// `tenancy.enabled` both true.
    pub path_based_public_routes: bool,
    /// Wildcard subdomain public surface (`*.status.{public_status.base_domain}`).
    /// Defaults to `false`. Requires `tenancy.enabled = true` and a
    /// well-formed `public_status.base_domain`; a startup assertion refuses
    /// to boot otherwise.
    pub subdomain_public_routes: bool,
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
            path_based_public_routes: true,
            subdomain_public_routes: false,
            free_tier_owner_org_limit: 3,
            deletion_grace_period_days: 30,
            purge_interval_secs: 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PublicStatusConfig {
    /// Base domain for `*.status.{base_domain}` per-org status pages. Used
    /// only when `tenancy.subdomain_public_routes = true`. A startup
    /// assertion refuses to boot when this is empty or has no dot in that
    /// mode — without that, `format!(".status.{}", "")` matches arbitrary
    /// `Host` headers ending in `.status.` and the slug extractor accepts
    /// attacker-supplied hosts.
    pub base_domain: String,

    pub cache_max_orgs: u32,
    pub cache_ttl_secs: u64,
    /// Idle eviction caps memory when tenants churn faster than the purge
    /// worker can reach them.
    pub last_good_ttl_secs: u64,

    pub logo_dir: String,
    pub max_logo_size_bytes: u32,
    pub allowed_logo_mime_types: Vec<String>,
    pub max_logo_dimension_px: u32,

    pub default_brand_color: String,
    pub default_show_powered_by: bool,

    /// Second line of defence behind the Caddy-side limit.
    pub public_per_ip_rate_limit_per_min: u32,
}

impl Default for PublicStatusConfig {
    fn default() -> Self {
        Self {
            base_domain: String::new(),
            cache_max_orgs: 1000,
            cache_ttl_secs: 10,
            last_good_ttl_secs: 3600,
            logo_dir: "/var/lib/status-monitor/logos".into(),
            max_logo_size_bytes: 204_800,
            allowed_logo_mime_types: vec![
                "image/png".into(),
                "image/jpeg".into(),
                "image/webp".into(),
            ],
            max_logo_dimension_px: 1200,
            default_brand_color: "#3b82f6".into(),
            default_show_powered_by: true,
            public_per_ip_rate_limit_per_min: 60,
        }
    }
}

/// `[quotas]`. Cache TTLs for the (later) plan/usage lookups, plus the
/// self-host-only limit overrides. Schema/config only in this phase — no
/// code reads these values yet.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct QuotasConfig {
    /// Plans change rarely; a few minutes of staleness is acceptable.
    pub plan_cache_ttl_secs: u64,
    /// Usage counts move fast under bursty creates; short TTL only.
    pub usage_cache_ttl_secs: u64,
    pub self_host_overrides: SelfHostOverrides,
}

impl Default for QuotasConfig {
    fn default() -> Self {
        Self {
            plan_cache_ttl_secs: 300,
            usage_cache_ttl_secs: 10,
            self_host_overrides: SelfHostOverrides::default(),
        }
    }
}

/// Self-host single-tenant limit overrides. Only consulted when
/// `tenancy.enabled = false`; ignored in SaaS mode. Every cap is optional —
/// an unset field falls back to the plan default. Wiring happens in a later
/// phase; this is the config surface only.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SelfHostOverrides {
    pub enabled: bool,
    pub max_targets: Option<u32>,
    pub min_check_interval_secs: Option<u32>,
    pub retention_days: Option<u32>,
    pub max_members: Option<u32>,
    pub max_pending_invitations: Option<u32>,
    pub max_api_tokens_per_user: Option<u32>,
    pub max_public_components: Option<u32>,
    pub max_maintenance_windows: Option<u32>,
    pub max_logo_size_bytes: Option<u32>,
}

/// `[rate_limits]`. Most numbers come from the `plans` table; these are the
/// janitor cadence and the per-IP values Caddy enforces (kept here for
/// reference / parity). Validated `>= 1` at load (I6).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimitsConfig {
    pub per_ip: PerIpRateLimits,
    pub janitor: RateLimitJanitorConfig,
}

/// Per-IP limits Caddy enforces. Mirrored here so docs/ops have one place to
/// read the numbers; the app does not key on the TCP peer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PerIpRateLimits {
    pub public_pages_per_min: u32,
    pub auth_endpoints_per_min: u32,
    pub org_creations_per_day: u32,
}

impl Default for PerIpRateLimits {
    fn default() -> Self {
        Self {
            public_pages_per_min: 60,
            auth_endpoints_per_min: 10,
            org_creations_per_day: 3,
        }
    }
}

/// Idle-entry janitor cadence for the in-process rate-limit map.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RateLimitJanitorConfig {
    pub cleanup_interval_hours: u64,
    pub idle_threshold_hours: u64,
}

impl Default for RateLimitJanitorConfig {
    fn default() -> Self {
        Self {
            cleanup_interval_hours: 6,
            idle_threshold_hours: 24,
        }
    }
}

/// `[abuse]`. URL-pattern deny-list (regex, case-insensitive) plus the path
/// to the YAML domain deny-list. Patterns are validated at config load
/// (`AbuseGuard::validate`) so a bad regex is a clean startup error, not a
/// construction panic. When `hot_reload_enabled`, SIGHUP re-reads the
/// patterns and deny-list file and swaps them in atomically (a malformed
/// edit is rejected and the running rules stay). When it is `false` no
/// SIGHUP handler is installed, so a deny-list edit needs a restart **and**
/// SIGHUP keeps its OS default of terminating the process immediately
/// (no graceful drain) — use SIGTERM/SIGINT to stop the server in that
/// mode, not SIGHUP.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AbuseConfig {
    pub url_patterns_denied: Vec<String>,
    pub domain_denylist_path: String,
    /// Optional hosts-format reputation feed (e.g. a StevenBlack/hosts
    /// mirror). Empty = the reputation check is off. Read locally at
    /// startup and on the SIGHUP reload — never fetched on the check path.
    pub reputation_source_path: String,
    pub hot_reload_enabled: bool,
}

impl Default for AbuseConfig {
    fn default() -> Self {
        Self {
            url_patterns_denied: default_url_patterns(),
            domain_denylist_path: "config/abuse_denylist.yaml".into(),
            reputation_source_path: String::new(),
            hot_reload_enabled: false,
        }
    }
}

/// Conservative reconnaissance / pen-test URL patterns. These match URLs that
/// are virtually always attack probes, never legitimate monitoring targets.
fn default_url_patterns() -> Vec<String> {
    [
        r"/\.git(/|$)",
        r"/\.env(/|$)",
        r"/\.svn(/|$)",
        r"/\.hg(/|$)",
        r"/\.(DS_Store|htaccess|htpasswd|npmrc|bash_history)(/|$)",
        r"/\.aws/",
        r"/\.ssh/",
        r"/phpmyadmin",
        r"/(adminer|dbadmin)(\.php)?",
        r"/wp-admin",
        r"/wp-login",
        r"/wp-config",
        r"/xmlrpc\.php",
        r"/cgi-bin/",
        r"/vendor/phpunit",
        r"/(phpinfo|info)\.php",
        r"/server-(status|info)",
        r"/actuator(/|$)",
        r"/manager/html",
        r"/solr/",
        r"/\.well-known/security\.txt",
        r"/autodiscover/autodiscover\.xml",
        r"\.(php|jsp|asp|aspx)\?",
        r"\.(sql|bak|old|swp|dump)(/|$|\?)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
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
    // Per-IP API rate limiting moved to Caddy (it sees the real peer); the
    // in-process limiter is now per-org / per-user via [rate_limits] and the
    // plans table. The old `api.rate_limit` (PeerIpKeyExtractor) layer is
    // gone — behind a proxy it collapsed to one global bucket.
    #[serde(default)]
    pub cors: CorsConfig,
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

    /// Reject `< 1` quota / rate / interval values at load with a
    /// field-named error (I6). A bad number is a clean startup *config*
    /// error, never a `.expect()` crash-loop in router/layer construction.
    pub fn validate_quotas_and_limits(&self) -> Result<()> {
        fn ge1_u64(v: u64, field: &str) -> Result<()> {
            if v < 1 {
                return Err(crate::error::AppError::Other(anyhow::anyhow!(
                    "{field} must be >= 1 (got {v})"
                )));
            }
            Ok(())
        }
        fn ge1_opt(v: Option<u32>, field: &str) -> Result<()> {
            if let Some(x) = v
                && x < 1
            {
                return Err(crate::error::AppError::Other(anyhow::anyhow!(
                    "{field} must be >= 1 (got {x})"
                )));
            }
            Ok(())
        }
        ge1_u64(
            self.quotas.plan_cache_ttl_secs,
            "quotas.plan_cache_ttl_secs",
        )?;
        ge1_u64(
            self.quotas.usage_cache_ttl_secs,
            "quotas.usage_cache_ttl_secs",
        )?;
        ge1_u64(
            self.rate_limits.janitor.cleanup_interval_hours,
            "rate_limits.janitor.cleanup_interval_hours",
        )?;
        ge1_u64(
            self.rate_limits.janitor.idle_threshold_hours,
            "rate_limits.janitor.idle_threshold_hours",
        )?;
        let o = &self.quotas.self_host_overrides;
        ge1_opt(o.max_targets, "quotas.self_host_overrides.max_targets")?;
        ge1_opt(
            o.min_check_interval_secs,
            "quotas.self_host_overrides.min_check_interval_secs",
        )?;
        ge1_opt(
            o.retention_days,
            "quotas.self_host_overrides.retention_days",
        )?;
        ge1_opt(o.max_members, "quotas.self_host_overrides.max_members")?;
        ge1_opt(
            o.max_pending_invitations,
            "quotas.self_host_overrides.max_pending_invitations",
        )?;
        ge1_opt(
            o.max_api_tokens_per_user,
            "quotas.self_host_overrides.max_api_tokens_per_user",
        )?;
        ge1_opt(
            o.max_public_components,
            "quotas.self_host_overrides.max_public_components",
        )?;
        ge1_opt(
            o.max_maintenance_windows,
            "quotas.self_host_overrides.max_maintenance_windows",
        )?;
        ge1_opt(
            o.max_logo_size_bytes,
            "quotas.self_host_overrides.max_logo_size_bytes",
        )?;
        Ok(())
    }
}
