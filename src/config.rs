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

const ENV_PREFIX: &str = "UPTIMEPAGE";
const ENV_SEPARATOR: &str = "__";
const DEFAULT_CONFIG_PATH: &str = "config/default.toml";
const CONFIG_PATH_ENV: &str = "UPTIMEPAGE_CONFIG_PATH";

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
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub escalation: EscalationConfig,
    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub flow: FlowConfig,
    #[serde(default)]
    pub operator: OperatorConfig,
    #[serde(default)]
    pub telegram: TelegramBotConfig,
    #[serde(default)]
    pub whatsapp_app: WhatsAppAppBotConfig,
    #[serde(default)]
    pub slack_oauth: ConnectOauthConfig,
    #[serde(default)]
    pub discord_oauth: ConnectOauthConfig,
    #[serde(default)]
    pub bootstrap: BootstrapConfig,
}

/// Unattended first-run seeding, for app-store installs that have no terminal.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BootstrapConfig {
    /// Owner to seed when the instance has no users yet. Empty disables it.
    pub email: String,
    pub org_name: String,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            email: String::new(),
            org_name: "My Org".to_string(),
        }
    }
}

/// `[slack_oauth]` / `[discord_oauth]`. Credentials of an operator-owned
/// OAuth app behind a one-click connect button: the dance hands back a
/// ready-made webhook URL so the user never copies one by hand. Empty
/// credentials hide the button; the manual-paste kind works either way.
/// Env only, never a config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ConnectOauthConfig {
    pub client_id: String,
    #[serde(default = "empty_secret", with = "secret_str")]
    pub client_secret: SecretString,
}

impl Default for ConnectOauthConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: empty_secret(),
        }
    }
}

impl ConnectOauthConfig {
    pub fn enabled(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.expose_secret().trim().is_empty()
    }
}

/// `[telegram]`. Operator-owned central bot shared by every org: customers
/// link a chat by tapping a deep link instead of running their own BotFather
/// bot. A non-empty `bot_token` enables the whole surface — the connect
/// button, the `/hooks/telegram` receiver, and the boot webhook handshake.
/// Empty leaves it absent; the bring-your-own `telegram` channel is
/// unaffected either way. All three values are capabilities — env only,
/// never a config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TelegramBotConfig {
    #[serde(default = "empty_secret", with = "secret_str")]
    pub bot_token: SecretString,
    /// Verified against `getMe` at boot so a mismatched deep link can't be
    /// minted against the wrong bot.
    pub bot_username: String,
    /// Echoed back by Telegram in `X-Telegram-Bot-Api-Secret-Token` on every
    /// update; the only thing authenticating the receiver.
    #[serde(default = "empty_secret", with = "secret_str")]
    pub webhook_secret: SecretString,
}

impl Default for TelegramBotConfig {
    fn default() -> Self {
        Self {
            bot_token: empty_secret(),
            bot_username: String::new(),
            webhook_secret: empty_secret(),
        }
    }
}

impl TelegramBotConfig {
    pub fn enabled(&self) -> bool {
        !self.bot_token.expose_secret().trim().is_empty()
    }

    /// Bot token for linked-channel delivery; `None` when not configured.
    pub fn delivery_token(&self) -> Option<&str> {
        self.enabled().then(|| self.bot_token.expose_secret())
    }
}

/// Operator-owned WhatsApp business number (Meta Cloud API) behind the
/// one-tap `whatsapp_app` channels. `enabled` is a deliberate spend gate:
/// template sends ride the operator's WABA at per-message Meta pricing, so
/// creds alone never switch the surface on.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WhatsAppAppBotConfig {
    pub enabled: bool,
    #[serde(default = "empty_secret", with = "secret_str")]
    pub access_token: SecretString,
    /// Cloud API id messages are sent from.
    pub phone_number_id: String,
    /// Display number in international digits — the `wa.me` deep-link
    /// target (NOT the phone_number_id).
    pub public_number: String,
    /// Meta app secret; signs every webhook delivery
    /// (`X-Hub-Signature-256`).
    #[serde(default = "empty_secret", with = "secret_str")]
    pub app_secret: SecretString,
    /// Echoed by Meta's one-time GET subscribe handshake.
    #[serde(default = "empty_secret", with = "secret_str")]
    pub verify_token: SecretString,
    /// Approved alert template with a single body parameter.
    pub template_name: String,
    pub language_code: String,
}

impl Default for WhatsAppAppBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            access_token: empty_secret(),
            phone_number_id: String::new(),
            public_number: String::new(),
            app_secret: empty_secret(),
            verify_token: empty_secret(),
            template_name: String::new(),
            language_code: "en".into(),
        }
    }
}

impl WhatsAppAppBotConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
            && !self.access_token.expose_secret().trim().is_empty()
            && !self.phone_number_id.trim().is_empty()
            && !self.app_secret.expose_secret().trim().is_empty()
            && !self.verify_token.expose_secret().trim().is_empty()
    }
}

/// `[operator]`. Instance-admin surface (`/operator/*`) for managing regions
/// and agents across all tenants. Gated by a static bearer secret — env only,
/// never a config file. Empty `admin_token` disables the surface entirely.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OperatorConfig {
    #[serde(default = "empty_secret", with = "secret_str")]
    pub admin_token: SecretString,
    /// An agent with no successful pull/push for this long is reported stale
    /// (dead-man's-switch): a Prometheus gauge flips and the operator surface
    /// flags it. Default 3× the agent's default pull interval.
    #[serde(default = "default_agent_stale_after_secs")]
    pub agent_stale_after_secs: u64,
}

fn default_agent_stale_after_secs() -> u64 {
    90
}

impl Default for OperatorConfig {
    fn default() -> Self {
        Self {
            admin_token: empty_secret(),
            agent_stale_after_secs: default_agent_stale_after_secs(),
        }
    }
}

/// `[flow]`. Browser-driven flow monitors, off by default. Runs where `enabled`
/// is set and the Lightpanda engine is at `lightpanda_path`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FlowConfig {
    pub enabled: bool,
    pub lightpanda_path: String,
    pub max_concurrency: usize,
    /// Per-check browser RSS ceiling (MB); over it the run is killed as `Error`
    /// so one heavy page can't OOM the node. 0 = off.
    pub mem_limit_mb: u64,
    /// Runtime SSRF guard: block private/internal IPs after DNS resolution, which
    /// the save-time URL check can't (redirects/`fetch`/rebinding resolve later).
    pub block_private_networks: bool,
    /// Extra CIDRs to block, comma-separated (`-` exempts). Defaults add metadata,
    /// loopback, CGNAT, and IPv6 ULA (Fly 6PN = `fc00::/7`)/link-local.
    pub block_cidrs: String,
    /// In-engine V8 heap cap per browser (MB); 0 = engine default. A belt for the
    /// RSS watchdog — set below `mem_limit_mb` to trip on JS-heap runaway first.
    pub v8_max_heap_mb: u64,
    /// Reject any single browser response larger than this (MB); 0 = no limit.
    pub max_response_mb: u64,
    /// Appended to the browser User-Agent for attribution; empty = none.
    pub user_agent_suffix: String,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lightpanda_path: "lightpanda".into(),
            max_concurrency: 2,
            mem_limit_mb: 250,
            block_private_networks: true,
            block_cidrs: "169.254.0.0/16,127.0.0.0/8,100.64.0.0/10,::1/128,fc00::/7,fe80::/10"
                .into(),
            v8_max_heap_mb: 0,
            max_response_mb: 0,
            user_agent_suffix: String::new(),
        }
    }
}

/// `[agent]`. Turns this process into a stateless regional probe: it pulls its
/// region's monitor config from a control plane and ships results back, running
/// no web/Postgres/ClickHouse/alerting of its own. Off by default (the process
/// is a normal dashboard). `token` carries a capability — env only, never a
/// config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentConfig {
    pub enabled: bool,
    pub control_plane_url: String,
    #[serde(default = "empty_secret", with = "secret_str")]
    pub token: SecretString,
    pub region: String,
    pub pull_interval_secs: u64,
    pub flush_interval_secs: u64,
    pub buffer_capacity: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            control_plane_url: String::new(),
            token: empty_secret(),
            region: String::new(),
            pull_interval_secs: 30,
            flush_interval_secs: 5,
            buffer_capacity: 10_000,
        }
    }
}

/// `[escalation]`. Incident paging engine and its operator surfaces (escalation
/// policies, on-call schedules). Off by default: a single-responder deployment
/// gets direct alerting and the engine + its UI stay hidden. When `enabled`, an
/// open incident pages the monitor's bound notification channels and the legacy
/// direct alert dispatch is suppressed (the incident becomes the single source
/// of down/up notification), and the escalation + on-call UI is mounted. When
/// disabled, incidents still open and show in the console but page no one — the
/// legacy alert path keeps firing.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EscalationConfig {
    pub enabled: bool,
    /// Retry-sweep cadence: how often failed pages are re-attempted.
    pub tick_interval_secs: u64,
    /// Backpressure: max pages re-sent per sweep.
    pub max_pages_per_tick: u32,
    /// Give up paging a channel after this many failed attempts.
    pub max_attempts: u32,
    /// Base delay for the exponential retry backoff: attempt n waits
    /// `base * 2^(n-1)` (capped) before the next try.
    pub retry_backoff_base_secs: u64,
    /// Ceiling on a single retry's backoff delay.
    pub retry_backoff_cap_secs: u64,
    /// Floor on the reconcile scan: without it an incident nobody can be paged
    /// about is re-attempted every tick, forever.
    pub reconcile_window_secs: u64,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: 15,
            max_pages_per_tick: 500,
            max_attempts: 5,
            retry_backoff_base_secs: 30,
            retry_backoff_cap_secs: 3600,
            reconcile_window_secs: 3600,
        }
    }
}

/// `[mcp]`. Read-only Model Context Protocol server at `/mcp` (Streamable
/// HTTP). Disabled by default; enable per deployment once the dedicated `mcp.`
/// host + Caddy route exist. `allowed_origins` feeds the transport's RFC 6454
/// Origin check (DNS-rebinding defense): empty disables it, and a request with
/// no `Origin` header always passes — non-browser clients like `mcp-remote`
/// send none, browser connectors send their own origin.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    pub allowed_origins: Vec<String>,
    /// Canonical absolute URI of the MCP endpoint — the OAuth resource
    /// identifier (RFC 8707 audience) and the `resource` value in the RFC 9728
    /// protected-resource metadata. e.g. `https://mcp.uptimepage.dev/mcp`.
    /// Empty disables OAuth audience binding (static-token mode only).
    pub resource_uri: String,
    /// Enable the OAuth 2.1 authorization-server endpoints (`/oauth/*` +
    /// discovery metadata) that back the one-click connector. Requires
    /// `resource_uri` and `auth.public_base_url` to be real HTTPS origins.
    pub oauth_enabled: bool,
    /// Access-token lifetime in seconds (short; auto-renewed via the rotating
    /// refresh token). The *connection* lifetime is the user's consent choice,
    /// which governs the refresh token. Default 1 hour.
    pub access_token_ttl_secs: u32,
}

/// 1 hour — short access tokens, renewed by the refresh token.
fn default_access_token_ttl_secs() -> u32 {
    3600
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_origins: Vec::new(),
            resource_uri: String::new(),
            oauth_enabled: false,
            access_token_ttl_secs: default_access_token_ttl_secs(),
        }
    }
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
    /// Empty leaves the help form, its route and its nav entry absent, so a
    /// self-host install never mails a vendor.
    pub support_address: String,
    pub resend: ResendConfig,
}

impl TransactionalEmailConfig {
    /// Whether mail leaves the process. `log` renders to tracing and drops it.
    pub fn delivers(&self) -> bool {
        self.provider != "log"
    }

    pub fn support_enabled(&self) -> bool {
        !self.support_address.trim().is_empty()
    }
}

impl Default for TransactionalEmailConfig {
    fn default() -> Self {
        Self {
            provider: "log".into(),
            from_name: "Uptimepage".into(),
            from_address: "no-reply@example.invalid".into(),
            support_address: String::new(),
            resend: ResendConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ResendConfig {
    #[serde(default = "empty_secret", with = "secret_str")]
    pub api_key: SecretString,
    /// Svix signing secret (`whsec_…`) of the Resend webhook endpoint.
    /// Empty = the `/hooks/resend` receiver is absent and bounce events
    /// are not consumed.
    #[serde(default = "empty_secret", with = "secret_str")]
    pub webhook_secret: SecretString,
}

impl ResendConfig {
    pub fn webhook_enabled(&self) -> bool {
        !self.webhook_secret.expose_secret().trim().is_empty()
    }
}

impl Default for ResendConfig {
    fn default() -> Self {
        Self {
            api_key: empty_secret(),
            webhook_secret: empty_secret(),
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
    pub github: OauthClientConfig,
    pub google: OauthClientConfig,
    pub invitations: InvitationsConfig,
    pub api_tokens: ApiTokensConfig,
    pub magic_link: MagicLinkConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled_methods: vec![
                "github_oauth".into(),
                "google_oauth".into(),
                "magic_link".into(),
            ],
            fingerprint_salt: String::new(),
            public_base_url: "http://localhost:8080".into(),
            session: SessionConfig::default(),
            // Scopes empty: default.toml + provider DEFAULT_SCOPES own them.
            github: OauthClientConfig::default(),
            google: OauthClientConfig::default(),
            invitations: InvitationsConfig::default(),
            api_tokens: ApiTokensConfig::default(),
            magic_link: MagicLinkConfig::default(),
        }
    }
}

impl AuthConfig {
    /// List = policy switch; OAuth additionally needs creds (capability).
    pub fn method_enabled(&self, name: &str) -> bool {
        self.enabled_methods.iter().any(|m| m == name)
    }

    /// Single predicate for the magic-link surface — route mounting, the
    /// login-page form, and the token-purge ticker must agree.
    pub fn magic_link_enabled(&self) -> bool {
        self.method_enabled("magic_link")
    }

    pub fn github_login_enabled(&self) -> bool {
        self.method_enabled("github_oauth") && self.github.is_configured()
    }

    pub fn google_login_enabled(&self) -> bool {
        self.method_enabled("google_oauth") && self.google.is_configured()
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

/// One OAuth login provider's client credentials. A partially-written TOML
/// section resets `scopes` to `[]` via the nested `#[serde(default)]`; each
/// provider module falls back to its own default scopes for that case.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OauthClientConfig {
    pub client_id: String,
    #[serde(default = "empty_secret", with = "secret_str")]
    pub client_secret: SecretString,
    pub redirect_url: String,
    pub scopes: Vec<String>,
}

impl Default for OauthClientConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: empty_secret(),
            redirect_url: String::new(),
            scopes: Vec::new(),
        }
    }
}

impl OauthClientConfig {
    /// All three required — Google hard-rejects an empty redirect_uri.
    pub fn is_configured(&self) -> bool {
        !self.client_id.is_empty()
            && !self.client_secret.expose_secret().is_empty()
            && !self.redirect_url.is_empty()
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
    /// `mcp_audit` rows older than this are deleted. A row identifies what the
    /// tool acted on, which for a monitor means its name and address, so the
    /// trail gets a bounded life rather than outliving every other record.
    pub mcp_audit_days: u32,
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
            mcp_audit_days: 730,
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

/// `[quotas]`. Cache TTLs for plan/usage lookups, and the boot-seeded org's plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct QuotasConfig {
    /// Plans change rarely; a few minutes of staleness is acceptable.
    pub plan_cache_ttl_secs: u64,
    /// Usage counts move fast under bursty creates; short TTL only.
    pub usage_cache_ttl_secs: u64,
    /// Plan the boot-seeded owner org lands on. Only the unattended first-run
    /// path reads it, so hosted is untouched: its orgs come from signup and keep
    /// the schema's `free`. Defaults to `pro` because `free`'s ceilings bound
    /// what one tenant costs a shared platform, which nobody pays for on their
    /// own hardware.
    pub default_plan: String,
}

impl Default for QuotasConfig {
    fn default() -> Self {
        Self {
            plan_cache_ttl_secs: 300,
            usage_cache_ttl_secs: 10,
            default_plan: "pro".to_string(),
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
    /// TCP keep-alive for the in-flight connection. Checks connect fresh each
    /// run (no pool), so this only spans one request's body read.
    pub tcp_keepalive_secs: u64,
    /// Identifiable so site owners allowlist our probes instead of blocking them.
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}

fn default_user_agent() -> String {
    concat!(
        "uptimepage/",
        env!("CARGO_PKG_VERSION"),
        " (+https://uptimepage.dev/bot)"
    )
    .to_string()
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

    /// `None` means no KEK, so secrets are stored in plaintext. Every store
    /// must be handed the same one or a row sealed by one process fails to
    /// open in another.
    pub fn cipher(&self) -> Result<Option<std::sync::Arc<crate::security::Cipher>>> {
        match self.kek() {
            Some(kek) => Ok(Some(std::sync::Arc::new(
                crate::security::Cipher::from_base64(kek).map_err(|e| {
                    crate::error::AppError::Other(anyhow::anyhow!(
                        "invalid credentials_kek_base64: {e}"
                    ))
                })?,
            ))),
            None => Ok(None),
        }
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SchedulerConfig {
    /// Off = this process probes nothing in-process (pure dashboard/brain);
    /// agents do all probing. On = the in-process scheduler probes `region`.
    #[serde(default = "default_scheduler_enabled")]
    pub enabled: bool,
    pub target_refresh_interval_secs: u64,
    /// This control plane's own region id. Its scheduler runs the targets
    /// assigned to this region and stamps results with it — the same query an
    /// agent pulls for its region. Boot reconciles the row into `regions`.
    #[serde(default = "default_region_id")]
    pub region: String,
    /// Region assigned to newly-created targets. Empty falls back to `region`.
    #[serde(default)]
    pub default_region: String,
}

fn default_region_id() -> String {
    "default".to_string()
}

fn default_scheduler_enabled() -> bool {
    true
}

impl SchedulerConfig {
    /// Region new targets are assigned to: explicit `default_region`, else the
    /// control plane's own `region`.
    pub fn effective_default_region(&self) -> &str {
        if self.default_region.trim().is_empty() {
            &self.region
        } else {
            &self.default_region
        }
    }
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
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
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
/// `UPTIMEPAGE_OBSERVABILITY__GRAFANA__API_KEY`.
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

/// External dead-man's-switch heartbeat. The app pings `url` on an interval
/// *only while every critical dependency is reachable*; an independent
/// watcher (Healthchecks.io, Dead Man's Snitch, Grafana OnCall heartbeat)
/// alerts when the pings stop. This is the one signal that survives the whole
/// box dying — the in-app metrics/alert path can't page when it's the thing
/// that's down. `url` carries a capability token, so it is env-sourced
/// (`UPTIMEPAGE_OBSERVABILITY__HEARTBEAT__URL`) and never logged.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default = "default_heartbeat_interval_seconds")]
    pub interval_seconds: u64,
}

fn default_heartbeat_interval_seconds() -> u64 {
    60
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            interval_seconds: default_heartbeat_interval_seconds(),
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
        ge1_u64(
            self.scheduler.target_refresh_interval_secs,
            "scheduler.target_refresh_interval_secs",
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
        if self.escalation.reconcile_window_secs <= self.escalation.tick_interval_secs {
            return Err(crate::error::AppError::Other(anyhow::anyhow!(
                "escalation.reconcile_window_secs ({}) must exceed tick_interval_secs ({}) \
                 or the reconcile scan never matches",
                self.escalation.reconcile_window_secs,
                self.escalation.tick_interval_secs
            )));
        }
        if self.escalation.enabled {
            ge1_u64(
                self.escalation.tick_interval_secs,
                "escalation.tick_interval_secs",
            )?;
            if self.escalation.max_attempts < 1 {
                return Err(crate::error::AppError::Other(anyhow::anyhow!(
                    "escalation.max_attempts must be >= 1 (got {})",
                    self.escalation.max_attempts
                )));
            }
        }
        // Zero is "keep nothing", not "keep nothing older than the window".
        for (days, field) in [
            (
                self.retention.login_attempts_days,
                "retention.login_attempts_days",
            ),
            (
                self.retention.quota_events_days,
                "retention.quota_events_days",
            ),
            (self.retention.audit_log_days, "retention.audit_log_days"),
            (self.retention.mcp_audit_days, "retention.mcp_audit_days"),
            (
                self.auth.session.idle_timeout_days,
                "auth.session.idle_timeout_days",
            ),
            (
                self.tenancy.deletion_grace_period_days,
                "tenancy.deletion_grace_period_days",
            ),
        ] {
            ge1_u64(u64::from(days), field)?;
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
                    "UPTIMEPAGE_OBSERVABILITY__GRAFANA__API_KEY is required when tracing_enabled and grafana.enabled are true".into(),
                ));
            }
        }
        let hb = &self.observability.heartbeat;
        if hb.enabled {
            if hb.url.trim().is_empty() {
                return Err(err(
                    "UPTIMEPAGE_OBSERVABILITY__HEARTBEAT__URL is required when observability.heartbeat.enabled is true".into(),
                ));
            }
            if hb.interval_seconds == 0 {
                return Err(err(
                    "observability.heartbeat.interval_seconds must be > 0".into()
                ));
            }
        }
        Ok(())
    }

    /// Central-bot invariants, enforced only when `telegram.bot_token` is set.
    /// A misconfigured bot here is a clean startup error rather than a half-up
    /// feature that mints dead deep links.
    pub fn validate_telegram(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let t = &self.telegram;
        if !t.enabled() {
            return Ok(());
        }
        if t.bot_username.trim().is_empty() {
            return Err(err(
                "telegram.bot_username is required when telegram.bot_token is set",
            ));
        }
        if t.webhook_secret.expose_secret().trim().len() < 32 {
            return Err(err(
                "UPTIMEPAGE_TELEGRAM__WEBHOOK_SECRET must be at least 32 chars when telegram.bot_token is set",
            ));
        }
        let base = self.auth.public_base_url.trim();
        match url::Url::parse(base) {
            Ok(u) if u.scheme() == "https" && u.host_str().is_some() => {}
            _ => {
                return Err(err(
                    "auth.public_base_url must be an https:// URL with a host for the telegram webhook",
                ));
            }
        }
        Ok(())
    }

    /// A half-configured Resend sender is a clean startup error, not a
    /// per-send failure after cutover. The webhook secret alone is fine —
    /// the bounce receiver works regardless of the sending provider.
    pub fn validate_email(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let e = &self.email;
        if e.provider != "resend" {
            return Ok(());
        }
        if e.resend.api_key.expose_secret().trim().is_empty() {
            return Err(err(
                "email.resend.api_key is required when email.provider = \"resend\"",
            ));
        }
        if e.from_address.trim().is_empty() {
            return Err(err(
                "email.from_address is required when email.provider = \"resend\"",
            ));
        }
        Ok(())
    }

    /// A half-configured operator WhatsApp number is a clean startup error,
    /// not a dead webhook or a failing send after the flag flip.
    pub fn validate_whatsapp_app(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let w = &self.whatsapp_app;
        if !w.enabled {
            return Ok(());
        }
        if w.access_token.expose_secret().trim().is_empty()
            || w.phone_number_id.trim().is_empty()
            || w.app_secret.expose_secret().trim().is_empty()
            || w.verify_token.expose_secret().trim().is_empty()
        {
            return Err(err(
                "whatsapp_app.enabled needs access_token, phone_number_id, app_secret and verify_token set",
            ));
        }
        if w.verify_token.expose_secret().trim().len() < 32 {
            return Err(err(
                "UPTIMEPAGE_WHATSAPP_APP__VERIFY_TOKEN must be at least 32 chars",
            ));
        }
        let n = w.public_number.trim();
        if !(5..=20).contains(&n.len()) || !n.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err(
                "whatsapp_app.public_number must be the display number as international digits",
            ));
        }
        if w.template_name.is_empty()
            || !w
                .template_name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(err(
                "whatsapp_app.template_name is required (lowercase letters, digits, and _ only)",
            ));
        }
        if w.language_code.is_empty()
            || !w
                .language_code
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(err(
                "whatsapp_app.language_code must be a code like en or en_US",
            ));
        }
        // Deliberate: sends are operator-paid Meta template messages with no
        // per-org cap yet — the flag flip is the only spend control.
        tracing::warn!(
            "whatsapp_app.enabled — operator-paid template sends are UNCAPPED; \
             monitor spend until per-org send caps land"
        );
        Ok(())
    }

    /// Validate the regional-agent section. Only enforced when `agent.enabled`.
    pub fn validate_runtime(&self) -> Result<()> {
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        let agent = &self.agent;
        if !agent.enabled {
            return Ok(());
        }
        if agent.control_plane_url.trim().is_empty() {
            return Err(err(
                "agent.control_plane_url is required when agent.enabled",
            ));
        }
        // Resolved secrets and decrypted credentials ride the config-pull
        // response, so the control-plane transport must be encrypted. Cleartext
        // is permitted only when private targets are explicitly opted in (a
        // trusted private-network or localhost control plane for dev/integration).
        let url = url::Url::parse(agent.control_plane_url.trim())
            .map_err(|_| err("agent.control_plane_url is not a valid URL"))?;
        if url.scheme() != "https" && !self.security.allow_private_targets {
            return Err(err(
                "agent.control_plane_url must use https; cleartext is permitted \
                 only with security.allow_private_targets for a trusted \
                 private-network or localhost control plane",
            ));
        }
        if agent.region.trim().is_empty() {
            return Err(err("agent.region is required when agent.enabled"));
        }
        if agent.token.expose_secret().trim().is_empty() {
            return Err(err(
                "UPTIMEPAGE_AGENT__TOKEN is required when agent.enabled is true",
            ));
        }
        if agent.pull_interval_secs == 0 {
            return Err(err("agent.pull_interval_secs must be > 0"));
        }
        if agent.buffer_capacity == 0 {
            return Err(err("agent.buffer_capacity must be > 0"));
        }
        Ok(())
    }

    /// Reject the published `monitor` credentials; unoverridden they expose every tenant row.
    pub fn validate_storage(&self) -> Result<()> {
        const SHIPPED: &str = "monitor";
        const OPT_IN: &str = "set UPTIMEPAGE_STORAGE__ALLOW_DEFAULT_CREDENTIALS=true \
                              for a local stack";
        fn err(msg: &str) -> crate::error::AppError {
            crate::error::AppError::Other(anyhow::anyhow!(msg.to_string()))
        }
        if self.storage.allow_default_credentials {
            return Ok(());
        }
        let pg = url::Url::parse(&self.storage.postgres.url)
            .map_err(|_| err("storage.postgres.url is not a valid URL"))?;
        if pg.username() == SHIPPED && pg.password() == Some(SHIPPED) {
            return Err(err(&format!(
                "storage.postgres.url still carries the shipped credentials; \
                 set UPTIMEPAGE_STORAGE__POSTGRES__URL, or {OPT_IN}"
            )));
        }
        let ch = self.storage.clickhouse.password.expose_secret();
        if ch.is_empty() || ch == SHIPPED {
            return Err(err(&format!(
                "storage.clickhouse.password is empty or still the shipped value; \
                 set UPTIMEPAGE_STORAGE__CLICKHOUSE__PASSWORD, or {OPT_IN}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::FileFormat;

    /// Self-hosters get the roomier plan without configuring anything, so the
    /// default is the whole feature; a revert to `free` would be silent.
    #[test]
    fn boot_seeding_defaults_to_pro() {
        // The `Default` impl, not the merged config: a stray
        // UPTIMEPAGE_QUOTAS__DEFAULT_PLAN in the shell would answer for it.
        assert_eq!(QuotasConfig::default().default_plan, "pro");
    }

    /// An override naming one quota key must not blank the others.
    #[test]
    fn a_partial_quotas_table_keeps_the_other_defaults() {
        let partial: QuotasConfig = Config::builder()
            .add_source(File::from_str("default_plan = 'free'", FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(partial.default_plan, "free");
        assert_eq!(partial.plan_cache_ttl_secs, 300);
    }

    /// The bot page spells the version out by hand, so a stale sample teaches
    /// site owners to allowlist a string our probes no longer send.
    #[test]
    fn the_bot_page_quotes_the_user_agent_we_actually_send() {
        assert!(
            include_str!("../docs/legal/bot.md").contains(&default_user_agent()),
            "docs/legal/bot.md does not quote {:?}",
            default_user_agent()
        );
    }

    fn scheduler_from(toml: &str) -> SchedulerConfig {
        Config::builder()
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn scheduler_enabled_defaults_true_and_parses_false() {
        assert!(scheduler_from("target_refresh_interval_secs = 30").enabled);
        assert!(!scheduler_from("enabled = false\ntarget_refresh_interval_secs = 30").enabled);
    }

    #[test]
    fn help_form_stays_off_until_an_address_is_configured() {
        let mut cfg = TransactionalEmailConfig::default();
        assert!(!cfg.support_enabled(), "default ships with no address");
        cfg.support_address = "   ".into();
        assert!(!cfg.support_enabled(), "whitespace is not an address");
        cfg.support_address = "hello@example.test".into();
        assert!(cfg.support_enabled());
    }

    #[test]
    fn every_purge_window_the_nightly_job_binds_rejects_zero() {
        for set_zero in [
            (|c: &mut AppConfig| c.retention.login_attempts_days = 0) as fn(&mut AppConfig),
            |c: &mut AppConfig| c.retention.quota_events_days = 0,
            |c: &mut AppConfig| c.retention.audit_log_days = 0,
            |c: &mut AppConfig| c.retention.mcp_audit_days = 0,
            |c: &mut AppConfig| c.auth.session.idle_timeout_days = 0,
            |c: &mut AppConfig| c.tenancy.deletion_grace_period_days = 0,
        ] {
            let mut cfg = AppConfig::load().expect("load");
            assert!(
                cfg.validate_quotas_and_limits().is_ok(),
                "defaults are sane"
            );
            set_zero(&mut cfg);
            assert!(
                cfg.validate_quotas_and_limits().is_err(),
                "a zero purge window must not boot"
            );
        }
    }

    #[test]
    fn reconcile_window_must_outlast_a_tick_or_the_scan_never_matches() {
        let mut cfg = AppConfig::load().expect("load");
        cfg.escalation.tick_interval_secs = 15;
        cfg.escalation.reconcile_window_secs = 15;
        assert!(cfg.validate_quotas_and_limits().is_err());
        cfg.escalation.reconcile_window_secs = 16;
        assert!(cfg.validate_quotas_and_limits().is_ok());
    }

    fn agent_cfg(url: &str) -> AppConfig {
        let mut cfg = AppConfig::load().expect("load");
        cfg.agent.enabled = true;
        cfg.agent.control_plane_url = url.to_string();
        cfg.agent.region = "eu-helsinki".into();
        cfg.agent.token = SecretString::from("agent-token".to_string());
        cfg
    }

    #[test]
    fn agent_https_control_plane_passes() {
        agent_cfg("https://app.example.com")
            .validate_runtime()
            .expect("https passes");
    }

    #[test]
    fn agent_cleartext_control_plane_rejected() {
        let err = agent_cfg("http://app.example.com")
            .validate_runtime()
            .expect_err("cleartext rejected");
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[test]
    fn agent_cleartext_allowed_with_private_targets_optin() {
        let mut cfg = agent_cfg("http://127.0.0.1:8080");
        cfg.security.allow_private_targets = true;
        cfg.validate_runtime()
            .expect("cleartext ok when private opted in");
    }

    fn storage_cfg() -> AppConfig {
        let mut cfg = AppConfig::load().expect("load");
        cfg.storage.allow_default_credentials = false;
        cfg.storage.postgres.url = "postgres://monitor:monitor@localhost:5432/monitor".into();
        cfg.storage.clickhouse.password = SecretString::from("monitor".to_string());
        cfg
    }

    #[test]
    fn shipped_postgres_credentials_rejected() {
        let err = storage_cfg()
            .validate_storage()
            .expect_err("shipped pg credentials rejected");
        assert!(err.to_string().contains("storage.postgres.url"), "{err}");
    }

    #[test]
    fn shipped_clickhouse_password_rejected() {
        let mut cfg = storage_cfg();
        cfg.storage.postgres.url = "postgres://monitor:s3cret@localhost:5432/monitor".into();
        let err = cfg
            .validate_storage()
            .expect_err("shipped ch password rejected");
        assert!(
            err.to_string().contains("storage.clickhouse.password"),
            "{err}"
        );
    }

    #[test]
    fn empty_clickhouse_password_rejected() {
        let mut cfg = storage_cfg();
        cfg.storage.postgres.url = "postgres://monitor:s3cret@localhost:5432/monitor".into();
        cfg.storage.clickhouse.password = SecretString::from(String::new());
        cfg.validate_storage()
            .expect_err("empty ch password rejected");
    }

    #[test]
    fn overridden_credentials_pass() {
        let mut cfg = storage_cfg();
        cfg.storage.postgres.url = "postgres://monitor:s3cret@localhost:5432/monitor".into();
        cfg.storage.clickhouse.password = SecretString::from("s3cret".to_string());
        cfg.validate_storage().expect("overridden credentials pass");
    }

    #[test]
    fn opt_in_allows_shipped_credentials() {
        let mut cfg = storage_cfg();
        cfg.storage.allow_default_credentials = true;
        cfg.validate_storage().expect("opt-in bypasses the guard");
    }

    #[test]
    fn bootstrap_disabled_by_default_and_org_name_survives_a_partial_table() {
        let loaded = AppConfig::load().expect("load");
        assert!(loaded.bootstrap.email.is_empty());
        assert_eq!(loaded.bootstrap.org_name, "My Org");

        // An override file naming only `email` must not blank the org name.
        let partial: BootstrapConfig = Config::builder()
            .add_source(File::from_str("email = 'a@b.test'", FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(partial.org_name, "My Org");
    }
}
