pub mod auth;
pub mod dashboard;
pub mod legal;
pub mod notification_channels;
pub mod public_status;
pub mod targets_detail;
pub mod targets_form;
pub mod targets_list;

use axum::response::{IntoResponse, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::domain::{CheckSpec, OrgId};
use crate::error::AppError;
use crate::web::CurrentOrg;
use crate::web::error::WebError;

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
