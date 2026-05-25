use std::path::PathBuf;

use config::{Config, Environment, File};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Default for a secret-bearing config field: an empty secret. Used by
/// `#[serde(default = "empty_secret")]` so a missing key deserialises to an
/// empty value rather than failing.
fn empty_secret() -> SecretString {
    SecretString::from(String::new())
}

/// (De)serialisation for `SecretString` config fields. `secrecy` deliberately
/// gives `SecretString` no `Serialize`, so `AppConfig`'s derive needs this:
/// it reads a plain string in and writes a fixed placeholder out, ensuring a
/// serialised config can never carry a real secret.
mod secret_str {
    use secrecy::SecretString;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(_v: &SecretString, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("[redacted]")
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SecretString, D::Error> {
        Ok(SecretString::from(String::deserialize(d)?))
    }
}

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
    pub tenancy: TenancyConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
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
    #[serde(default)]
    pub marketing: MarketingConfig,
}

/// `[marketing]`. Optional apex/`www` marketing site + blog served from
/// the same binary. Hard-isolated module — see `src/marketing/`. Disabled
/// by default; when enabled, the dispatch seam routes the apex and `www`
/// hosts to the marketing router and leaves every other host on the app
/// router unchanged. Boot invariants live in `AppConfig::validate_marketing`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MarketingConfig {
    pub enabled: bool,
    /// CTA + login link target on every marketing page. The marketing
    /// module never imports app code — this is the only handle it has on
    /// the app surface, so the extracted service points anywhere with one
    /// config change.
    pub app_url: String,
    /// Fully-qualified canonical origin (scheme + host, no trailing
    /// slash). Used for `<link rel="canonical">`, OG / JSON-LD absolute
    /// URLs, and the sitemap.
    pub canonical_origin: String,
    /// Belt-and-braces guard for subdomain labels that must never alias a
    /// tenant slug (`www`, `app`). The dispatch seam already routes
    /// apex/`www`/`app` explicitly; this list is asserted to be a subset
    /// of `domain::reserved_slugs::RESERVED` at boot so the two lists
    /// can't drift.
    pub reserved_subdomains: Vec<String>,
    pub blog_enabled: bool,
}

impl Default for MarketingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_url: String::new(),
            canonical_origin: String::new(),
            reserved_subdomains: vec!["www".into(), "app".into()],
            blog_enabled: true,
        }
    }
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ResendConfig {
    #[serde(default = "empty_secret", with = "secret_str")]
    pub api_key: SecretString,
}

impl Default for ResendConfig {
    fn default() -> Self {
        Self {
            api_key: empty_secret(),
        }
    }
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
    #[serde(default = "empty_secret", with = "secret_str")]
    pub client_secret: SecretString,
    pub redirect_url: String,
    pub scopes: Vec<String>,
    pub http_connect_timeout_ms: u64,
    pub http_request_timeout_ms: u64,
}

impl Default for GithubOauthConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: empty_secret(),
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
    /// Path-based public surface (`/status/<slug>`, `/api/public/v1/*` on the
    /// operator host). The slug always identifies the org; there is no
    /// ambient "default tenant".
    pub path_based_public_routes: bool,
    /// Wildcard subdomain public surface (`*.{public_status.base_domain}`).
    /// Requires a well-formed `public_status.base_domain`; a startup
    /// assertion refuses to boot otherwise.
    pub subdomain_public_routes: bool,
    /// Free-tier cap on the number of orgs a single user can own.
    pub free_tier_owner_org_limit: u32,
    /// Grace period before soft-deleted orgs *and users* are purged. Single
    /// source of truth for the recovery window: the daily retention job binds
    /// this, and the Privacy Policy's "recoverable for 30 days" line is
    /// asserted equal to it in tests.
    pub deletion_grace_period_days: u32,
}

impl Default for TenancyConfig {
    fn default() -> Self {
        Self {
            path_based_public_routes: true,
            subdomain_public_routes: false,
            free_tier_owner_org_limit: 3,
            deletion_grace_period_days: 30,
        }
    }
}

/// Long-horizon data-retention windows for the daily purge job. Every field
/// here is bound by `jobs::retention`; an unhonoured knob is worse than a
/// missing one, so `check_results_days` lives only in the ClickHouse
/// migration TTL (an env override here would have been silently ignored —
/// the TTL is baked at migration time, not re-issued as an ALTER on boot).
/// Other cadences live with their owner: OAuth-state and magic-link tokens
/// in their own short-cadence security jobs; expired invitations in the
/// invitations janitor; session idle/absolute timeouts in `[auth.session]`;
/// soft-deleted org/user grace in `tenancy.deletion_grace_period_days`;
/// server/app log retention in the Docker log driver.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(default)]
pub struct RetentionConfig {
    /// `login_attempts` rows older than this are deleted.
    pub login_attempts_days: u32,
    /// `quota_events` rows older than this are deleted.
    pub quota_events_days: u32,
    /// `org_audit_log` rows older than this are deleted.
    pub audit_log_days: u32,
    /// Days after an API token's `expires_at` before its row is
    /// hard-deleted. Live tokens never count against the per-user cap
    /// (`api_tokens::count_for_user` filters by expiry) so the only purpose
    /// of this window is to bound table growth and shrink the
    /// rotation-pattern leak from a compromised user reading their own
    /// `token_prefix` / `name` history.
    pub api_tokens_post_expiry_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            login_attempts_days: 180,
            quota_events_days: 90,
            audit_log_days: 730,
            api_tokens_post_expiry_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PublicStatusConfig {
    /// Base domain for `*.{base_domain}` per-org status pages (apex-wildcard
    /// shape). Used only when `tenancy.subdomain_public_routes = true`. A
    /// startup assertion refuses to boot when this is empty or has no dot in
    /// that mode — without that, the strip-suffix parser collapses to a bare
    /// dot match and accepts arbitrary `Host` headers.
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
            max_logo_size_bytes: 1_048_576,
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

/// `[quotas]`. Cache TTLs for plan/usage lookups.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct QuotasConfig {
    /// Plans change rarely; a few minutes of staleness is acceptable.
    pub plan_cache_ttl_secs: u64,
    /// Usage counts move fast under bursty creates; short TTL only.
    pub usage_cache_ttl_secs: u64,
}

impl Default for QuotasConfig {
    fn default() -> Self {
        Self {
            plan_cache_ttl_secs: 300,
            usage_cache_ttl_secs: 10,
        }
    }
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
    /// Per-(org, host, port) in-flight cap. Tenant-scoped, fail-fast.
    #[serde(default = "default_per_host_max_inflight")]
    pub per_host_max_inflight: usize,
    /// Process-wide RDAP concurrency cap (per TLD).
    #[serde(default = "default_rdap_max_inflight")]
    pub rdap_max_inflight: usize,
}

fn default_per_host_max_inflight() -> usize {
    2
}

fn default_rdap_max_inflight() -> usize {
    1
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
    #[serde(default = "empty_secret", with = "secret_str")]
    pub credentials_kek_base64: SecretString,
    /// CIDR ranges whose `X-Forwarded-For` header is honoured for client-IP
    /// extraction. The TCP peer's address is checked against this list; if
    /// it matches, the rightmost untrusted hop in XFF wins. Anything else
    /// falls back to the TCP peer (no spoofable header). Empty by default
    /// — operators behind a reverse proxy (Caddy / nginx / a CDN) MUST set
    /// this, otherwise every `ip_hash` written to the database collapses to
    /// the proxy's address and IP-keyed abuse/audit signals are useless.
    #[serde(default)]
    pub trusted_proxies: Vec<ipnet::IpNet>,
}

impl SecurityConfig {
    /// Returns Some(trimmed KEK string) if a non-empty value is configured, None otherwise.
    pub fn kek(&self) -> Option<&str> {
        let t = self.credentials_kek_base64.expose_secret().trim();
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
    #[serde(default = "empty_secret", with = "secret_str")]
    pub password: SecretString,
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
    /// Master on/off for OpenTelemetry trace export. Export is active
    /// only when this AND `grafana.enabled` are true.
    pub tracing_enabled: bool,
    #[serde(default)]
    pub grafana: GrafanaConfig,
    #[serde(default = "default_gauge_sample_interval_ms")]
    pub gauge_sample_interval_ms: u64,
}

fn default_gauge_sample_interval_ms() -> u64 {
    1000
}

fn default_trace_sample_ratio() -> f64 {
    // Capture every trace by default: request volume is low and a head
    // sample rate that drops 95% of an already-sparse stream leaves the
    // trace view effectively empty. Lower this once volume justifies the
    // ingest cost.
    1.0
}

/// OTLP trace export to Grafana Cloud (or any OTLP/HTTP collector).
/// Credentials never live in TOML — `api_key` is sourced only from
/// `STATUS_MONITOR_OBSERVABILITY__GRAFANA__API_KEY`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GrafanaConfig {
    #[serde(default)]
    pub enabled: bool,
    /// OTLP/HTTP base, no signal suffix (e.g.
    /// `https://otlp-gateway-<zone>.grafana.net/otlp`). The service
    /// appends `/v1/traces` (a value already ending in it is left as-is).
    #[serde(default)]
    pub otlp_endpoint: String,
    /// Grafana Cloud numeric instance / stack id (basic-auth username).
    #[serde(default)]
    pub instance_id: String,
    #[serde(default = "empty_secret", with = "secret_str")]
    pub api_key: SecretString,
    /// Head sampling ratio applied under a parent-based sampler.
    #[serde(default = "default_trace_sample_ratio")]
    pub trace_sample_ratio: f64,
}

impl Default for GrafanaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: String::new(),
            instance_id: String::new(),
            api_key: empty_secret(),
            trace_sample_ratio: default_trace_sample_ratio(),
        }
    }
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
                    .with_list_parse_key("dns.servers")
                    .with_list_parse_key("security.trusted_proxies"),
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
        if self.checker.per_host_max_inflight == 0 {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "checker.per_host_max_inflight must be >= 1"
            )));
        }
        if self.checker.rdap_max_inflight == 0 {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "checker.rdap_max_inflight must be >= 1"
            )));
        }
        Ok(())
    }

    /// Marketing-site boot invariants. Cheap startup errors, never
    /// panics in router construction. Skipped wholesale when
    /// `marketing.enabled = false` so self-host deployments need not set
    /// any of these.
    pub fn validate_marketing(&self) -> Result<()> {
        fn err(msg: String) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg))
        }
        let m = &self.marketing;
        if !m.enabled {
            return Ok(());
        }
        let base = self.public_status.base_domain.trim();
        if base.is_empty() || !base.contains('.') {
            return Err(err(format!(
                "marketing.enabled = true requires public_status.base_domain to be a non-empty FQDN (got {base:?})"
            )));
        }
        for (field, value) in [
            ("marketing.canonical_origin", m.canonical_origin.as_str()),
            ("marketing.app_url", m.app_url.as_str()),
        ] {
            let v = value.trim();
            if v.is_empty() {
                return Err(err(format!(
                    "{field} is required when marketing.enabled = true"
                )));
            }
            if !v.starts_with("https://") {
                return Err(err(format!("{field} must start with https:// (got {v:?})")));
            }
            if v.ends_with('/') {
                return Err(err(format!(
                    "{field} must not end with a trailing slash (got {v:?})"
                )));
            }
        }
        for sub in &m.reserved_subdomains {
            let lower = sub.to_ascii_lowercase();
            if !crate::domain::reserved_slugs::is_reserved(&lower) {
                return Err(err(format!(
                    "marketing.reserved_subdomains entry {sub:?} is not in \
                     domain::reserved_slugs::RESERVED_SLUGS — keep the two lists aligned"
                )));
            }
        }
        // The session cookie must not be scoped to a parent zone that the
        // marketing host inherits; otherwise the app's session ID rides
        // along to the apex and the marketing CDN cache becomes Vary:
        // Cookie. Host-only (empty Domain) is always safe.
        let cd = self.auth.session.cookie_domain.trim();
        if !cd.is_empty() {
            let stripped = cd.trim_start_matches('.');
            if stripped == base || base.ends_with(&format!(".{stripped}")) {
                return Err(err(format!(
                    "auth.session.cookie_domain={cd:?} overlaps marketing host {base:?}; \
                     leave cookie_domain empty (host-only) so the apex marketing surface \
                     is not Vary: Cookie"
                )));
            }
        }
        Ok(())
    }

    /// Trace-export config is a clean startup error when inconsistent,
    /// never a runtime panic. Credentials are required only when export
    /// is actually active (`tracing_enabled` AND `grafana.enabled`); the
    /// sample ratio is always range-checked.
    pub fn validate_observability(&self) -> Result<()> {
        fn err(msg: String) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg))
        }
        let g = &self.observability.grafana;
        let r = g.trace_sample_ratio;
        if !(0.0..=1.0).contains(&r) {
            return Err(err(format!(
                "observability.grafana.trace_sample_ratio must be in [0.0, 1.0] (got {r})"
            )));
        }
        if self.observability.tracing_enabled && g.enabled {
            if g.otlp_endpoint.trim().is_empty() {
                return Err(err(
                    "observability.grafana.otlp_endpoint is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
            if g.instance_id.trim().is_empty() {
                return Err(err(
                    "observability.grafana.instance_id is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
            if g.api_key.expose_secret().trim().is_empty() {
                return Err(err(
                    "STATUS_MONITOR_OBSERVABILITY__GRAFANA__API_KEY is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
        }
        Ok(())
    }
}
