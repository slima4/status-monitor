pub mod auth;
pub mod dashboard;
pub mod public_status;
pub mod targets_detail;
pub mod targets_form;
pub mod targets_list;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::domain::CheckSpec;

/// Maps a `CheckSpec` to a UI-friendly `(kind, address)` pair.
/// Used by the list and detail views; centralized so adding a new
/// check variant updates both call-sites.
pub(crate) fn describe_check(spec: &CheckSpec) -> (&'static str, String) {
    match spec {
        CheckSpec::Http(h) => ("HTTP", h.url.to_string()),
        CheckSpec::Tcp(c) => ("TCP", format!("{}:{}", c.host, c.port)),
        CheckSpec::TlsCert(c) => ("TLS", format!("{}:{}", c.host, c.port)),
        CheckSpec::DomainExpiry(c) => ("DOMAIN", c.domain.clone()),
    }
}

pub(crate) fn fmt_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Human-readable wall-clock UTC string, e.g. "2026-05-13 12:34 UTC".
/// Pair with `fmt_ts` (ISO 8601) for `<time datetime>` round-trips.
pub(crate) fn fmt_human(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}
