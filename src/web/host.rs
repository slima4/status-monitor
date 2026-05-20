//! Host-based org resolution for the public status surface.
//!
//! Two surfaces share one binary and one set of handlers:
//!  * path-based (self-host) — every request resolves to the default org;
//!  * subdomain (SaaS) — the org is taken from `{slug}.{base_domain}`.
//!
//! `extract_status_slug` is the pure host parser; [`StatusPageOrg`] is the
//! request extractor that picks the surface from config and yields the
//! `OrgId` the handler should serve. The host *determines the org*; the path
//! *determines the resource* (the JSON paths are identical across surfaces).

use axum::extract::{FromRef, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::header::HOST;
use axum::http::request::Parts;

use crate::api::public_error::PublicAppError;
use crate::api::subdomain_public_routes_enabled;
use crate::app::AppState;
use crate::domain::OrgId;

/// Parsed `{slug}.{base_domain}` host. Borrows the slug out of the `Host`
/// header so the common path allocates nothing.
#[derive(Debug, PartialEq, Eq)]
pub struct StatusPageHost<'a> {
    pub slug: &'a str,
}

/// Pull the org slug out of a status-page host, or `None` if `host` is not a
/// well-formed `{slug}.{base_domain}`.
///
/// `base_domain` must be non-empty and contain a dot. Without that guard an
/// empty/misconfigured base domain collapses the match suffix to a single
/// dot, and any host ending in `.` would parse as a valid slug. The boot-time
/// config assertion normally prevents an empty base domain reaching here;
/// this is the second layer of defence so a misconfigured dev/test rig can't
/// extract slugs from arbitrary hosts.
pub fn extract_status_slug<'a>(host: &'a str, base_domain: &str) -> Option<StatusPageHost<'a>> {
    if base_domain.is_empty() || !base_domain.contains('.') {
        return None;
    }
    // Equivalent to stripping `".{base_domain}"` but without the per-request
    // `format!` allocation — this runs on every anonymous subdomain request.
    let slug = host.strip_suffix(base_domain)?.strip_suffix('.')?;
    // Reject the bare `{base_domain}` (empty slug) and any deeper subdomain
    // (a remaining dot means `a.b.{base_domain}`).
    if slug.is_empty() || slug.contains('.') {
        return None;
    }
    Some(StatusPageHost { slug })
}

/// The org a public-status request should be served from. Construct only via
/// the extractor — never by hand — so the host→org provenance stays at the
/// type level.
///
/// Resolution is surface-aware:
///  * subdomain surface live → parse the `Host` header and resolve the slug
///    through [`find_public_status_org_by_slug`], which filters
///    `public_status_enabled = true`. A missing/garbled host or a
///    not-opted-in org is a 404 — the public surface never confirms which
///    orgs exist.
///  * otherwise (path-based / self-host) → the boot-time default org.
///
/// [`find_public_status_org_by_slug`]: crate::storage::orgs::find_public_status_org_by_slug
#[derive(Debug, Clone, Copy)]
pub struct StatusPageOrg(pub OrgId);

/// Shared resolver behind the [`StatusPageOrg`] extractor. Exposed so the
/// server-rendered handlers can map a failure to the styled HTML error page
/// instead of the JSON envelope an extractor rejection would produce.
pub async fn resolve_status_page_org(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<OrgId, PublicAppError> {
    if !subdomain_public_routes_enabled(&state.cfg) {
        return Ok(state.default_org_id);
    }

    let host = headers
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .ok_or(PublicAppError::NotFound)?;
    // Strip the port; `extract_status_slug` matches host names only.
    let host = host.split(':').next().unwrap_or(host);

    let parsed = extract_status_slug(host, &state.cfg.public_status.base_domain)
        .ok_or(PublicAppError::NotFound)?;

    let pool = state.db.as_ref().ok_or_else(|| {
        PublicAppError::Internal(anyhow::anyhow!(
            "subdomain public routes enabled but no database handle"
        ))
    })?;

    // One indexed point lookup (partial index on slug WHERE
    // public_status_enabled) per subdomain request. Intentional: the host
    // determines the org, so resolution can't be hoisted out of the request.
    // Self-host short-circuits above and never pays this. A slug→org cache is
    // a deliberate non-goal here — the downstream page cache absorbs the
    // expensive aggregation; this is a cheap keyed read.
    //
    // Public-status-specific lookup: filters `public_status_enabled = true`
    // and returns the `PublicStatusOrg` newtype the operator path can't
    // accept. Reusing the authenticated `find_id_by_slug` here would serve
    // every org's public page regardless of opt-in.
    let org = crate::storage::orgs::find_public_status_org_by_slug(pool, parsed.slug)
        .await?
        .ok_or(PublicAppError::NotFound)?;
    Ok(org.0.id)
}

impl<S> FromRequestParts<S> for StatusPageOrg
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = PublicAppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let org = resolve_status_page_org(&app_state, &parts.headers).await?;
        Ok(StatusPageOrg(org))
    }
}

/// True when the SaaS subdomain surface is live AND the request's `Host`
/// parses as a `{slug}.{base_domain}` (so an opted-in org *might* answer).
/// The `/` dispatcher uses this to choose between the operator dashboard
/// and the per-org public page; the actual org lookup still happens through
/// [`StatusPageOrg`], which 404s when the slug is unknown or opted out.
pub fn is_subdomain_public_request(state: &AppState, headers: &HeaderMap) -> bool {
    if !subdomain_public_routes_enabled(&state.cfg) {
        return false;
    }
    let Some(host) = headers.get(HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let host = host.split(':').next().unwrap_or(host);
    extract_status_slug(host, &state.cfg.public_status.base_domain).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_slug_from_well_formed_host() {
        assert_eq!(
            extract_status_slug("acme.example.com", "example.com"),
            Some(StatusPageHost { slug: "acme" })
        );
    }

    #[test]
    fn rejects_bare_base_domain() {
        // The base domain itself (no slug label) must not parse as a slug.
        assert_eq!(extract_status_slug("example.com", "example.com"), None);
    }

    #[test]
    fn rejects_empty_slug() {
        assert_eq!(extract_status_slug(".example.com", "example.com"), None);
    }

    #[test]
    fn rejects_non_matching_host() {
        assert_eq!(extract_status_slug("acme.other.com", "example.com"), None);
    }

    #[test]
    fn rejects_deeper_subdomain() {
        assert_eq!(extract_status_slug("a.b.example.com", "example.com"), None);
    }

    #[test]
    fn empty_base_domain_matches_nothing() {
        // An empty base domain must not collapse the suffix into a wildcard
        // that accepts attacker-supplied hosts.
        assert_eq!(extract_status_slug("foo.", ""), None);
        assert_eq!(extract_status_slug("evil.", ""), None);
    }

    #[test]
    fn single_label_base_domain_rejected() {
        // `local` has no dot — refuse so `foo.local` can't resolve in a
        // misconfigured dev rig.
        assert_eq!(extract_status_slug("foo.local", "local"), None);
    }

    #[test]
    fn caller_must_strip_port_before_calling() {
        // The extractor strips the port; the pure parser does not, so a
        // host carrying `:443` is (correctly) not a match here.
        assert_eq!(
            extract_status_slug("acme.example.com:443", "example.com"),
            None
        );
    }
}
