//! Marketing site & blog served from the apex host.
//!
//! **Hard-isolated by contract:** this module must not import
//! `crate::storage`, `crate::domain`, `crate::tenancy`, `crate::state`,
//! or `AppState`. The dispatch seam (one middleware, one router call —
//! see `marketing::dispatch`) is the entire coupling to the rest of the
//! binary; extracting marketing to its own service is a copy + delete.
//! No-DB on the marketing path is what makes that extraction trivial.
//!
//! Permitted shared deps: axum, askama, tower-http, the config crate.
//! The Markdown renderer is **vendored locally** (`blog::render`) —
//! deliberately not the trusted-unsanitised legal renderer; blog
//! content is third-party PR input and has a different trust model.

pub mod blog;
pub mod config;
pub mod dispatch;
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
pub fn router(cfg: MarketingCfg) -> Router {
    let state = Arc::new(cfg);
    let mut r = Router::new()
        .route("/", get(pages::landing))
        .route("/robots.txt", get(seo::robots_txt))
        .route("/sitemap.xml", get(seo::sitemap_xml))
        .route("/llms.txt", get(seo::llms_txt));
    if state.blog_enabled {
        r = r
            .route("/blog", get(blog::index))
            .route("/blog/{slug}", get(blog::post));
    }
    r.fallback(pages::not_found)
        .layer(CompressionLayer::new())
        .with_state(state)
}
