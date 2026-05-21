//! Page handlers (landing + branded 404). Landing renders into a
//! `OnceLock` at first hit, so every request after that serves the
//! cached body + stable ETag with no askama work and no allocations.

use std::sync::Arc;
use std::sync::OnceLock;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::response::{IntoResponse, Response};

use crate::marketing::seo::{JsonLd, OpenGraph, json_ld_organization, json_ld_website};
use crate::web::assets::filters;

use super::config::{BRAND, MarketingCfg};

const PAGE_CACHE_CONTROL: &str = "public, max-age=300, stale-while-revalidate=86400";
const NOT_FOUND_CACHE_CONTROL: &str = "public, max-age=300";

#[derive(Template, WebTemplate)]
#[template(path = "marketing/landing.html")]
struct LandingPage {
    app_url: String,
    canonical_url: String,
    og: OpenGraph,
    org_json_ld: JsonLd,
    website_json_ld: JsonLd,
    version: &'static str,
    pricing_features: &'static [&'static str],
}

const PRICING_FEATURES: &[&str] = &[
    "Unlimited monitors",
    "Public status page",
    "Slack, email & webhook alerts",
    "Team members",
    "No credit card",
];

#[derive(Template, WebTemplate)]
#[template(path = "marketing/not_found.html")]
pub struct NotFoundPage {
    pub canonical_url: String,
}

/// One landing render per process. The body is invariant after boot —
/// `app_url`, `canonical_origin`, version, JSON-LD all come from
/// startup config — so re-rendering and re-hashing per request would
/// burn ~80–150 µs for an identical response.
static LANDING_CACHED: OnceLock<CachedRender> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct CachedRender {
    pub(crate) body: String,
    pub(crate) etag: String,
}

fn render_landing(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = cfg.canonical_origin.clone();
    let og = OpenGraph::default_for(
        &format!("{BRAND} — uptime monitoring & public status pages"),
        &canonical_url,
    );
    let page = LandingPage {
        app_url: cfg.app_url.clone(),
        canonical_url,
        org_json_ld: json_ld_organization(&cfg.canonical_origin),
        website_json_ld: json_ld_website(&cfg.canonical_origin),
        og,
        version: env!("CARGO_PKG_VERSION"),
        pricing_features: PRICING_FEATURES,
    };
    let body = page
        .render()
        .unwrap_or_else(|e| format!("<!-- landing render failed: {e} -->"));
    let etag = body_etag(&body);
    CachedRender { body, etag }
}

pub async fn landing(State(cfg): State<Arc<MarketingCfg>>, headers: HeaderMap) -> Response {
    let cached = LANDING_CACHED.get_or_init(|| render_landing(&cfg));
    serve_cached(&headers, cached, PAGE_CACHE_CONTROL)
}

pub async fn not_found(State(cfg): State<Arc<MarketingCfg>>) -> Response {
    static NF_CACHED: OnceLock<CachedRender> = OnceLock::new();
    let cached = NF_CACHED.get_or_init(|| {
        let body = NotFoundPage {
            canonical_url: cfg.canonical_origin.clone(),
        }
        .render()
        .unwrap_or_else(|_| "Not Found".to_string());
        let etag = body_etag(&body);
        CachedRender { body, etag }
    });
    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (CACHE_CONTROL, NOT_FOUND_CACHE_CONTROL.to_string()),
            (ETAG, cached.etag.clone()),
        ],
        cached.body.clone(),
    )
        .into_response()
}

/// 200 + body OR 304 when the client's `If-None-Match` matches. ETag
/// is a SHA-256 prefix of the rendered HTML so identical builds serve a
/// stable token — what makes a CDN's revalidation actually save bytes.
pub(crate) fn serve_cached(
    headers: &HeaderMap,
    cached: &CachedRender,
    cache_control: &'static str,
) -> Response {
    if headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == cached.etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (ETAG, cached.etag.clone()),
                (CACHE_CONTROL, cache_control.to_string()),
            ],
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (CACHE_CONTROL, cache_control.to_string()),
            (ETAG, cached.etag.clone()),
        ],
        cached.body.clone(),
    )
        .into_response()
}

pub(crate) fn body_etag(body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("\"{}\"", hex::encode(&hasher.finalize()[..16]))
}
