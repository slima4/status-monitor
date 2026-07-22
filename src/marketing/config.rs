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
pub const META_DESCRIPTION: &str = "Uptime monitoring and public status pages that just work. HTTP, TCP, DNS, TLS, ping checks. Slack, Telegram, SMS, PagerDuty alerts. Start free, no card.";

/// Automation surfaces on their own prod hosts — not derived from
/// `canonical_origin` (separate hostnames), so authored absolute.
pub const MCP_URL: &str = "https://mcp.uptimepage.dev/mcp";
pub const TERRAFORM_URL: &str = "https://registry.terraform.io/providers/uptimepage/uptimepage";
pub const SOURCE_URL: &str = "https://github.com/uptimepage/uptimepage";

/// Named blog author — a verifiable Person for search-engine E-E-A-T.
#[derive(Debug, Clone)]
pub struct Author {
    pub name: &'static str,
    pub role: &'static str,
    pub bio: &'static str,
    pub url: &'static str,
    pub image: &'static str,
    pub same_as: &'static [(&'static str, &'static str)],
}

pub const AUTHOR: Author = Author {
    name: "Artem Senenko",
    role: "Founder & Software Engineer, Uptimepage",
    bio: "Software engineer with 20+ years building and running production \
          systems: microservice architecture on Kubernetes, cloud \
          infrastructure on AWS and Terraform, and security-critical SaaS in \
          the fintech domain.",
    url: "https://www.linkedin.com/in/artem-senenko-b3195927/",
    image: "img/authors/artem-senenko.jpg",
    same_as: &[
        (
            "LinkedIn",
            "https://www.linkedin.com/in/artem-senenko-b3195927/",
        ),
        ("GitHub", "https://github.com/slima4"),
        ("X", "https://x.com/sl1ma4"),
        ("Mastodon", "https://mastodon.social/@slima4"),
        ("Strivle", "https://www.strivle.com/u/slima4"),
    ],
};

/// What the marketing handlers read: the relevant fields of
/// `crate::config::MarketingConfig`, plus what the discovery surfaces need
/// from other config sections. Owns its fields so this struct compiles
/// untouched in the extracted service.
#[derive(Debug, Clone)]
pub struct MarketingCfg {
    pub app_url: String,
    pub canonical_origin: String,
    pub blog_enabled: bool,
    /// This deployment's MCP endpoint. Never falls back to [`MCP_URL`]:
    /// the catalog is a machine contract, that constant is hosted-only copy.
    pub mcp_url: Option<String>,
}
