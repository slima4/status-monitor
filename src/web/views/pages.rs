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
    Ok(pages
        .into_iter()
        .map(|p| {
            let url = public_base(&state.cfg, &p.slug)
                .map(|o| public_status_url(&state.cfg, &o))
                .unwrap_or_default();
            PageRow {
                id: p.id.to_string(),
                name: p.name,
                slug: p.slug,
                enabled: p.enabled,
                url,
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
    pub on_page: bool,
    pub public_name: String,
    pub public_group: String,
}

pub struct StyleOption {
    pub value: &'static str,
    pub selected: bool,
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
    pub logo_url: String,
    /// Intrinsic logo dims for CLS-safe `<img width height>`; 0 = no logo.
    pub logo_w: i64,
    pub logo_h: i64,
    pub max_logo_bytes: u64,
    pub max_logo_dim_px: u32,
    /// Monitors on this page first (in order), then the rest by name.
    pub components: Vec<ComponentRow>,
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
    let mut rows: Vec<ComponentRow> = curated
        .iter()
        .map(|c| ComponentRow {
            target_id: c.target_id.to_string(),
            monitor_name: c.monitor_name.clone(),
            on_page: true,
            public_name: c.public_name.clone().unwrap_or_default(),
            public_group: c.public_group.clone().unwrap_or_default(),
        })
        .collect();
    // The editor curates from the org's full monitor set, so pull every
    // target — the default filter caps at 100, which would silently hide
    // monitors past that from large orgs.
    let mut others = state
        .target_store
        .list(
            org,
            crate::storage::TargetFilter {
                limit: Some(MAX_CURATION_CANDIDATES),
                ..Default::default()
            },
        )
        .await?;
    others.retain(|t| !on_page.contains(&t.id));
    others.sort_by_key(|t| t.name.to_lowercase());
    rows.extend(others.into_iter().map(|t| ComponentRow {
        target_id: t.id.to_string(),
        monitor_name: t.name,
        on_page: false,
        public_name: String::new(),
        public_group: String::new(),
    }));

    let cfg = &state.cfg.public_status;
    let b = &page.branding;
    let base = public_base(&state.cfg, &page.slug);
    let status_url = base
        .as_deref()
        .map(|o| public_status_url(&state.cfg, o))
        .unwrap_or_default();
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
        logo_url,
        logo_w,
        logo_h,
        max_logo_bytes: u64::from(cfg.max_logo_size_bytes),
        max_logo_dim_px: cfg.max_logo_dimension_px,
        components: rows,
    }
    .into_response())
}
