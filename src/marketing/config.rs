//! Owned config for the marketing module. A small subset of global
//! config is cloned in at boot — no `AppConfig`, no pool, no `AppState`.

/// What the marketing handlers read. Mirrors the relevant fields of
/// `crate::config::MarketingConfig` so this struct compiles untouched
/// in the extracted service.
#[derive(Debug, Clone)]
pub struct MarketingCfg {
    pub app_url: String,
    pub canonical_origin: String,
    pub blog_enabled: bool,
}
