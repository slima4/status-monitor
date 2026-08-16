//! Operator-facing subsystems: the console, browser flows, probe agents, escalation and MCP.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::{empty_secret, secret_str};

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
