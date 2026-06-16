//! Marketing site & blog served from the apex host.
//!
//! **Hard-isolated by contract:** this module must not import
//! `crate::storage`, `crate::domain`, `crate::tenancy`, `crate::state`,
//! or `AppState`. The dispatch seam (one middleware, one router call —
//! see `marketing::dispatch`) is the entire coupling to the rest of the
//! binary; extracting marketing to its own service is a copy + delete.
//! No-DB on the marketing path is what makes that extraction trivial.
//!
//! Permitted shared deps: axum, askama, tower-http, the config crate,
//! and `crate::web::assets` (the fingerprinted-asset map + askama
//! filter + `mount_static` helper). On extraction the assets module
//! moves with the marketing site or is duplicated into the extracted
//! service; either way the rebind is one path.
//! The Markdown renderer is **vendored locally** (`blog::render`) —
//! deliberately not the trusted-unsanitised legal renderer; blog
//! content is third-party PR input and has a different trust model.

pub mod blog;
pub mod config;
pub mod dispatch;
pub mod landings;
pub mod legal;
pub mod pages;
pub mod seo;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tower_http::compression::CompressionLayer;

pub use config::MarketingCfg;
pub use dispatch::RouteByHost;

/// Build the marketing router. Takes its own config — no `AppState`,
/// no pools. The `CompressionLayer` is scoped here so it never wraps
/// `/healthz`/`/readyz` on the app surface.
///
/// Every page/blog/legal/seo cache is warmed here at boot via
/// [`warm_caches`], so the first real request after startup never pays
/// askama + SHA-256 + (for blog) markdown + ammonia on the hot path.
pub fn router(cfg: MarketingCfg) -> Router {
    let state = Arc::new(cfg);
    warm_caches(&state);
    let mut r = Router::new()
        .route("/", get(pages::landing))
        .route("/robots.txt", get(seo::robots_txt))
        .route("/sitemap.xml", get(seo::sitemap_xml))
        .route("/llms.txt", get(seo::llms_txt))
        .route("/llms-full.txt", get(seo::llms_full_txt));
    r = legal::mount(r);
    r = landings::mount(r);
    if state.blog_enabled {
        r = r
            .route("/blog", get(blog::index))
            .route("/blog/{slug}", get(blog::post));
    }
    // The dispatcher routes the whole apex/www host to this router, so
    // marketing must own a `/static/{*path}` route — otherwise every
    // asset href emitted by a template falls through to the marketing
    // 404. Funnelled through `mount_static` so the path + handler stay
    // in lockstep with the operator-app declaration.
    crate::web::assets::mount_static(r)
        .fallback(pages::not_found)
        .layer(CompressionLayer::new())
        .with_state(state)
}

/// Eagerly populate every `OnceLock` cache in the marketing module.
/// Each callee is independently idempotent (`OnceLock::get_or_init` +
/// `list_published()` auto-loads posts), so call order is irrelevant —
/// no cross-module ordering contract to keep in lockstep with edits.
fn warm_caches(state: &Arc<MarketingCfg>) {
    pages::warm(state);
    legal::warm(state);
    landings::warm(state);
    if state.blog_enabled {
        blog::warm(state);
    }
    seo::warm(state);
}
