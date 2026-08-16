//! Sign-in, sessions, invitations, API tokens and first-run seeding.

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::{empty_secret, secret_str};

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
