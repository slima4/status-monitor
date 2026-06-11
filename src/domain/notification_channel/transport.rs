//! The per-transport contract. Every delivery destination implements this
//! over its own config struct; [`super::ChannelConfig`] only delegates, so a
//! new transport is a new module here plus one enum variant — the compiler
//! walks you through the rest.

use super::ChannelKind;

pub const MASK: &str = "***";

pub trait TransportConfig {
    const KIND: ChannelKind;

    fn kind(&self) -> ChannelKind {
        Self::KIND
    }

    /// Overwrite every secret-bearing field with the `***` mask in place.
    /// Non-secret routing shape (header *names*, chat id) is kept so the UI
    /// can still show which channel this is.
    fn redact_in_place(&mut self);

    /// True if any secret-bearing field still carries the redaction
    /// sentinel. A `GET → PATCH` round-trip that re-submits a masked config
    /// must be rejected, never written back as the literal `***`.
    fn has_redaction_sentinel(&self) -> bool;

    /// Cheap structural validation (no network). Returns a human message on
    /// the first problem. Reachability / SSRF checks belong to the notifier
    /// transport, not here.
    fn validate(&self) -> Result<(), String>;

    /// The customer-controlled destination URL to run through the abuse
    /// deny-list. `None` means deliveries go to a fixed vendor endpoint and
    /// there is nothing to inspect. No default on purpose: skipping the
    /// abuse gate must be an explicit per-transport decision, not an
    /// omission.
    fn abuse_url(&self) -> Option<&str>;

    /// True when only the operator's own flow may produce this config (a
    /// caller-supplied destination would ride the operator's credentials).
    /// No default on purpose, like [`Self::abuse_url`].
    fn operator_managed(&self) -> bool;
}

/// `https://`-only URL rule shared by the URL-bearing transports.
pub(super) fn require_https(u: &str, field: &str) -> Result<(), String> {
    let parsed = url::Url::parse(u).map_err(|_| format!("{field} is not a valid URL"))?;
    if parsed.scheme() != "https" {
        return Err(format!("{field} must be an https:// URL"));
    }
    Ok(())
}
