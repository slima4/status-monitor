pub mod auth;
pub mod dashboard;
pub mod escalation;
pub mod incidents;
pub mod legal;
pub mod notification_channels;
pub mod on_call;
pub mod pages;
pub mod public_status;
pub mod region_display;
pub mod share;
pub mod targets_detail;
pub mod targets_form;
pub mod targets_list;

use std::fmt;

use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde::Serialize;

use crate::domain::{CheckSpec, OrgId};
use crate::error::AppError;
use crate::web::CurrentOrg;
use crate::web::error::WebError;

/// Shared range tab descriptor — the per-page handler builds a `Vec`
/// from its allowed key set, marking exactly one entry `selected`. One
/// source so the Console / Detail / Incidents tabs render identical
/// markup and the active tab can never silently double-fire.
pub struct RangeOption {
    pub key: &'static str,
    pub selected: bool,
}

/// Page-size option in a list footer. `hx_get` switches the link from a
/// full navigation to an htmx swap of the list region.
pub struct PageSizeLink {
    pub n: usize,
    pub href: String,
    pub hx_get: Option<String>,
    pub active: bool,
}

/// Prev/next link in a list footer.
pub struct PagerLink {
    pub label: &'static str,
    pub href: String,
    pub hx_get: Option<String>,
}

pub(crate) fn build_range_options(active: &'static str, keys: &[&'static str]) -> Vec<RangeOption> {
    keys.iter()
        .map(|k| RangeOption {
            key: k,
            selected: *k == active,
        })
        .collect()
}

/// Returns the matching key from `keys` if `raw` is one of them, else
/// `default`. Tiny but used by every page that exposes a `?range=` tab
/// strip — centralised so adding a new preset is one edit.
pub(crate) fn resolve_range_key(
    raw: Option<&str>,
    keys: &[&'static str],
    default: &'static str,
) -> &'static str {
    raw.and_then(|s| keys.iter().copied().find(|k| *k == s))
        .unwrap_or(default)
}

/// Resolve the caller's tenant for a `/settings/*` page exactly as the API
/// does. An *unauthenticated* hit bounces to login (so a bookmarked settings
/// URL works after sign-in); a Forbidden / DB error surfaces as the HTML
/// error page, never a misleading login loop. Shared by every settings view.
pub(crate) fn resolve_org(
    org: Result<CurrentOrg, AppError>,
    redirect_to: &str,
) -> Result<OrgId, Box<Response>> {
    match org {
        Ok(CurrentOrg(o)) => Ok(o),
        Err(AppError::Unauthorized) => Err(Box::new(
            crate::web::auth::login_redirect(redirect_to).into_response(),
        )),
        Err(e) => Err(Box::new(WebError::from(e).into_response())),
    }
}

/// Pretty-print a string map for a "headers (JSON object)" form field,
/// falling back to an empty object so the textarea is never blank/invalid.
pub(crate) fn json_pretty<T: Serialize>(m: &T) -> String {
    serde_json::to_string_pretty(m).unwrap_or_else(|_| "{}".into())
}

/// Maps a `CheckSpec` to a UI-friendly `(kind, address)` pair.
/// Used by the list and detail views; centralized so adding a new
/// check variant updates both call-sites.
pub(crate) fn describe_check(spec: &CheckSpec) -> (&'static str, String) {
    match spec {
        CheckSpec::Http(h) => ("HTTP", h.url.to_string()),
        CheckSpec::Tcp(c) => ("TCP", format!("{}:{}", c.host, c.port)),
        CheckSpec::TlsCert(c) => ("TLS", format!("{}:{}", c.host, c.port)),
        CheckSpec::DomainExpiry(c) => ("DOMAIN", c.domain.clone()),
        CheckSpec::Dns(c) => ("DNS", format!("{} {}", c.record_type.as_str(), c.domain)),
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

/// Exact single-unit duration (`45s`, `5m`, `24h`) for config values that
/// must round-trip — the lossy two-unit display lives in [`HumanDur`].
pub(crate) fn exact_duration(secs: u64) -> String {
    if secs % 3_600 == 0 {
        format!("{}h", secs / 3_600)
    } else if secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Two-unit duration string, e.g. `"45s"`, `"17m"`, `"2h 14m"`, `"1d 1h"`.
/// Negative durations clamp to zero.
pub(crate) fn humanize_duration(d: ChronoDuration) -> String {
    HumanDur(d.num_seconds()).to_string()
}

/// Display wrapper for [`humanize_duration`] that writes directly to a
/// `fmt::Formatter` instead of allocating an intermediate `String`. Cheap
/// to construct from the raw seconds the storage layer already returns.
pub struct HumanDur(pub i64);

impl fmt::Display for HumanDur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.0.max(0);
        if total < 60 {
            return write!(f, "{total}s");
        }
        let mins = total / 60;
        if mins < 60 {
            return write!(f, "{mins}m");
        }
        let hours = mins / 60;
        let rem_mins = mins % 60;
        if hours < 24 {
            if rem_mins == 0 {
                return write!(f, "{hours}h");
            }
            return write!(f, "{hours}h {rem_mins}m");
        }
        let days = hours / 24;
        let rem_hours = hours % 24;
        if rem_hours == 0 {
            write!(f, "{days}d")
        } else {
            write!(f, "{days}d {rem_hours}h")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_duration_picks_largest_unit() {
        assert_eq!(humanize_duration(ChronoDuration::seconds(0)), "0s");
        assert_eq!(humanize_duration(ChronoDuration::seconds(45)), "45s");
        assert_eq!(humanize_duration(ChronoDuration::minutes(17)), "17m");
        assert_eq!(humanize_duration(ChronoDuration::minutes(134)), "2h 14m");
        assert_eq!(humanize_duration(ChronoDuration::hours(25)), "1d 1h");
        assert_eq!(humanize_duration(ChronoDuration::hours(48)), "2d");
        assert_eq!(humanize_duration(ChronoDuration::seconds(-5)), "0s");
    }
}
