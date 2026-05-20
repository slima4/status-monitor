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

/// Subdomain labels that route to the operator surface (dashboard + auth +
/// API) instead of the per-org public page. These are NOT the
/// signup-collision list (`crate::domain::reserved_slugs`) — that list is
/// much broader (~75 entries) and exists to keep an org from claiming a
/// slug that *could* collide with any operator URL or namespace. Mixing
/// the two here would route every reserved label (e.g. `acme`,
/// `hetzner`, `gdpr`) to the operator login page, multiplying the
/// operator surface across dozens of hosts and leaking the reserved list
/// via response codes. Keep this set minimal — only labels actually
/// served by the operator front door.
const OPERATOR_LABELS: &[&str] = &["app"];

/// Marketing labels — apex (empty) and `www` route to the marketing site,
/// not to a tenant. Kept tight (only `www`); deeper aliases would
/// multiply marketing's surface and shadow tenant slugs.
const MARKETING_LABELS: &[&str] = &["www"];

fn is_operator_label(slug: &str) -> bool {
    OPERATOR_LABELS.iter().any(|l| slug.eq_ignore_ascii_case(l))
}

/// One source of truth for the production wire format. Constructed once
/// at boot from `public_status.base_domain` and shared between
/// [`extract_status_slug`] and [`classify_host`] so they cannot drift.
/// Apex shape is flat (`{slug}.{base_domain}`) — there is no `.status.`
/// infix; the FLATTEN-STATUS-HOST change already removed it.
#[derive(Debug, Clone)]
pub struct HostScheme {
    pub base_domain: String,
    pub apex: String,
    pub www_host: String,
    pub app_host: String,
    /// `.{base_domain}` — what every tenant host ends with. Carries the
    /// leading dot so a `strip_suffix` accepts only `{slug}.{base}`, not
    /// the bare base.
    pub tenant_suffix: String,
}

impl HostScheme {
    /// Validates the base domain (non-empty, contains a dot) and derives
    /// the apex / `www` / `app` / tenant-suffix triplet. Returns a
    /// human-readable error suitable for a startup config error.
    pub fn from_base_domain(base: &str) -> std::result::Result<Self, String> {
        let trimmed = base.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            return Err("base_domain must not be empty".into());
        }
        if !trimmed.contains('.') {
            return Err(format!("base_domain {trimmed:?} must contain a dot"));
        }
        Ok(Self {
            apex: trimmed.clone(),
            www_host: format!("www.{trimmed}"),
            app_host: format!("app.{trimmed}"),
            tenant_suffix: format!(".{trimmed}"),
            base_domain: trimmed,
        })
    }
}

/// What kind of host arrived. The marketing dispatch seam routes
/// `Marketing` (and `Unknown`, so garbage cannot fall through to a
/// tenant) to the marketing router; `App` and `TenantPublic` go to the
/// existing app router which already does per-host org resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    Marketing,
    App,
    TenantPublic,
    Unknown,
}

/// Classify a request's `Host` header against the production wire
/// format. The caller strips the port; this is host-only.
pub fn classify_host(host: &str, scheme: &HostScheme) -> HostClass {
    let host = host.split(':').next().unwrap_or(host);
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if host == scheme.apex {
        return HostClass::Marketing;
    }
    if host == scheme.app_host {
        return HostClass::App;
    }
    if let Some(slug) = host.strip_suffix(&scheme.tenant_suffix) {
        if slug.is_empty() || slug.contains('.') {
            return HostClass::Unknown;
        }
        if MARKETING_LABELS.iter().any(|l| slug == *l) {
            return HostClass::Marketing;
        }
        if is_operator_label(slug) {
            return HostClass::App;
        }
        return HostClass::TenantPublic;
    }
    HostClass::Unknown
}

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
    // FQDN form: RFC 1034 allows a trailing dot (`acme.example.com.`).
    // Browsers strip it before display but a crafted curl/script can send
    // it. Without this normalisation, the suffix match would fail and the
    // request would fall through to the operator dashboard — exposing the
    // operator surface on what looks like a tenant host.
    let host = host.strip_suffix('.').unwrap_or(host);
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
/// parses as a non-operator `{slug}.{base_domain}`. The `/` dispatcher
/// uses this to choose between the operator dashboard and the per-org
/// public page. Only the operator label set ([`OPERATOR_LABELS`]) falls
/// through to the dashboard; any other label — including signup-reserved
/// ones like `acme` or `hetzner` — keeps the public dispatcher, which
/// then 404s through [`StatusPageOrg`] when no opted-in org owns that
/// slug. Routing the broader reserved list here would multiply the
/// operator surface (login, API, settings) across dozens of hosts and
/// leak the reserved set via response-code fingerprints.
pub fn is_subdomain_public_request(state: &AppState, headers: &HeaderMap) -> bool {
    if !subdomain_public_routes_enabled(&state.cfg) {
        return false;
    }
    let Some(host) = headers.get(HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Ok(scheme) = HostScheme::from_base_domain(&state.cfg.public_status.base_domain) else {
        return false;
    };
    matches!(classify_host(host, &scheme), HostClass::TenantPublic)
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

    #[test]
    fn accepts_fqdn_trailing_dot() {
        // RFC 1034 FQDN form; a crafted client can send the trailing dot.
        // Without normalisation the suffix match misses and the request
        // would otherwise fall through to the operator dashboard.
        assert_eq!(
            extract_status_slug("acme.example.com.", "example.com"),
            Some(StatusPageHost { slug: "acme" })
        );
    }

    #[test]
    fn fqdn_bare_base_still_rejected() {
        // Trailing dot on the base domain alone — must NOT collapse into a
        // valid (empty-slug) parse.
        assert_eq!(extract_status_slug("example.com.", "example.com"), None);
    }

    fn scheme() -> HostScheme {
        HostScheme::from_base_domain("example.com").unwrap()
    }

    #[test]
    fn classify_apex_is_marketing() {
        assert_eq!(
            classify_host("example.com", &scheme()),
            HostClass::Marketing
        );
    }

    #[test]
    fn classify_www_is_marketing() {
        assert_eq!(
            classify_host("www.example.com", &scheme()),
            HostClass::Marketing
        );
    }

    #[test]
    fn classify_app_is_app() {
        assert_eq!(classify_host("app.example.com", &scheme()), HostClass::App);
    }

    #[test]
    fn classify_tenant_slug_is_tenant_public() {
        assert_eq!(
            classify_host("acme.example.com", &scheme()),
            HostClass::TenantPublic
        );
    }

    #[test]
    fn classify_strips_port() {
        assert_eq!(
            classify_host("example.com:8080", &scheme()),
            HostClass::Marketing
        );
        assert_eq!(
            classify_host("acme.example.com:443", &scheme()),
            HostClass::TenantPublic
        );
    }

    #[test]
    fn classify_strips_trailing_dot() {
        // FQDN form: a crafted curl can send `Host: acme.example.com.`.
        // Must classify identically — otherwise the dispatcher would
        // 404 a real tenant.
        assert_eq!(
            classify_host("acme.example.com.", &scheme()),
            HostClass::TenantPublic
        );
        assert_eq!(classify_host("app.example.com.", &scheme()), HostClass::App);
    }

    #[test]
    fn classify_is_case_insensitive() {
        assert_eq!(
            classify_host("ACME.Example.COM", &scheme()),
            HostClass::TenantPublic
        );
    }

    #[test]
    fn classify_deeper_subdomain_is_unknown() {
        // `a.b.example.com` must NOT alias a tenant slug; the dispatcher
        // hands `Unknown` to marketing 404, never to a tenant.
        assert_eq!(
            classify_host("a.b.example.com", &scheme()),
            HostClass::Unknown
        );
    }

    #[test]
    fn classify_unrelated_host_is_unknown() {
        assert_eq!(
            classify_host("acme.other.com", &scheme()),
            HostClass::Unknown
        );
        assert_eq!(classify_host("garbage", &scheme()), HostClass::Unknown);
    }

    #[test]
    fn classify_empty_host_is_unknown() {
        assert_eq!(classify_host("", &scheme()), HostClass::Unknown);
    }

    #[test]
    fn host_scheme_rejects_empty_or_single_label() {
        assert!(HostScheme::from_base_domain("").is_err());
        assert!(HostScheme::from_base_domain("   ").is_err());
        assert!(HostScheme::from_base_domain("local").is_err());
    }
}
