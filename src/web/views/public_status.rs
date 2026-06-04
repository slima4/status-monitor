//! Server-rendered public status page.
//!
//! Reads from the same `PublicSource` (and therefore the same in-process
//! cache) as the JSON endpoint, so a JSON and HTML request landing in the
//! same 10s window share one aggregator run.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::public_error::PublicAppError;
use crate::app::AppState;
use crate::config::PublicStatusConfig;
use crate::domain::AssetSlot;
use crate::domain::elapsed_at;
use crate::domain::{
    DayState, IncidentSeverity, IncidentStatusPhase, OrgId, OverallState, PublicComponent,
    PublicComponentGroup, PublicComponentStatus, PublicIncident, PublicIncidentUpdate,
    PublicMaintenance, PublicOrgBranding, PublicStatusPage, StatusPageId,
};
use crate::public_status::HistoryIncidentMarker;
use crate::storage::orgs::{OrgBranding, load_page_branding};
use crate::web::error::{NotFoundPage, UnavailablePage};
use crate::web::filters;
use crate::web::host::{HostShape, parse_host_shape, resolve_status_page};
use crate::web::views::humanize_duration;

#[derive(Debug, Default, Deserialize)]
pub struct StatusParams {
    /// HTMX partial swap — return just the refresh region.
    pub fragment: Option<u8>,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/status.html")]
pub struct StatusFullPage {
    pub view: StatusView,
    pub branding: BrandingView,
    pub og: OgMeta,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/region.html")]
pub struct StatusRegion {
    pub view: StatusView,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/incident.html")]
pub struct IncidentDetailPage {
    pub branding: BrandingView,
    pub incident: IncidentDetailView,
    pub generated_at: DateTime<Utc>,
    pub rss_url: &'static str,
    pub og: OgMeta,
}

#[derive(Template, WebTemplate)]
#[template(path = "public/archive.html")]
pub struct IncidentArchivePage {
    pub branding: BrandingView,
    /// Incidents bucketed by `(year, month-name)` in DESC chronological
    /// order. The template iterates each month as a section so the user
    /// scans by date without explicit date-pickers — UX matches the
    /// Atlassian / Statuspage.io archive convention.
    pub months: Vec<MonthBucket>,
    /// Opaque keyset cursor for the *next* page of older incidents; `None`
    /// when this is the last page. The "Older incidents →" link only
    /// renders when set.
    pub next_cursor: Option<String>,
    pub rss_url: &'static str,
    pub og: OgMeta,
}

/// OG/Twitter card metadata for the public status surface. Empty `url` /
/// `image` mean "skip that tag" — fine for self-hosted setups where the
/// marketing origin isn't configured.
#[derive(Default)]
pub struct OgMeta {
    pub title: String,
    pub description: String,
    pub og_type: &'static str,
    pub url: String,
    pub image: String,
}

pub struct MonthBucket {
    /// Localised-ish label like "May 2026". Already date-formatted, so the
    /// template renders it verbatim with no extra filters.
    pub label: String,
    pub incidents: Vec<IncidentSummary>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ArchiveParams {
    /// Keyset cursor returned by the previous archive page's next link.
    /// Same opaque-token shape used by `/api/public/v1/incidents`.
    pub cursor: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<StatusParams>,
) -> Response {
    let page_ref = match resolve_status_page(&state, &headers).await {
        Ok(p) => p,
        Err(err) => return render_public_error(err),
    };
    let (page, markers) = match state.public_source.page_with_markers(page_ref).await {
        Ok(pair) => pair,
        Err(err) => return render_public_error(err),
    };
    let view = build_view(&page, &markers);
    if params.fragment.unwrap_or(0) != 0 {
        // Chrome-free auto-refresh fragment: no header/footer/style, so the
        // branding lookup is skipped on the 30s poll.
        StatusRegion { view }.into_response()
    } else {
        let branding = resolve_branding(&state, page_ref.org, page_ref.page, &page.site_name).await;
        let og = build_og_meta(
            &state,
            &headers,
            "/status",
            format!("{} Status", branding.display_name),
            format!(
                "Live operational status for {}. Current uptime, recent incidents, scheduled maintenance.",
                branding.display_name
            ),
            "website",
        );
        StatusFullPage { view, branding, og }.into_response()
    }
}

/// Serves the page's uploaded logo (or 404 when none is set). Same host→page
/// resolution as the page itself; the query string is a cache-buster only,
/// never a selector — the bytes come from the page's `logo` asset row.
pub async fn logo(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let page_ref = match resolve_status_page(&state, &headers).await {
        Ok(p) => p,
        Err(err) => return render_public_error(err),
    };
    match state
        .page_asset_store
        .get(page_ref.page, AssetSlot::Logo)
        .await
    {
        Ok(Some(asset)) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=3600, immutable".to_owned(),
                ),
                // User-uploaded bytes served on the same origin as the app
                // (path-based public routing). nosniff stops MIME-guessing
                // past the forced image type; inline keeps it a passive asset.
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                (header::CONTENT_DISPOSITION, "inline".to_owned()),
            ],
            asset.bytes,
        )
            .into_response(),
        Ok(None) => render_public_error(PublicAppError::NotFound),
        Err(e) => render_public_error(PublicAppError::from(e)),
    }
}

pub async fn incident(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    let page_ref = match resolve_status_page(&state, &headers).await {
        Ok(p) => p,
        Err(err) => return render_public_error(err),
    };
    let (inc_res, page_res) = tokio::join!(
        state.public_source.incident_by_id(page_ref, id),
        state.public_source.page(page_ref),
    );
    let inc = match inc_res {
        Ok(i) => i,
        Err(err) => return render_public_error(err),
    };
    let fallback_name = match page_res {
        Ok(p) => p.site_name.clone(),
        Err(err) => return render_public_error(err),
    };
    let branding = resolve_branding(&state, page_ref.org, page_ref.page, &fallback_name).await;
    let now = Utc::now();
    let og = build_og_meta(
        &state,
        &headers,
        &format!("/status/incidents/{id}"),
        format!("{} · {} Status", inc.title, branding.display_name),
        format!("Incident report for {}: {}.", inc.component_name, inc.title),
        "article",
    );
    IncidentDetailPage {
        branding,
        incident: IncidentDetailView::from_incident(&inc, now),
        generated_at: now,
        rss_url: RSS_URL,
        og,
    }
    .into_response()
}

/// Cursor-paginated archive view of every public incident for the org.
/// Groups visually by month in DESC order so the user scans the page like
/// a calendar without an explicit date picker. The link from the main
/// status page (`/status`) lands here without a cursor; the "Older
/// incidents →" link at the bottom passes `?cursor=…` to walk backwards.
pub async fn archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ArchiveParams>,
) -> Response {
    use crate::api::cursor::IncidentCursor;
    use crate::public_status::IncidentListQuery;

    let page_ref = match resolve_status_page(&state, &headers).await {
        Ok(p) => p,
        Err(err) => return render_public_error(err),
    };
    let cursor = match params.cursor.as_deref().map(IncidentCursor::decode) {
        Some(Ok(c)) => Some(c),
        Some(Err(_)) => return render_public_error(PublicAppError::BadRequest("invalid cursor")),
        None => None,
    };
    let query = IncidentListQuery {
        limit: ARCHIVE_PAGE_SIZE,
        cursor,
        ongoing_only: false,
    };
    let (page_res, list_res) = tokio::join!(
        state.public_source.page(page_ref),
        state.public_source.list_incidents(page_ref, query),
    );
    let fallback_name = match page_res {
        Ok(p) => p.site_name.clone(),
        Err(err) => return render_public_error(err),
    };
    let listing = match list_res {
        Ok(l) => l,
        Err(err) => return render_public_error(err),
    };
    let branding = resolve_branding(&state, page_ref.org, page_ref.page, &fallback_name).await;
    let now = Utc::now();
    let months = bucket_by_month(&listing.items, now);
    let og = build_og_meta(
        &state,
        &headers,
        "/status/incidents",
        format!("Incident history · {} Status", branding.display_name),
        format!("Historical incidents for {}.", branding.display_name),
        "website",
    );
    IncidentArchivePage {
        branding,
        months,
        next_cursor: listing.next_cursor,
        rss_url: RSS_URL,
        og,
    }
    .into_response()
}

/// Group sorted-DESC incidents into per-month buckets. Sort order is
/// preserved within and across buckets because the caller hands us rows
/// already ordered by `(started_at DESC, id DESC)` via the keyset query.
fn bucket_by_month(items: &[PublicIncident], now: DateTime<Utc>) -> Vec<MonthBucket> {
    let mut out: Vec<MonthBucket> = Vec::new();
    for incident in items {
        // `%B %Y` → "May 2026". chrono's locale is C, which is exactly what
        // we want for a stable status-page header; no manual month-name table.
        let label = incident.started_at.format("%B %Y").to_string();
        match out.last_mut() {
            Some(bucket) if bucket.label == label => {
                bucket.incidents.push(build_incident_summary(incident, now));
            }
            _ => {
                out.push(MonthBucket {
                    label,
                    incidents: vec![build_incident_summary(incident, now)],
                });
            }
        }
    }
    out
}

/// Maps a `PublicAppError` to an HTML response for the rendered routes —
/// avoids leaking the JSON envelope into the browser.
fn render_public_error(err: PublicAppError) -> Response {
    match err {
        PublicAppError::NotFound => {
            (StatusCode::NOT_FOUND, NotFoundPage { active_tab: "" }).into_response()
        }
        PublicAppError::InvalidDays | PublicAppError::BadRequest(_) => {
            (StatusCode::BAD_REQUEST, NotFoundPage { active_tab: "" }).into_response()
        }
        PublicAppError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            UnavailablePage { active_tab: "" },
        )
            .into_response(),
        PublicAppError::Internal(e) => {
            tracing::error!(error = %e, "public status page internal error");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                UnavailablePage { active_tab: "" },
            )
                .into_response()
        }
    }
}

// --- View model ----------------------------------------------------------

const RSS_URL: &str = "/api/public/v1/incidents.rss";
const HISTORY_LEN: usize = 90;
/// Default page size for the archive view. Small enough that each render is
/// snappy on the unauthenticated, edge-cached path; the keyset cursor walks
/// older pages on demand.
const ARCHIVE_PAGE_SIZE: u32 = 25;
/// Single source of truth for the logo path — referenced by both the route
/// registration and the URL the template emits so they cannot drift.
pub const LOGO_ROUTE: &str = "/status/branding/logo";

/// Origin a page is served from: an absolute host in subdomain mode, an empty
/// string (same operator host) in path mode, or `None` when no public surface
/// is mounted. Single source for both the API view and the settings editor so
/// their URLs can't diverge.
pub fn public_base(cfg: &crate::config::AppConfig, slug: &str) -> Option<String> {
    use crate::api::routes::{path_based_public_routes_enabled, subdomain_public_routes_enabled};
    if subdomain_public_routes_enabled(cfg) {
        return Some(format!("https://{slug}.{}", cfg.public_status.base_domain));
    }
    if path_based_public_routes_enabled(cfg) {
        return Some(String::new());
    }
    None
}

/// Public page URL from an origin: the apex in subdomain mode, `{origin}/status`
/// in path mode.
pub fn public_status_url(cfg: &crate::config::AppConfig, origin: &str) -> String {
    use crate::api::routes::subdomain_public_routes_enabled;
    if subdomain_public_routes_enabled(cfg) {
        origin.to_owned()
    } else {
        format!("{origin}/status")
    }
}

/// Logo URL stamped with the asset's content hash (cache-buster), or `None`
/// when no public surface is mounted.
pub fn public_logo_url(base: Option<&str>, hash: &str) -> Option<String> {
    base.map(|origin| format!("{origin}{LOGO_ROUTE}?v={hash}"))
}

/// `.{base_domain}` slug-preview suffix in subdomain mode; `None` in path mode.
pub fn public_host_suffix(cfg: &crate::config::AppConfig) -> Option<String> {
    use crate::api::routes::subdomain_public_routes_enabled;
    subdomain_public_routes_enabled(cfg)
        .then(|| format!(".{}", cfg.public_status.base_domain.trim_start_matches('.')))
}

pub struct StatusView {
    pub site_name: String,
    pub site_title: String,
    pub overall_label: String,
    pub overall_class: &'static str,
    pub overall_icon: &'static str,
    pub overall_aria: &'static str,
    pub generated_at: DateTime<Utc>,
    pub groups: Vec<GroupView>,
    pub active_incidents: Vec<IncidentSummary>,
    pub recent_incidents: Vec<IncidentSummary>,
    /// True when the org has more incidents past the rendered window. Drives
    /// the "older incidents" archive link in the recent-incidents section.
    pub recent_incidents_has_more: bool,
    pub active_maintenance: Vec<MaintenanceView>,
    pub upcoming_maintenance: Vec<MaintenanceView>,
    pub has_active_incident: bool,
    pub has_maintenance: bool,
    pub has_components: bool,
    pub rss_url: &'static str,
    pub meta_description: String,
    /// Inlined at `#day-strip-data`; consumed by day_popover.js.
    pub day_strip_json: String,
}

pub struct GroupView {
    pub heading: String,
    pub components: Vec<ComponentView>,
}

pub struct ComponentView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status_label: &'static str,
    pub status_class: &'static str,
    pub status_icon: &'static str,
    pub history: Vec<DayCell>,
    pub uptime_pct: String,
    pub history_summary: String,
}

pub struct DayCell {
    pub class: &'static str,
    pub aria_label: String,
    /// Index into the day_strip_json blob for this component.
    pub day_index: usize,
}

#[derive(serde::Serialize)]
struct DayStripComponent {
    name: String,
    days: Vec<DayPopoverEntry>,
}

#[derive(serde::Serialize)]
struct DayPopoverEntry {
    date: String,
    state: &'static str,
    state_class: &'static str,
    show_badge: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    downtime: Option<String>,
    related: Vec<DayRelated>,
}

#[derive(serde::Serialize)]
struct DayRelated {
    title: String,
    url: String,
}

/// Common header fields shared by the recent-incidents list and the detail page.
pub struct IncidentHeader {
    pub id: String,
    pub component_name: String,
    pub title: String,
    pub severity_label: &'static str,
    pub severity_class: &'static str,
    pub phase_label: &'static str,
    pub phase_class: &'static str,
    pub started_at: DateTime<Utc>,
    pub ongoing: bool,
    /// Elapsed seconds at page-build `now`. Populated for both closed and
    /// ongoing incidents — the public view shows a duration for all rows.
    pub duration_secs: i64,
}

pub struct IncidentSummary {
    pub header: IncidentHeader,
    pub ended_at: Option<DateTime<Utc>>,
    pub latest_message: Option<String>,
    pub permalink: String,
}

pub struct IncidentDetailView {
    pub header: IncidentHeader,
    pub ended_at: Option<DateTime<Utc>>,
    pub updates: Vec<IncidentUpdateView>,
    pub postmortem: Option<crate::domain::PublicPostmortem>,
}

pub struct IncidentUpdateView {
    pub posted_at: DateTime<Utc>,
    pub phase_label: &'static str,
    pub phase_class: &'static str,
    pub message: String,
}

pub struct MaintenanceView {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub affects: String,
    /// Seconds until `starts_at` for upcoming windows; `None` once started.
    pub starts_in_secs: Option<i64>,
}

/// Operator-controlled branding, resolved for rendering. Optional DB fields
/// have already had their defaults applied here, so the template just prints
/// these values — no fallback logic in the template layer.
pub struct BrandingView {
    pub display_name: String,
    /// Pre-sanitised HTML (markdown → ammonia allow-list). Rendered with the
    /// `safe` filter; it is the *only* unescaped value on the page.
    pub about_html: Option<String>,
    /// Already passed through [`safe_brand_color`]; safe to interpolate into
    /// the `:root` `<style>` block verbatim.
    pub brand_color: String,
    pub brand_text: &'static str,
    pub logo_url: Option<String>,
    pub show_powered_by: bool,
    pub style: &'static str,
}

impl BrandingView {
    fn from_org(o: &OrgBranding, cfg: &PublicStatusConfig) -> Self {
        let display_name = o.resolved_display_name().to_owned();
        let about_html = o
            .branding
            .public_about
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(render_about);
        let brand_color = safe_brand_color(
            o.branding.public_brand_color.as_deref(),
            &cfg.default_brand_color,
        );
        let brand_text = safe_brand_text_for(&brand_color);
        BrandingView {
            display_name,
            about_html,
            brand_color,
            brand_text,
            logo_url: o
                .branding
                .logo_hash
                .as_deref()
                .map(|h| format!("{LOGO_ROUTE}?v={h}")),
            show_powered_by: o.branding.show_powered_by(cfg.default_show_powered_by),
            style: o.branding.public_style.as_str(),
        }
    }
}

/// Loads the org's branding for a rendered page. A missing row, missing DB
/// handle, or query error degrades to defaults keyed off `fallback_name` —
/// the status page must still render if branding can't be read.
async fn resolve_branding(
    state: &AppState,
    org: OrgId,
    page: StatusPageId,
    fallback_name: &str,
) -> BrandingView {
    let cfg = &state.cfg.public_status;
    let mut view = if let Some(pool) = state.db.as_ref()
        && let Ok(Some(ob)) = load_page_branding(pool, page).await
    {
        BrandingView::from_org(&ob, cfg)
    } else {
        BrandingView::from_org(
            &OrgBranding {
                name: fallback_name.to_owned(),
                slug: String::new(),
                branding: PublicOrgBranding::default(),
            },
            cfg,
        )
    };
    // On a plan-lookup fault, fail closed (badge shown) but log it — otherwise a
    // DB blip silently strips a paying Pro page's white-label with no signal.
    let white_label = match state.quotas.limit_for_org(org).await {
        Ok(p) => p.white_label_enabled,
        Err(e) => {
            tracing::warn!(error = %e, org = %org.0, "white-label gate: plan lookup failed; showing badge");
            false
        }
    };
    view.show_powered_by = enforce_powered_by(
        view.show_powered_by,
        state.cfg.marketing.enabled,
        white_label,
    );
    view
}

/// White-label is Pro-only: on SaaS a plan without white-label always shows the
/// "powered by" badge, whatever the page stored. Self-host (marketing off) and
/// white-label plans keep the stored preference.
fn enforce_powered_by(stored: bool, saas: bool, white_label: bool) -> bool {
    if saas && !white_label { true } else { stored }
}

/// Independent, template-side re-validation of the brand colour. Trusts
/// neither the DB CHECK constraint nor the app-level validator: it owns its
/// own `^#[0-9a-fA-F]{6}$` predicate and returns `default` on any mismatch, so
/// the value interpolated into the `<style>` block can never break out of the
/// CSS rule even if an upstream layer's predicate is later widened.
pub fn safe_brand_color(raw: Option<&str>, default: &str) -> String {
    fn is_strict_hex(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
    }
    match raw {
        Some(c) if is_strict_hex(c) => c.to_owned(),
        _ => default.to_owned(),
    }
}

/// Pick whichever of `#ffffff` or `#0f172a` gives the higher WCAG contrast
/// ratio against the brand. Threshold-by-luminance breaks for mid-tone
/// brands like Twitter blue or Slack yellow (white passes AA-Large but fails
/// AA on small text).
pub fn safe_brand_text_for(brand_hex: &str) -> &'static str {
    const WHITE: &str = "#ffffff";
    const DARK: &str = "#0f172a";
    const L_WHITE: f32 = 1.0;
    // Pre-computed relative luminance of #0f172a.
    const L_DARK: f32 = 0.0145;

    fn srgb_to_linear(c: u8) -> f32 {
        let s = f32::from(c) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    let b = brand_hex.as_bytes();
    if b.len() != 7 || b[0] != b'#' {
        return WHITE;
    }
    let Ok(rgb) = u32::from_str_radix(brand_hex.trim_start_matches('#'), 16) else {
        return WHITE;
    };
    let r = srgb_to_linear(((rgb >> 16) & 0xff) as u8);
    let g = srgb_to_linear(((rgb >> 8) & 0xff) as u8);
    let bl = srgb_to_linear((rgb & 0xff) as u8);
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * bl;
    let contrast_white = (L_WHITE + 0.05) / (lum + 0.05);
    let contrast_dark = (lum + 0.05) / (L_DARK + 0.05);
    if contrast_dark > contrast_white {
        DARK
    } else {
        WHITE
    }
}

/// Builds OG/Twitter metadata for the public surface. `og:url` is emitted
/// only when the request Host validates as the apex or a tenant subdomain
/// of `public_status.base_domain` — without that gate, an attacker hitting
/// the page with `Host: evil.com` would poison the scraper cache so social
/// shares of legitimate URLs unfurl with attacker's domain. `og:image`
/// degrades to empty (template skips the tag) when the marketing origin
/// isn't configured.
fn build_og_meta(
    state: &AppState,
    headers: &HeaderMap,
    path: &str,
    title: String,
    description: String,
    og_type: &'static str,
) -> OgMeta {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|s| matches!(*s, "http" | "https"))
        .unwrap_or("https");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let url = match parse_host_shape(host, &state.cfg.public_status.base_domain) {
        HostShape::Apex | HostShape::Subdomain(_) => {
            // Strip port + trailing dot the same way `parse_host_shape` does
            // so the URL canonicalises to RFC-1034 form.
            let canonical = host
                .split(':')
                .next()
                .unwrap_or(host)
                .strip_suffix('.')
                .unwrap_or_else(|| host.split(':').next().unwrap_or(host));
            format!("{scheme}://{canonical}{path}")
        }
        HostShape::Other => String::new(),
    };
    let origin = &state.cfg.marketing.canonical_origin;
    let image = if origin.is_empty() {
        String::new()
    } else {
        format!("{origin}/static/marketing/og.png")
    };
    OgMeta {
        title,
        description,
        og_type,
        url,
        image,
    }
}

/// Allow-list sanitiser for `public_about`. Built once: the tag set and
/// builder are immutable, and `clean` takes `&self`, so there's no reason to
/// reconstruct it per render.
static ABOUT_SANITIZER: std::sync::LazyLock<ammonia::Builder<'static>> =
    std::sync::LazyLock::new(|| {
        let mut b = ammonia::Builder::default();
        b.tags(
            ["p", "strong", "em", "a", "br", "ul", "ol", "li"]
                .into_iter()
                .collect(),
        )
        .link_rel(Some("noopener nofollow"));
        b
    });

/// Renders `public_about` markdown to HTML, then strips everything outside a
/// small allow-list. The parser output is never trusted: ammonia is the
/// boundary, not pulldown-cmark.
pub fn render_about(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    ABOUT_SANITIZER.clean(&html).to_string()
}

// --- Builders ------------------------------------------------------------

fn build_view(page: &PublicStatusPage, history_markers: &[HistoryIncidentMarker]) -> StatusView {
    let now = page.generated_at;

    let groups = page.groups.iter().map(build_group).collect::<Vec<_>>();
    let has_components = groups.iter().any(|g| !g.components.is_empty());

    let day_strip_json = build_day_strip_json(page, history_markers, now);

    let active = page
        .active_incidents
        .iter()
        .map(|i| build_incident_summary(i, now))
        .collect::<Vec<_>>();
    let recent = page
        .recent_incidents
        .iter()
        .map(|i| build_incident_summary(i, now))
        .collect::<Vec<_>>();

    let active_m = page
        .active_maintenance
        .iter()
        .map(|m| build_maintenance(m, now))
        .collect::<Vec<_>>();
    let upcoming_m = page
        .upcoming_maintenance
        .iter()
        .map(|m| build_maintenance(m, now))
        .collect::<Vec<_>>();

    let (overall_class, overall_icon, overall_aria) = overall_classes(page.overall.state);
    let site_title = format!("{} Status", page.site_name);
    let meta_description = format!("Live operational status for {}", page.site_name);

    StatusView {
        site_name: page.site_name.clone(),
        site_title,
        overall_label: page.overall.label.clone(),
        overall_class,
        overall_icon,
        overall_aria,
        generated_at: now,
        groups,
        has_active_incident: !active.is_empty(),
        active_incidents: active,
        recent_incidents: recent,
        recent_incidents_has_more: page.recent_incidents_has_more,
        has_maintenance: !active_m.is_empty() || !upcoming_m.is_empty(),
        active_maintenance: active_m,
        upcoming_maintenance: upcoming_m,
        has_components,
        rss_url: RSS_URL,
        meta_description,
        day_strip_json,
    }
}

fn build_group(g: &PublicComponentGroup) -> GroupView {
    GroupView {
        heading: g.name.clone().unwrap_or_else(|| "Other".to_string()),
        components: g.components.iter().map(build_component).collect(),
    }
}

fn build_component(c: &PublicComponent) -> ComponentView {
    let (status_label, status_class, status_icon) = component_classes(c.current_status);
    let history = build_history(c, &c.history);
    let (uptime_pct, summary) = history_stats(&c.history);
    ComponentView {
        id: c.id.to_string(),
        name: c.name.clone(),
        description: c.description.clone().filter(|s| !s.is_empty()),
        status_label,
        status_class,
        status_icon,
        history,
        uptime_pct,
        history_summary: summary,
    }
}

fn build_history(component: &PublicComponent, states: &[DayState]) -> Vec<DayCell> {
    let total = states.len().max(1);
    states
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let (class, label, _tint) = day_classes(*s);
            let day_index = idx;
            let days_ago = total - 1 - idx;
            DayCell {
                class,
                day_index,
                aria_label: format!("{} ({} days ago) — {}", component.name, days_ago, label),
            }
        })
        .collect()
}

/// Build the inline popover blob. UTC day boundary matches the
/// aggregator's `toStartOfDay`, so cell colour and popover state align.
fn build_day_strip_json(
    page: &PublicStatusPage,
    history_markers: &[HistoryIncidentMarker],
    now: DateTime<Utc>,
) -> String {
    let today_start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|nd| DateTime::<Utc>::from_naive_utc_and_offset(nd, Utc))
        .unwrap_or(now);
    // Bucket markers once so the day loop scans only the component's own
    // incidents instead of every org-wide marker × every day.
    let mut by_comp: std::collections::HashMap<Uuid, Vec<&HistoryIncidentMarker>> =
        std::collections::HashMap::new();
    for m in history_markers {
        by_comp.entry(m.component_id).or_default().push(m);
    }
    let empty: Vec<&HistoryIncidentMarker> = Vec::new();
    let mut blob: std::collections::BTreeMap<String, DayStripComponent> =
        std::collections::BTreeMap::new();
    for group in &page.groups {
        for c in &group.components {
            let total = c.history.len().max(1);
            let markers = by_comp.get(&c.id).unwrap_or(&empty);
            let days = c
                .history
                .iter()
                .enumerate()
                .map(|(idx, s)| {
                    let days_ago = total - 1 - idx;
                    let day_start = today_start - ChronoDuration::days(days_ago as i64);
                    let day_end = day_start + ChronoDuration::days(1);
                    let (downtime, related) = day_overlap(markers, day_start, day_end, now);
                    let (_class, state, state_class) = day_classes(*s);
                    let show_badge = !matches!(s, DayState::Operational | DayState::NoData)
                        || !related.is_empty();
                    DayPopoverEntry {
                        date: day_start.format("%-d %b %Y").to_string(),
                        state,
                        state_class,
                        show_badge,
                        downtime: (downtime > ChronoDuration::zero())
                            .then(|| humanize_duration(downtime)),
                        related,
                    }
                })
                .collect();
            blob.insert(
                c.id.to_string(),
                DayStripComponent {
                    name: c.name.clone(),
                    days,
                },
            );
        }
    }
    // Escape every `<`, `>`, `&` to JSON `\uXXXX` so a malicious incident
    // title can't terminate the inline <script> with `</script>`, slip into
    // a comment via `<!--`, or close the JSON early via `&`/CDATA tricks.
    // Browsers parse `<` etc. back to the original char on JSON.parse.
    serde_json::to_string(&blob)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// Sum incident time overlapping `[day_start, day_end)`. Caller pre-filters
/// to one component's markers. Open-ended incidents clamp to `now`.
fn day_overlap(
    incidents: &[&HistoryIncidentMarker],
    day_start: DateTime<Utc>,
    day_end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> (ChronoDuration, Vec<DayRelated>) {
    let mut total = ChronoDuration::zero();
    let mut links = Vec::new();
    for inc in incidents {
        let end = inc.ended_at.unwrap_or(now);
        if inc.started_at >= day_end || end <= day_start {
            continue;
        }
        let overlap_start = inc.started_at.max(day_start);
        let overlap_end = end.min(day_end);
        let overlap = (overlap_end - overlap_start).max(ChronoDuration::zero());
        total += overlap;
        links.push(DayRelated {
            title: inc.title.clone(),
            url: format!("/status/incidents/{}", inc.id),
        });
    }
    (total, links)
}

fn history_stats(states: &[DayState]) -> (String, String) {
    let mut with_data = 0usize;
    let mut bad = 0usize;
    let mut degraded = 0usize;
    for s in states {
        match s {
            DayState::NoData => {}
            DayState::MajorOutage | DayState::PartialOutage => {
                with_data += 1;
                bad += 1;
            }
            DayState::Degraded => {
                with_data += 1;
                degraded += 1;
            }
            DayState::Operational | DayState::Maintenance => with_data += 1,
        }
    }
    if with_data == 0 {
        return ("—".to_string(), format!("{HISTORY_LEN} days, no data"));
    }
    let pct = 100.0 - (bad as f64 / with_data as f64) * 100.0;
    let summary = if bad == 0 && degraded == 0 {
        format!("{with_data} days, no incidents")
    } else if bad == 0 {
        format!("{with_data} days, {degraded} degraded")
    } else {
        format!(
            "{with_data} days, {bad} outage{}, {degraded} degraded",
            if bad == 1 { "" } else { "s" },
        )
    };
    (format!("{pct:.2}"), summary)
}

fn build_incident_header(i: &PublicIncident, now: DateTime<Utc>) -> IncidentHeader {
    let ongoing = i.ended_at.is_none();
    let duration_secs = elapsed_at(i.started_at, i.ended_at, now).num_seconds();
    let (severity_label, severity_class) = severity_classes(i.severity);
    let (phase_label, phase_class) = phase_classes(i.status_phase);
    IncidentHeader {
        id: i.id.to_string(),
        component_name: i.component_name.clone(),
        title: i.title.clone(),
        severity_label,
        severity_class,
        phase_label,
        phase_class,
        started_at: i.started_at,
        ongoing,
        duration_secs,
    }
}

fn build_incident_summary(i: &PublicIncident, now: DateTime<Utc>) -> IncidentSummary {
    let header = build_incident_header(i, now);
    let latest_message = i.updates.last().map(|u: &PublicIncidentUpdate| {
        if u.message.len() > 240 {
            format!("{}…", &u.message[..240])
        } else {
            u.message.clone()
        }
    });
    IncidentSummary {
        permalink: format!("/status/incidents/{}", header.id),
        header,
        ended_at: i.ended_at,
        latest_message,
    }
}

impl IncidentDetailView {
    fn from_incident(i: &PublicIncident, now: DateTime<Utc>) -> Self {
        let header = build_incident_header(i, now);
        let updates = i
            .updates
            .iter()
            .map(|u| {
                let (l, c) = phase_classes(u.phase);
                IncidentUpdateView {
                    posted_at: u.posted_at,
                    phase_label: l,
                    phase_class: c,
                    message: u.message.clone(),
                }
            })
            .collect();
        Self {
            header,
            ended_at: i.ended_at,
            updates,
            postmortem: i.postmortem.clone(),
        }
    }
}

fn build_maintenance(m: &PublicMaintenance, now: DateTime<Utc>) -> MaintenanceView {
    let starts_in_secs = (m.starts_at > now).then(|| (m.starts_at - now).num_seconds());
    MaintenanceView {
        id: m.id.to_string(),
        title: m.title.clone(),
        description: m.description.clone().filter(|s| !s.is_empty()),
        starts_at: m.starts_at,
        ends_at: m.ends_at,
        affects: m.affected_component_names.join(", "),
        starts_in_secs,
    }
}

// --- Classifiers ---------------------------------------------------------

// Iconography follows the design system: solid filled circle (U+25CF) for
// go/no-go states (operational, major outage), gear (U+2699) for maintenance,
// warning sign (U+26A0) for degraded / partial outage. No emoji — every glyph
// here is a non-emoji Unicode symbol that inherits text colour via currentColor.
fn overall_classes(s: OverallState) -> (&'static str, &'static str, &'static str) {
    match s {
        OverallState::Operational => ("public-overall--op", "\u{25CF}", "All systems operational"),
        OverallState::Maintenance => (
            "public-overall--mnt",
            "\u{2699}\u{FE0E}",
            "Maintenance in progress",
        ),
        OverallState::MinorDisruption => (
            "public-overall--minor",
            "\u{26A0}\u{FE0E}",
            "Minor service disruption",
        ),
        OverallState::PartialOutage => (
            "public-overall--part",
            "\u{26A0}\u{FE0E}",
            "Partial system outage",
        ),
        OverallState::MajorOutage => ("public-overall--maj", "\u{25CF}", "Major system outage"),
    }
}

fn component_classes(s: PublicComponentStatus) -> (&'static str, &'static str, &'static str) {
    match s {
        PublicComponentStatus::Operational => ("Operational", "public-cmp--op", "\u{25CF}"),
        PublicComponentStatus::Degraded => ("Degraded", "public-cmp--deg", "\u{26A0}\u{FE0E}"),
        PublicComponentStatus::PartialOutage => {
            ("Partial outage", "public-cmp--part", "\u{26A0}\u{FE0E}")
        }
        PublicComponentStatus::MajorOutage => ("Major outage", "public-cmp--maj", "\u{25CF}"),
        PublicComponentStatus::Maintenance => {
            ("Maintenance", "public-cmp--mnt", "\u{2699}\u{FE0E}")
        }
    }
}

/// (bar fill class, human label, popover badge tint class).
fn day_classes(s: DayState) -> (&'static str, &'static str, &'static str) {
    match s {
        DayState::Operational => ("day-cell--op", "Operational", "day-pop-status--op"),
        DayState::Degraded => ("day-cell--deg", "Degraded", "day-pop-status--deg"),
        DayState::PartialOutage => ("day-cell--part", "Partial outage", "day-pop-status--part"),
        DayState::MajorOutage => ("day-cell--maj", "Major outage", "day-pop-status--maj"),
        DayState::Maintenance => ("day-cell--mnt", "Maintenance", "day-pop-status--mnt"),
        DayState::NoData => ("day-cell--none", "No data", "day-pop-status--none"),
    }
}

fn severity_classes(s: IncidentSeverity) -> (&'static str, &'static str) {
    match s {
        IncidentSeverity::Minor => ("Minor", "public-chip public-sev--minor"),
        IncidentSeverity::Major => ("Major", "public-chip public-sev--major"),
        IncidentSeverity::Critical => ("Critical", "public-chip public-sev--critical"),
    }
}

fn phase_classes(p: IncidentStatusPhase) -> (&'static str, &'static str) {
    match p {
        IncidentStatusPhase::Investigating => {
            ("Investigating", "public-chip public-phase--investigating")
        }
        IncidentStatusPhase::Identified => ("Identified", "public-chip public-phase--identified"),
        IncidentStatusPhase::Monitoring => ("Monitoring", "public-chip public-phase--monitoring"),
        IncidentStatusPhase::Resolved => ("Resolved", "public-chip public-phase--resolved"),
        IncidentStatusPhase::Postmortem => ("Postmortem", "public-chip public-phase--postmortem"),
    }
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::OverallStatus;

    #[test]
    fn powered_by_forced_for_saas_non_white_label() {
        // SaaS free: badge shown even if the page tried to hide it.
        assert!(enforce_powered_by(false, true, false));
        assert!(enforce_powered_by(true, true, false));
        // SaaS white-label (Pro): stored preference honoured.
        assert!(!enforce_powered_by(false, true, true));
        // Self-host: unrestricted, stored preference honoured.
        assert!(!enforce_powered_by(false, false, false));
    }

    fn sample_page() -> PublicStatusPage {
        PublicStatusPage {
            overall: OverallStatus {
                state: OverallState::Operational,
                label: "All Systems Operational".into(),
            },
            generated_at: Utc::now(),
            site_name: "Acme".into(),
            groups: vec![PublicComponentGroup {
                name: Some("API".into()),
                components: vec![PublicComponent {
                    id: Uuid::nil(),
                    name: "Gateway".into(),
                    description: Some("Customer-facing edge".into()),
                    current_status: PublicComponentStatus::Operational,
                    history: vec![DayState::Operational; HISTORY_LEN],
                }],
            }],
            active_incidents: vec![],
            recent_incidents: vec![],
            recent_incidents_has_more: false,
            active_maintenance: vec![],
            upcoming_maintenance: vec![],
        }
    }

    fn sample_branding() -> BrandingView {
        BrandingView::from_org(
            &OrgBranding {
                name: "Acme".into(),
                slug: "acme".into(),
                branding: PublicOrgBranding::default(),
            },
            &PublicStatusConfig::default(),
        )
    }

    #[test]
    fn full_page_renders_chrome_and_components() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Acme Status"));
        assert!(html.contains("All Systems Operational"));
        assert!(html.contains("Gateway"));
        assert!(html.contains(r#"hx-get="/status?fragment=1""#));
        assert!(html.contains(r#"hx-trigger="every 30s""#));
        assert!(html.contains("data-tz"));
        assert!(html.contains("/static/js/htmx.min.js"));
        assert!(html.contains("/static/js/ui/localtime.js"));
        assert!(html.contains("/static/js/public/day_popover.js"));
        assert!(html.contains("/api/public/v1/incidents.rss"));
    }

    #[test]
    fn day_strip_renders_trigger_buttons_and_blob() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusRegion { view }.render().unwrap();
        assert!(html.contains("data-day-trigger"));
        assert!(html.contains(r#"data-comp="#));
        assert!(html.contains(r#"data-day="#));
        // Shared popover + content template + JSON blob — each appears once.
        assert_eq!(html.matches(r#"id="day-popover""#).count(), 1);
        assert_eq!(html.matches(r#"id="day-popover-related-tpl""#).count(), 1);
        assert_eq!(html.matches(r#"id="day-strip-data""#).count(), 1);
        // Roving tabindex: one `tabindex="0"` per component strip; the rest
        // are `tabindex="-1"`. Sample has 1 component × 90 days = 1 stop.
        assert_eq!(html.matches(r#"tabindex="0""#).count(), 1);
        assert_eq!(html.matches(r#"tabindex="-1""#).count(), 89);
    }

    #[test]
    fn day_strip_blob_links_overlapping_incident() {
        let mut p = sample_page();
        // Replace `today` with a bad day, then add a marker for a 40m
        // incident on the matching component.
        let comp_id = p.groups[0].components[0].id;
        let last = p.groups[0].components[0].history.len() - 1;
        p.groups[0].components[0].history[last] = DayState::MajorOutage;
        let marker = HistoryIncidentMarker {
            id: Uuid::new_v4(),
            component_id: comp_id,
            title: "Edge nodes returning 502".into(),
            started_at: p.generated_at - ChronoDuration::minutes(45),
            ended_at: Some(p.generated_at - ChronoDuration::minutes(5)),
        };
        let view = build_view(&p, &[marker]);
        // The JSON blob is the data source — assert against it, not the
        // rendered <li> markup (the JS builds those at runtime).
        assert!(view.day_strip_json.contains("Edge nodes returning 502"));
        assert!(view.day_strip_json.contains("day-pop-status--maj"));
        assert!(view.day_strip_json.contains("40m"));
        assert!(view.day_strip_json.contains("\"show_badge\":true"));
    }

    #[test]
    fn day_strip_blob_html_safe() {
        // An attacker-controlled incident title may not introduce ANY raw
        // `<`, `>`, or `&` into the inline JSON — those would let a title
        // close the <script>, open a comment (`<!--`), or break CDATA.
        let p = sample_page();
        let comp_id = p.groups[0].components[0].id;
        let marker = HistoryIncidentMarker {
            id: Uuid::new_v4(),
            component_id: comp_id,
            title: "evil </script><!--<img src=x>& bad".into(),
            started_at: p.generated_at - ChronoDuration::minutes(30),
            ended_at: Some(p.generated_at - ChronoDuration::minutes(5)),
        };
        let view = build_view(&p, &[marker]);
        assert!(!view.day_strip_json.contains('<'));
        assert!(!view.day_strip_json.contains('>'));
        // `&` appears only as `&` — never raw.
        assert!(!view.day_strip_json.contains('&'));
        assert!(view.day_strip_json.contains("\\u003c"));
        // Round-trip safety: the encoded blob still parses back to the
        // original string after JSON.parse.
        let parsed: serde_json::Value =
            serde_json::from_str(&view.day_strip_json).expect("valid json");
        let title = parsed[comp_id.to_string()]["days"]
            .as_array()
            .and_then(|days| {
                days.iter()
                    .rev()
                    .find(|d| !d["related"].as_array().unwrap().is_empty())
            })
            .map(|d| d["related"][0]["title"].as_str().unwrap().to_string())
            .expect("found title");
        assert_eq!(title, "evil </script><!--<img src=x>& bad");
    }

    #[test]
    fn day_overlap_clamps_to_day_window() {
        let comp_id = Uuid::new_v4();
        let now = Utc::now();
        let day_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|nd| DateTime::<Utc>::from_naive_utc_and_offset(nd, Utc))
            .unwrap();
        let day_end = day_start + ChronoDuration::days(1);
        let inc = HistoryIncidentMarker {
            id: Uuid::new_v4(),
            component_id: comp_id,
            title: "spans midnight".into(),
            started_at: day_start - ChronoDuration::hours(3),
            ended_at: Some(day_start + ChronoDuration::hours(2)),
        };
        let pool: Vec<&HistoryIncidentMarker> = vec![&inc];
        let (dur, links) = day_overlap(&pool, day_start, day_end, now);
        assert_eq!(links.len(), 1);
        assert_eq!(dur, ChronoDuration::hours(2));
        let empty: Vec<&HistoryIncidentMarker> = Vec::new();
        let (dur2, links2) = day_overlap(&empty, day_start, day_end, now);
        assert!(links2.is_empty());
        assert_eq!(dur2, ChronoDuration::zero());
    }

    #[test]
    fn fragment_renders_region_without_doctype() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusRegion { view }.render().unwrap();
        assert!(!html.contains("<!doctype html>"));
        assert!(!html.contains("<nav"));
        assert!(html.contains(r#"id="status-region""#));
        assert!(html.contains(r#"hx-trigger="every 30s""#));
        assert!(html.contains("Gateway"));
    }

    #[test]
    fn empty_page_renders_with_zero_components() {
        let mut p = sample_page();
        p.groups.clear();
        let view = build_view(&p, &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("No public components"));
    }

    #[test]
    fn active_incident_banner_renders_when_present() {
        let mut p = sample_page();
        p.overall = OverallStatus {
            state: OverallState::MinorDisruption,
            label: "Minor Service Disruption".into(),
        };
        p.active_incidents.push(PublicIncident {
            id: Uuid::nil(),
            component_id: Uuid::nil(),
            component_name: "Gateway".into(),
            title: "Latency spike".into(),
            started_at: Utc::now() - ChronoDuration::minutes(14),
            ended_at: None,
            severity: IncidentSeverity::Major,
            status_phase: IncidentStatusPhase::Identified,
            updates: vec![PublicIncidentUpdate {
                posted_at: Utc::now() - ChronoDuration::minutes(2),
                phase: IncidentStatusPhase::Identified,
                message: "Rolling back the deploy.".into(),
            }],
            postmortem: None,
        });
        let view = build_view(&p, &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Active incident"));
        assert!(html.contains("Latency spike"));
        assert!(html.contains("Identified"));
        assert!(html.contains("Rolling back the deploy."));
    }

    #[test]
    fn maintenance_card_renders_when_present() {
        let mut p = sample_page();
        p.active_maintenance.push(PublicMaintenance {
            id: Uuid::nil(),
            title: "DB upgrade".into(),
            description: Some("Brief".into()),
            starts_at: Utc::now() - ChronoDuration::minutes(5),
            ends_at: Utc::now() + ChronoDuration::hours(1),
            affected_component_names: vec!["Gateway".into()],
        });
        let view = build_view(&p, &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Scheduled maintenance"));
        assert!(html.contains("DB upgrade"));
        assert!(html.contains("Gateway"));
    }

    #[test]
    fn day_classes_cover_all_states() {
        for s in [
            DayState::Operational,
            DayState::Degraded,
            DayState::PartialOutage,
            DayState::MajorOutage,
            DayState::Maintenance,
            DayState::NoData,
        ] {
            let (class, label, tint) = day_classes(s);
            assert!(!class.is_empty());
            assert!(!label.is_empty());
            assert!(tint.starts_with("day-pop-status--"));
        }
    }

    #[test]
    fn history_stats_computes_uptime() {
        let mut h = vec![DayState::Operational; 90];
        h[10] = DayState::MajorOutage;
        let (pct, summary) = history_stats(&h);
        assert!(pct.starts_with("98"));
        assert!(summary.contains("1 outage"));
    }

    #[test]
    fn incident_detail_renders() {
        let inc = PublicIncident {
            id: Uuid::nil(),
            component_id: Uuid::nil(),
            component_name: "Gateway".into(),
            title: "Latency spike".into(),
            started_at: Utc::now() - ChronoDuration::minutes(30),
            ended_at: Some(Utc::now()),
            severity: IncidentSeverity::Major,
            status_phase: IncidentStatusPhase::Resolved,
            updates: vec![
                PublicIncidentUpdate {
                    posted_at: Utc::now() - ChronoDuration::minutes(25),
                    phase: IncidentStatusPhase::Investigating,
                    message: "Looking into it.".into(),
                },
                PublicIncidentUpdate {
                    posted_at: Utc::now() - ChronoDuration::minutes(5),
                    phase: IncidentStatusPhase::Resolved,
                    message: "Rolled back the deploy.".into(),
                },
            ],
            postmortem: None,
        };
        let detail = IncidentDetailView::from_incident(&inc, Utc::now());
        let html = IncidentDetailPage {
            branding: sample_branding(),
            incident: detail,
            generated_at: Utc::now(),
            rss_url: RSS_URL,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Latency spike"));
        assert!(html.contains("Investigating"));
        assert!(html.contains("Resolved"));
        assert!(html.contains("Rolled back the deploy."));
    }

    #[test]
    fn incident_detail_renders_published_postmortem() {
        let mut inc = fake_incident(Utc::now() - ChronoDuration::hours(2), 9, "DB outage");
        inc.postmortem = Some(crate::domain::PublicPostmortem {
            summary: Some("Connection pool exhausted.".into()),
            root_cause: Some("Missing timeout.".into()),
            impact: None,
            action_items: vec![crate::domain::PublicActionItem {
                text: "Cap pool checkout".into(),
                done: true,
            }],
            published_at: Utc::now(),
        });
        let html = IncidentDetailPage {
            branding: sample_branding(),
            incident: IncidentDetailView::from_incident(&inc, Utc::now()),
            generated_at: Utc::now(),
            rss_url: RSS_URL,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Postmortem"));
        assert!(html.contains("Connection pool exhausted."));
        assert!(html.contains("Cap pool checkout"));

        // No postmortem → no section.
        let mut plain = fake_incident(Utc::now() - ChronoDuration::hours(2), 8, "Blip");
        plain.postmortem = None;
        let html2 = IncidentDetailPage {
            branding: sample_branding(),
            incident: IncidentDetailView::from_incident(&plain, Utc::now()),
            generated_at: Utc::now(),
            rss_url: RSS_URL,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(!html2.contains("Postmortem"));
    }

    fn branding_with(b: PublicOrgBranding) -> BrandingView {
        BrandingView::from_org(
            &OrgBranding {
                name: "Acme".into(),
                slug: "acme".into(),
                branding: b,
            },
            &PublicStatusConfig::default(),
        )
    }

    #[test]
    fn safe_brand_color_accepts_strict_hex_only() {
        assert_eq!(safe_brand_color(Some("#a1B2c3"), "#000000"), "#a1B2c3");
        for bad in [
            "blue",
            "#abc",
            "#3b82f",
            "#3b82f60",
            "#zzzzzz",
            "  #3b82f6",
            "red; } body { display: none }",
        ] {
            assert_eq!(
                safe_brand_color(Some(bad), "#3b82f6"),
                "#3b82f6",
                "should reject {bad:?}"
            );
        }
        assert_eq!(safe_brand_color(None, "#3b82f6"), "#3b82f6");
    }

    #[test]
    fn brand_text_picks_white_on_very_dark_brands() {
        for dark in ["#000000", "#1e3a8a", "#064e3b", "#4c1d95", "#7c2d12"] {
            assert_eq!(
                safe_brand_text_for(dark),
                "#ffffff",
                "{dark} should pair with white"
            );
        }
    }

    #[test]
    fn brand_text_picks_dark_on_anything_brighter() {
        // Pastels, mid-tones (Twitter blue, Slack yellow, Tailwind 500s) all
        // give better contrast against near-black than white per WCAG.
        for bright in [
            "#ffffff", "#ffee99", "#facc15", "#bef264", "#fde68a", "#1d9bf0", "#3b82f6", "#06b6d4",
            "#22c55e", "#ecb22e",
        ] {
            assert_eq!(
                safe_brand_text_for(bright),
                "#0f172a",
                "{bright} should pair with near-black"
            );
        }
    }

    #[test]
    fn brand_text_falls_back_to_white_on_malformed() {
        for bad in ["", "blue", "#fff", "#zzzzzz"] {
            assert_eq!(safe_brand_text_for(bad), "#ffffff");
        }
    }

    #[test]
    fn render_about_strips_disallowed_and_keeps_allowlist() {
        let html = render_about("**bold** _em_ <script>alert(1)</script>\n\n- a\n- b");
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>em</em>"));
        assert!(html.contains("<li>a</li>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)"));
    }

    #[test]
    fn render_about_adds_rel_to_links() {
        let html = render_about("[x](https://example.com)");
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("rel=\"noopener nofollow\""));
    }

    #[test]
    fn branding_renders_logo_about_and_color() {
        let view = build_view(&sample_page(), &[]);
        let branding = branding_with(PublicOrgBranding {
            public_display_name: Some("Acme Public".into()),
            public_about: Some("**hi** there".into()),
            public_brand_color: Some("#ff0000".into()),
            logo_hash: Some("deadbeefcafef00d".into()),
            public_show_powered_by: Some(false),
            ..PublicOrgBranding::default()
        });
        let html = StatusFullPage {
            view,
            branding,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Acme Public Status"));
        assert!(html.contains("--brand-color: #ff0000;"));
        assert!(html.contains(r#"src="/status/branding/logo?v=deadbeefcafef00d""#));
        assert!(html.contains("<strong>hi</strong> there"));
        assert!(!html.contains("Powered by uptimepage"));
    }

    #[test]
    fn og_tags_render_with_image_when_marketing_origin_set() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta {
                title: "Acme Status".into(),
                description: "Live operational status for Acme.".into(),
                og_type: "website",
                url: "https://acme.uptimepage.dev/status".into(),
                image: "https://uptimepage.dev/static/marketing/og.png".into(),
            },
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"<meta property="og:title" content="Acme Status">"#));
        assert!(
            html.contains(
                r#"<meta property="og:url" content="https://acme.uptimepage.dev/status">"#
            )
        );
        assert!(html.contains(
            r#"<meta property="og:image" content="https://uptimepage.dev/static/marketing/og.png">"#
        ));
        assert!(html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#));
        assert!(html.contains(
            r#"<meta name="twitter:image" content="https://uptimepage.dev/static/marketing/og.png">"#
        ));
    }

    #[test]
    fn og_tags_fall_back_to_summary_when_image_empty() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta {
                title: "Acme Status".into(),
                description: "Live operational status for Acme.".into(),
                og_type: "website",
                url: String::new(),
                image: String::new(),
            },
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"<meta name="twitter:card" content="summary">"#));
        assert!(!html.contains("og:image"));
        assert!(!html.contains("og:url"));
        assert!(!html.contains("twitter:image"));
    }

    #[test]
    fn powered_by_shown_by_default() {
        let view = build_view(&sample_page(), &[]);
        let html = StatusFullPage {
            view,
            branding: sample_branding(),
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        assert!(html.contains("Powered by uptimepage"));
    }

    // PRE-MORTEM PM #6: a relaxed DB/app validator must not let a crafted
    // brand colour break out of the `:root` rule. The template-side
    // sanitiser is the independent layer that holds even then.
    #[test]
    fn malicious_brand_color_cannot_escape_style_rule() {
        let view = build_view(&sample_page(), &[]);
        let branding = branding_with(PublicOrgBranding {
            public_brand_color: Some("red; } body { display: none } /*".into()),
            ..PublicOrgBranding::default()
        });
        let html = StatusFullPage {
            view,
            branding,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();
        // Exactly one `--brand-color:` declaration, and it is the default —
        // `var(--brand-color)` uses have no colon so they don't match.
        assert_eq!(html.matches("--brand-color:").count(), 1);
        assert!(html.contains("--brand-color: #3b82f6;"));
        assert!(!html.contains("display: none"));
        assert!(!html.contains("} body {"));
    }

    fn fake_incident(started_at: DateTime<Utc>, id_low: u8, title: &str) -> PublicIncident {
        let mut id_bytes = [0u8; 16];
        id_bytes[15] = id_low;
        PublicIncident {
            id: Uuid::from_bytes(id_bytes),
            component_id: Uuid::nil(),
            component_name: "API".into(),
            title: title.into(),
            started_at,
            ended_at: Some(started_at + ChronoDuration::minutes(15)),
            severity: IncidentSeverity::Minor,
            status_phase: IncidentStatusPhase::Resolved,
            updates: Vec::new(),
            postmortem: None,
        }
    }

    #[test]
    fn bucket_by_month_groups_consecutive_incidents() {
        let now = Utc::now();
        let may_a = chrono::DateTime::parse_from_rfc3339("2026-05-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let may_b = chrono::DateTime::parse_from_rfc3339("2026-05-01T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let apr = chrono::DateTime::parse_from_rfc3339("2026-04-15T11:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let items = vec![
            fake_incident(may_a, 1, "May late"),
            fake_incident(may_b, 2, "May early"),
            fake_incident(apr, 3, "April"),
        ];
        let buckets = bucket_by_month(&items, now);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].label, "May 2026");
        assert_eq!(buckets[0].incidents.len(), 2);
        assert_eq!(buckets[1].label, "April 2026");
        assert_eq!(buckets[1].incidents.len(), 1);
    }

    #[test]
    fn archive_page_renders_buckets_and_next_link() {
        let now = Utc::now();
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-22T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let items = vec![fake_incident(started, 1, "ECS rolled back")];
        let months = bucket_by_month(&items, now);
        let page = IncidentArchivePage {
            branding: sample_branding(),
            months,
            next_cursor: Some("opaque-cursor-token".into()),
            rss_url: RSS_URL,
            og: OgMeta::default(),
        };
        let html = page.render().unwrap();
        assert!(html.contains("Incident history"));
        assert!(html.contains("May 2026"));
        assert!(html.contains("ECS rolled back"));
        assert!(
            html.contains("/status/incidents?cursor=opaque-cursor-token"),
            "next-page link must include cursor"
        );
    }

    #[test]
    fn archive_page_renders_empty_state_without_next_link() {
        let page = IncidentArchivePage {
            branding: sample_branding(),
            months: Vec::new(),
            next_cursor: None,
            rss_url: RSS_URL,
            og: OgMeta::default(),
        };
        let html = page.render().unwrap();
        assert!(html.contains("No incidents recorded."));
        assert!(!html.contains("Older incidents"));
    }

    #[test]
    fn branding_defaults_when_all_fields_null() {
        // With every override unset, the display name falls back to the org
        // name and no logo image is emitted (the header shows text). The
        // default colour and powered-by footer are covered by their own
        // tests above; this one pins the resolved-name + no-logo path.
        let view = build_view(&sample_page(), &[]);
        let branding = branding_with(PublicOrgBranding::default());
        let html = StatusFullPage {
            view,
            branding,
            og: OgMeta::default(),
        }
        .render()
        .unwrap();

        assert!(html.contains("Acme Status"), "display name = org name");
        assert!(
            !html.contains("/status/branding/logo"),
            "no logo img when path unset"
        );
    }
}
