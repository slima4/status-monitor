//! Owned config for the marketing module. A small subset of global
//! config is cloned in at boot — no `AppConfig`, no pool, no `AppState`.

/// Product wordmark. Single source — every Rust-side emitter (OG, JSON-LD,
/// llms.txt, blog renderer) reads from here so a rename is one diff. The
/// askama templates also embed this literal for now; treat the template
/// strings as authored copy that should be re-reviewed on a rename rather
/// than mechanically swapped, but keep this constant authoritative for
/// anything machine-generated.
pub const BRAND: &str = "Uptimepage";

/// One-line product pitch. Reused by the OG/Twitter card default
/// description, llms.txt, and the `{% block description %}` fallback in
/// base.html. Keeping a single source prevents drift (proved during the
/// rebrand pass — the base.html default and the seo.rs default disagreed
/// for hours after the templates were updated).
pub const TAGLINE: &str = "Uptime monitoring and public status pages that just work.";

/// What the marketing handlers read. Mirrors the relevant fields of
/// `crate::config::MarketingConfig` so this struct compiles untouched
/// in the extracted service.
#[derive(Debug, Clone)]
pub struct MarketingCfg {
    pub app_url: String,
    pub canonical_origin: String,
    pub blog_enabled: bool,
}
