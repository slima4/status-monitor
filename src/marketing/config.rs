//! Owned config for the marketing module. A small subset of global
//! config is cloned in at boot — no `AppConfig`, no pool, no `AppState`.

/// Product wordmark. Single source — every Rust-side emitter (OG, JSON-LD,
/// llms.txt, blog renderer) reads from here so a rename is one diff. The
/// askama templates also embed this literal for now; treat the template
/// strings as authored copy that should be re-reviewed on a rename rather
/// than mechanically swapped, but keep this constant authoritative for
/// anything machine-generated.
pub const BRAND: &str = "Uptimepage";

/// Short one-line pitch. Used by llms.txt and as the in-image subtitle
/// on the OG card.
pub const TAGLINE: &str = "Uptime monitoring and public status pages that just work.";

/// `<meta name="description">` + OG `og:description`. Sized to Google's
/// 110–160 char sweet spot so search snippets don't truncate mid-sentence.
pub const META_DESCRIPTION: &str = "Uptime monitoring and public status pages that just work. HTTP, TCP, DNS, TLS checks. Slack, email, webhook alerts. Start free, no card.";

/// Automation surfaces on their own prod hosts — not derived from
/// `canonical_origin` (separate hostnames), so authored absolute.
pub const MCP_URL: &str = "https://mcp.uptimepage.dev/mcp";
pub const TERRAFORM_URL: &str = "https://registry.terraform.io/providers/uptimepage/uptimepage";

/// What the marketing handlers read. Mirrors the relevant fields of
/// `crate::config::MarketingConfig` so this struct compiles untouched
/// in the extracted service.
#[derive(Debug, Clone)]
pub struct MarketingCfg {
    pub app_url: String,
    pub canonical_origin: String,
    pub blog_enabled: bool,
}
