//! Operator UI for managing public status pages: a list (`/settings/pages`)
//! and a per-page editor (`/settings/pages/{id}`). Both drive the
//! `/api/v1/status-pages` JSON API via fetch + `smRenderApiError` (the shared
//! modern-form pattern), so this module only renders chrome + initial state.

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{OrgId, PublicStyle};
use crate::error::AppError;
use crate::web::auth::CurrentOrg;
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::views::public_status::{
    public_base, public_host_suffix, public_logo_url, public_status_url,
};
use crate::web::views::resolve_org;

const TAB_PAGES: &str = "pages";

/// Upper bound on monitors offered as curation candidates in the page editor.
/// Matches the store's hard `LIMIT` ceiling, so every monitor an org can own is
/// reachable from the editor.
const MAX_CURATION_CANDIDATES: usize = 10_000;

// ── List ──────────────────────────────────────────────────────────────────────

pub struct PageRow {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub enabled: bool,
    /// Absolute live URL, or empty in path mode.
    pub url: String,
    pub subscriber_count: i64,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/pages.html")]
pub struct PagesPage {
    pub active_tab: &'static str,
    pub rows: Vec<PageRow>,
    /// True when the org is at its `max_status_pages` cap (create disabled).
    pub at_limit: bool,
    pub max_pages: i32,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/pages_partial.html")]
pub struct PagesPartial {
    pub rows: Vec<PageRow>,
}

async fn build_rows(state: &AppState, org: OrgId) -> WebResult<Vec<PageRow>> {
    let pages = state.status_page_store.list(org).await?;
    let counts: std::collections::HashMap<Uuid, i64> = match state.db.as_ref() {
        Some(pool) => crate::storage::subscribers::verified_counts(pool, org.0)
            .await?
            .into_iter()
            .collect(),
        None => std::collections::HashMap::new(),
    };
    Ok(pages
        .into_iter()
        .map(|p| {
            let url = public_base(&state.cfg, &p.slug)
                .map(|o| public_status_url(&state.cfg, &o))
                .unwrap_or_default();
            let subscriber_count = counts.get(&p.id.0).copied().unwrap_or(0);
            PageRow {
                id: p.id.to_string(),
                name: p.name,
                slug: p.slug,
                enabled: p.enabled,
                url,
                subscriber_count,
            }
        })
        .collect())
}

pub async fn pages_list(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
) -> WebResult<Response> {
    let org = match resolve_org(org, "/settings/pages") {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let rows = build_rows(&state, org).await?;
    let max_pages = state.quotas.limit_for_org(org).await?.max_status_pages;
    Ok(PagesPage {
        active_tab: TAB_PAGES,
        at_limit: rows.len() as i32 >= max_pages,
        max_pages,
        rows,
    }
    .into_response())
}

pub async fn pages_partial(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
) -> WebResult<Response> {
    let org = match resolve_org(org, "/settings/pages") {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let rows = build_rows(&state, org).await?;
    Ok(PagesPartial { rows }.into_response())
}

// ── Editor ────────────────────────────────────────────────────────────────────

/// One monitor as a curation candidate: whether it's on this page, and its
/// per-page overrides when it is.
pub struct ComponentRow {
    pub target_id: String,
    pub monitor_name: String,
    pub kind: &'static str,
    pub name_hint: &'static str,
    pub on_page: bool,
    pub public_name: String,
    pub public_group: String,
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "http" => "HTTP",
        "tcp" => "TCP",
        "ping" => "Ping",
        "heartbeat" => "Heartbeat",
        "dns" => "DNS",
        "tls_cert" => "TLS",
        "domain_expiry" => "Domain",
        _ => "Check",
    }
}

fn name_hint(kind: &str) -> &'static str {
    match kind {
        "http" => "e.g. Website",
        "tcp" => "e.g. TCP port",
        "ping" => "e.g. Gateway",
        "heartbeat" => "e.g. Nightly backup",
        "dns" => "e.g. DNS",
        "tls_cert" => "e.g. TLS certificate",
        "domain_expiry" => "e.g. Domain expiry",
        _ => "Public display name",
    }
}

pub struct StyleOption {
    pub value: &'static str,
    pub selected: bool,
}

/// One subscriber row in the editor's roster. `contact` is masked for email.
pub struct SubscriberView {
    pub id: String,
    pub contact: String,
    pub channel: String,
    pub confirmed: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/page_editor.html")]
pub struct PageEditorPage {
    pub active_tab: &'static str,
    pub id: String,
    pub slug: String,
    pub name: String,
    pub enabled: bool,
    pub public_display_name: String,
    pub public_about: String,
    pub brand_color_value: String,
    pub show_powered_by: bool,
    pub styles: Vec<StyleOption>,
    pub host_suffix: Option<String>,
    pub status_url: String,
    /// Absolute base for the embeddable badge SVG. `None` only when no public
    /// base URL is configured.
    pub badge_url: Option<String>,
    pub logo_url: String,
    /// Intrinsic logo dims for CLS-safe `<img width height>`; 0 = no logo.
    pub logo_w: i64,
    pub logo_h: i64,
    pub max_logo_bytes: u64,
    pub max_logo_dim_px: u32,
    /// Monitors on this page first (in order), then the rest by name.
    pub components: Vec<ComponentRow>,
    /// Confirmed + pending subscribers, newest first.
    pub subscribers: Vec<SubscriberView>,
}

pub async fn page_editor(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
    Path(id): Path<Uuid>,
) -> WebResult<Response> {
    use crate::domain::StatusPageId;
    let org = match resolve_org(org, "/settings/pages") {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let page_id = StatusPageId(id);
    let Some(page) = state.status_page_store.get(org, page_id).await? else {
        return Ok((
            axum::http::StatusCode::NOT_FOUND,
            crate::web::error::NotFoundPage {
                active_tab: TAB_PAGES,
            },
        )
            .into_response());
    };

    // On-page components (curated, in order) + every other org monitor.
    let curated = state
        .status_page_store
        .list_components(org, page_id)
        .await?;
    let on_page: std::collections::HashSet<Uuid> = curated.iter().map(|c| c.target_id).collect();
    // Pull every target: the default filter caps at 100, which would silently
    // hide monitors past that from large orgs.
    let all_targets = state
        .target_store
        .list(
            org,
            crate::storage::TargetFilter {
                limit: Some(MAX_CURATION_CANDIDATES),
                ..Default::default()
            },
        )
        .await?;
    let kind_by_id: std::collections::HashMap<Uuid, &'static str> =
        all_targets.iter().map(|t| (t.id, t.check.kind())).collect();
    let mut rows: Vec<ComponentRow> = curated
        .iter()
        .map(|c| {
            let kind = kind_by_id.get(&c.target_id).copied().unwrap_or("");
            ComponentRow {
                target_id: c.target_id.to_string(),
                monitor_name: c.monitor_name.clone(),
                kind: kind_label(kind),
                name_hint: name_hint(kind),
                on_page: true,
                public_name: c.public_name.clone().unwrap_or_default(),
                public_group: c.public_group.clone().unwrap_or_default(),
            }
        })
        .collect();
    let mut others: Vec<_> = all_targets
        .into_iter()
        .filter(|t| !on_page.contains(&t.id))
        .collect();
    others.sort_by_key(|t| t.name.to_lowercase());
    rows.extend(others.into_iter().map(|t| {
        let kind = t.check.kind();
        ComponentRow {
            kind: kind_label(kind),
            name_hint: name_hint(kind),
            target_id: t.id.to_string(),
            monitor_name: t.name,
            on_page: false,
            public_name: String::new(),
            public_group: String::new(),
        }
    }));

    let cfg = &state.cfg.public_status;
    let b = &page.branding;
    let base = public_base(&state.cfg, &page.slug);
    let status_url = base
        .as_deref()
        .map(|o| public_status_url(&state.cfg, o))
        .unwrap_or_default();
    // Absolute origin for the badge: subdomain in SaaS mode, else the
    // configured public base URL so path-based/self-host deploys still get a
    // README-ready link.
    let badge_origin = crate::web::host::page_origin(
        &state.cfg.public_status.base_domain,
        &state.cfg.auth.public_base_url,
        &page.slug,
        None,
        false,
    );
    let badge_url = (!badge_origin.is_empty()).then(|| {
        format!(
            "{}/api/public/v1/badge.svg",
            badge_origin.trim_end_matches('/')
        )
    });
    let logo_url = b
        .logo_hash
        .as_deref()
        .and_then(|hash| public_logo_url(base.as_deref(), hash))
        .unwrap_or_default();
    let (logo_w, logo_h) = if b.logo_hash.is_some() {
        match state
            .page_asset_store
            .get_meta(org, page_id, crate::domain::AssetSlot::Logo)
            .await?
        {
            Some(m) => (
                m.metadata
                    .get("width")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                m.metadata
                    .get("height")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            ),
            None => (0, 0),
        }
    } else {
        (0, 0)
    };
    let styles = PublicStyle::ALL
        .iter()
        .map(|v| StyleOption {
            value: v,
            selected: *v == b.public_style.as_str(),
        })
        .collect();

    let subscribers = match state.db.as_ref() {
        Some(pool) => crate::storage::subscribers::list_for_page(pool, org.0, page_id.0)
            .await?
            .into_iter()
            .map(|s| {
                let contact = if s.channel == "email" {
                    crate::email::mask_email(&s.target)
                } else {
                    s.target
                };
                SubscriberView {
                    id: s.id.to_string(),
                    contact,
                    channel: s.channel,
                    confirmed: s.verified,
                    created_at: s.created_at,
                }
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(PageEditorPage {
        active_tab: TAB_PAGES,
        id: page.id.to_string(),
        slug: page.slug.clone(),
        name: page.name,
        enabled: page.enabled,
        public_display_name: b.public_display_name.clone().unwrap_or_default(),
        public_about: b.public_about.clone().unwrap_or_default(),
        brand_color_value: b
            .public_brand_color
            .clone()
            .unwrap_or_else(|| cfg.default_brand_color.clone()),
        show_powered_by: b.show_powered_by(cfg.default_show_powered_by),
        styles,
        host_suffix: public_host_suffix(&state.cfg),
        status_url,
        badge_url,
        logo_url,
        logo_w,
        logo_h,
        max_logo_bytes: u64::from(cfg.max_logo_size_bytes),
        max_logo_dim_px: cfg.max_logo_dimension_px,
        components: rows,
        subscribers,
    }
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;

    #[test]
    fn every_check_kind_has_label_and_hint() {
        for kind in crate::domain::CheckSpec::ALL_KINDS {
            assert_ne!(kind_label(kind), "Check", "fallback label for {kind}");
            assert_ne!(
                name_hint(kind),
                "Public display name",
                "fallback hint for {kind}"
            );
        }
    }

    fn editor(badge_url: Option<String>) -> PageEditorPage {
        PageEditorPage {
            active_tab: TAB_PAGES,
            id: "11111111-1111-1111-1111-111111111111".into(),
            slug: "acme".into(),
            name: "acme".into(),
            enabled: true,
            public_display_name: "Acme".into(),
            public_about: String::new(),
            brand_color_value: "#000000".into(),
            show_powered_by: true,
            styles: Vec::new(),
            host_suffix: Some(".uptimepage.dev".into()),
            status_url: "https://acme.uptimepage.dev".into(),
            badge_url,
            logo_url: String::new(),
            logo_w: 0,
            logo_h: 0,
            max_logo_bytes: 1024,
            max_logo_dim_px: 512,
            components: vec![ComponentRow {
                target_id: "22222222-2222-2222-2222-222222222222".into(),
                monitor_name: "api".into(),
                kind: "HTTP",
                name_hint: "e.g. Website",
                on_page: true,
                public_name: "API".into(),
                public_group: String::new(),
            }],
            subscribers: Vec::new(),
        }
    }

    #[test]
    fn badge_panel_renders_overall_and_per_component_snippets() {
        let html = editor(Some(
            "https://acme.uptimepage.dev/api/public/v1/badge.svg".into(),
        ))
        .render()
        .unwrap();
        assert!(html.contains("https://acme.uptimepage.dev/api/public/v1/badge.svg"));
        assert!(html.contains("?component=22222222-2222-2222-2222-222222222222"));
        assert!(html.contains("data-copy=\"#badge-md-overall\""));
    }

    #[test]
    fn component_row_shows_check_kind() {
        let html = editor(None).render().unwrap();
        assert!(html.contains("HTTP"));
    }

    #[test]
    fn badge_panel_absent_without_badge_url() {
        let html = editor(None).render().unwrap();
        assert!(!html.contains("/api/public/v1/badge.svg"));
    }
}
