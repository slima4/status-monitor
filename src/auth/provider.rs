//! OAuth providers wired through the auth flow. The variant set is the
//! single source of truth: the Postgres CHECK on `oauth_identities.provider`
//! is the closed list of [`OauthProvider::ALL`]; `oauth_states.provider`
//! additionally accepts [`CONNECT_PROVIDERS`]. Both validated by
//! `tests/enum_drift_test.rs`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OauthProvider {
    Github,
    Google,
}

impl OauthProvider {
    /// Every variant in declaration order. Used by the enum-drift integration
    /// test to compare against the live Postgres CHECK constraint; keep in
    /// lockstep with the enum body.
    pub const ALL: &'static [Self] = &[Self::Github, Self::Google];

    /// Stable string used in the Postgres CHECK constraints and bound as the
    /// `provider` parameter at every INSERT / WHERE site. Routing through this
    /// method means adding a new provider requires updating the enum, which
    /// the drift test then ties to a matching migration.
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
        }
    }
}

/// Connect-purpose OAuth dances (channel attach, not login): allowed in
/// `oauth_states.provider` on top of [`OauthProvider::ALL`], never in
/// `oauth_identities.provider` — they produce no identity row.
pub const SLACK_CONNECT_PROVIDER: &str = "slack_connect";

pub const CONNECT_PROVIDERS: &[&str] = &[SLACK_CONNECT_PROVIDER];
