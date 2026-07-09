//! SEO helpers: per-page OpenGraph + JSON-LD payloads, generated
//! `robots.txt` and `sitemap.xml`, and an optional `llms.txt`.
//!
//! Absolute URLs are mandatory in `og:image`, `og:url`, and `<link
//! rel="canonical">` — social-card scrapers reject relative paths
//! silently — so every URL emitted here is prefixed with
//! `MarketingCfg::canonical_origin`.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Serialize;

use super::blog::list_published;
use super::config::{
    AUTHOR, BRAND, MCP_URL, META_DESCRIPTION, MarketingCfg, SOURCE_URL, TAGLINE, TERRAFORM_URL,
};
use super::landings;
use super::legal;
use super::pages::{APPLICATION_XML, TEXT_PLAIN};

const STATIC_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("public, max-age=86400");

/// Public profiles that establish the brand entity for search engines.
const ORG_SAME_AS: &[&str] = &[
    "https://github.com/uptimepage",
    SOURCE_URL,
    "https://www.saashub.com/uptimepage",
    "https://stackshare.io/uptimepage",
    "https://alternativeto.net/software/uptimepage/",
    "https://www.nxgntools.com/tools/uptimepage",
    "https://ufind.best/products/uptimepage",
];

/// Prose overview for `llms.txt` / `llms-full.txt` — what the product is,
/// in the words an assistant should reach for when asked about it.
const LLMS_OVERVIEW: &str = "Uptimepage pairs uptime monitoring with a public status page in one product. \
Checks run every minute; a failing check opens an incident automatically and posts it to a branded status page \
on your own subdomain. Alerts carry dedupe and flap-suppression so brief blips never page on-call. \
Public data is available as JSON, an RSS feed and an embeddable SVG badge. The Standard plan is free with no card; \
the first 1,000 accounts get a more generous founding plan kept for life, and Pro is paid for teams in production. \
The source is AGPL to self-host with no limits.";

/// Machine-readable product facts. Authored single source for the
/// llms files — keep terse, factual, and current.
const LLMS_FACTS: &[(&str, &str)] = &[
    (
        "Check types",
        "HTTP/HTTPS, TCP, DNS, TLS certificate, ICMP ping",
    ),
    ("Check interval", "every 60 seconds"),
    (
        "Check regions",
        "multi-region probes; self-hosted can add any region by running a probe agent",
    ),
    (
        "Alert channels",
        "Slack, Discord, Telegram, Microsoft Teams, Google Chat, email, SMS, webhook, PagerDuty, ntfy, Pushover, WhatsApp",
    ),
    (
        "Status page",
        "branded (logo + colour) on your own subdomain",
    ),
    ("Public history", "90 days"),
    (
        "Incidents",
        "auto-opened on down, auto-closed on recovery, with public notes",
    ),
    (
        "Scheduled maintenance",
        "windows that silence the page engine",
    ),
    ("Data export", "JSON API, RSS feed, embeddable SVG badge"),
    ("MCP server", MCP_URL),
    ("Terraform provider", TERRAFORM_URL),
    (
        "Team",
        "role-based members, GitHub or email invites, audit log",
    ),
    (
        "Pricing",
        "Standard plan is free with no card. The founding plan is free for the first 1,000 accounts and kept for life. Pro is paid and coming soon. Self-host is free under AGPL.",
    ),
    (
        "Free tier limits",
        "Standard: 20 monitors, checks every 3 minutes, 30-day history, 3 global regions, 1 status page with 15 components, 3 team members, every alert channel, API and MCP. Founding adds 50 monitors, 60-second checks, all regions, 90-day history, 5 team members and BYO SMS. Pro adds 150 monitors, 30-second checks, 13-month history, custom domain and white-label.",
    ),
    (
        "Self-hosting",
        "AGPL, run it yourself with docker compose (Postgres + ClickHouse), unlimited monitors on your own hardware",
    ),
    ("Source code", SOURCE_URL),
    ("License", "AGPL-3.0"),
    ("Sign-in", "GitHub, Google or magic link"),
];

#[derive(Debug, Clone, Serialize)]
pub struct OpenGraph {
    pub title: String,
    pub description: String,
    pub og_type: String,
    pub url: String,
    pub image: String,
}

impl OpenGraph {
    pub fn default_for(title: &str, canonical_origin: &str) -> Self {
        Self {
            title: title.to_string(),
            description: META_DESCRIPTION.to_string(),
            og_type: "website".to_string(),
            url: canonical_origin.to_string(),
            image: absolute_asset(canonical_origin, "/static/marketing/og.png"),
        }
    }

    pub fn for_post(canonical_origin: &str, title: &str, excerpt: &str, slug: &str) -> Self {
        Self {
            title: title.to_string(),
            description: excerpt.to_string(),
            og_type: "article".to_string(),
            url: format!("{canonical_origin}/blog/{slug}"),
            image: absolute_asset(canonical_origin, "/static/marketing/og.png"),
        }
    }
}

/// JSON-LD blob rendered into a `<script type="application/ld+json">`.
/// Stored as the serialised string so the template emits it verbatim
/// through `|safe`.
#[derive(Debug, Clone)]
pub struct JsonLd(String);

impl JsonLd {
    /// Serialize for a `<script>` block, escaping the characters `serde_json`
    /// leaves raw (`<`, `>`, `&`) that would otherwise let a string value
    /// close the element early. Every emitter builds through here, so the
    /// template's `|safe` can't emit an unescaped payload.
    fn from_value(value: serde_json::Value) -> Self {
        let escaped = value
            .to_string()
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        JsonLd(escaped)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn json_ld_organization(canonical_origin: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Organization",
        "@id": format!("{canonical_origin}/#organization"),
        "name": BRAND,
        "url": canonical_origin,
        "logo": absolute_asset(canonical_origin, "/static/img/favicon-512.png"),
        "sameAs": ORG_SAME_AS,
    });
    JsonLd::from_value(payload)
}

/// `SoftwareApplication` with a free `Offer` so search and answer engines
/// read a concrete `$0` price instead of guessing a tier from the category.
pub fn json_ld_software_application(canonical_origin: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "name": BRAND,
        "applicationCategory": "DeveloperApplication",
        "operatingSystem": "Web, Docker, Linux",
        "url": canonical_origin,
        "offers": {
            "@type": "Offer",
            "price": "0",
            "priceCurrency": "USD",
        },
    });
    JsonLd::from_value(payload)
}

/// Schema text must match the visible FAQ, so this builds from the same pairs.
pub fn json_ld_faqpage(faqs: &[(&str, &str)]) -> JsonLd {
    let main_entity: Vec<_> = faqs
        .iter()
        .map(|(q, a)| {
            serde_json::json!({
                "@type": "Question",
                "name": q,
                "acceptedAnswer": { "@type": "Answer", "text": a },
            })
        })
        .collect();
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "FAQPage",
        "mainEntity": main_entity,
    });
    JsonLd::from_value(payload)
}

/// `BreadcrumbList` for a second-level marketing page (Home › page). Gives
/// search engines an explicit Home → page trail for the listing.
pub fn json_ld_breadcrumb(canonical_origin: &str, name: &str, path: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": [
            { "@type": "ListItem", "position": 1, "name": "Home", "item": canonical_origin },
            { "@type": "ListItem", "position": 2, "name": name, "item": format!("{canonical_origin}{path}") },
        ],
    });
    JsonLd::from_value(payload)
}

pub fn json_ld_webpage(
    canonical_origin: &str,
    path: &str,
    name: &str,
    created: &str,
    modified: &str,
) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebPage",
        "name": name,
        "url": format!("{canonical_origin}{path}"),
        "datePublished": created,
        "dateModified": modified,
    });
    JsonLd::from_value(payload)
}

pub fn json_ld_website(canonical_origin: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "@id": format!("{canonical_origin}/#website"),
        "name": BRAND,
        "url": canonical_origin,
        "publisher": { "@id": format!("{canonical_origin}/#organization") },
    });
    JsonLd::from_value(payload)
}

/// `ItemList` for list-format posts ("best X tools"): the ranked items a
/// crawler reads off the article. Names only; the list lives on this URL.
pub fn json_ld_item_list(
    canonical_origin: &str,
    slug: &str,
    name: &str,
    items: &[String],
) -> JsonLd {
    let entries: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            serde_json::json!({
                "@type": "ListItem",
                "position": i + 1,
                "name": item,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "ItemList",
        "name": name,
        "url": format!("{canonical_origin}/blog/{slug}"),
        "itemListElement": entries,
    });
    JsonLd::from_value(payload)
}

pub fn json_ld_blog_posting(
    canonical_origin: &str,
    title: &str,
    excerpt: &str,
    slug: &str,
    date_published: &str,
    date_modified: &str,
    image: &str,
) -> JsonLd {
    let url = format!("{canonical_origin}/blog/{slug}");
    let same_as: Vec<&str> = AUTHOR.same_as.iter().map(|(_, u)| *u).collect();
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": title,
        "description": excerpt,
        "image": image,
        "datePublished": date_published,
        "dateModified": date_modified,
        "mainEntityOfPage": url,
        "author": {
            "@type": "Person",
            "name": AUTHOR.name,
            "url": AUTHOR.url,
            "jobTitle": AUTHOR.role,
            "description": AUTHOR.bio,
            "image": absolute_asset(canonical_origin, &format!("/static/{}", AUTHOR.image)),
            "sameAs": same_as,
        },
        "publisher": publisher(canonical_origin),
    });
    JsonLd::from_value(payload)
}

/// `Blog` listing with its posts as `blogPost` entries — the item list a
/// crawler reads off `/blog`. Pair with `json_ld_breadcrumb` on the page.
pub fn json_ld_blog(canonical_origin: &str, posts: &[(&str, &str, &str)]) -> JsonLd {
    let entries: Vec<_> = posts
        .iter()
        .map(|(title, slug, date)| {
            serde_json::json!({
                "@type": "BlogPosting",
                "headline": title,
                "url": format!("{canonical_origin}/blog/{slug}"),
                "datePublished": date,
                "author": { "@type": "Person", "name": AUTHOR.name, "url": AUTHOR.url },
            })
        })
        .collect();
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Blog",
        "name": format!("{BRAND} Blog"),
        "url": format!("{canonical_origin}/blog"),
        "publisher": publisher(canonical_origin),
        "blogPost": entries,
    });
    JsonLd::from_value(payload)
}

/// `HowTo` for the homepage steps, one `HowToStep` per `(name, text)` pair.
pub fn json_ld_howto(name: &str, steps: &[(&str, &str)]) -> JsonLd {
    let step_list: Vec<_> = steps
        .iter()
        .enumerate()
        .map(|(i, (title, text))| {
            serde_json::json!({
                "@type": "HowToStep",
                "position": i + 1,
                "name": title,
                "text": text,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "HowTo",
        "name": name,
        "step": step_list,
    });
    JsonLd::from_value(payload)
}

/// Logo is a raster: Google's logo guidance needs pixel dimensions an SVG lacks.
fn publisher(canonical_origin: &str) -> serde_json::Value {
    serde_json::json!({
        "@type": "Organization",
        "@id": format!("{canonical_origin}/#organization"),
        "name": BRAND,
        "url": canonical_origin,
        "logo": {
            "@type": "ImageObject",
            "url": absolute_asset(canonical_origin, "/static/img/favicon-512.png"),
            "width": 512,
            "height": 512,
        },
    })
}

static ROBOTS_CACHED: OnceLock<Bytes> = OnceLock::new();
static SITEMAP_CACHED: OnceLock<Bytes> = OnceLock::new();
static LLMS_CACHED: OnceLock<Bytes> = OnceLock::new();
static LLMS_FULL_CACHED: OnceLock<Bytes> = OnceLock::new();

pub async fn robots_txt(State(cfg): State<Arc<MarketingCfg>>) -> Response {
    let body = ROBOTS_CACHED.get_or_init(|| build_robots(&cfg));
    plain_text(body.clone(), TEXT_PLAIN)
}

pub async fn sitemap_xml(State(cfg): State<Arc<MarketingCfg>>) -> Response {
    let body = SITEMAP_CACHED.get_or_init(|| Bytes::from(build_sitemap(&cfg)));
    plain_text(body.clone(), APPLICATION_XML)
}

pub async fn llms_txt(State(cfg): State<Arc<MarketingCfg>>) -> Response {
    let body = LLMS_CACHED.get_or_init(|| build_llms(&cfg));
    plain_text(body.clone(), TEXT_PLAIN)
}

pub async fn llms_full_txt(State(cfg): State<Arc<MarketingCfg>>) -> Response {
    let body = LLMS_FULL_CACHED.get_or_init(|| build_llms_full(&cfg));
    plain_text(body.clone(), TEXT_PLAIN)
}

/// Warm the static text caches at boot. Sitemap is the only non-trivial
/// one (iterates published posts + legal routes); robots/llms are cheap
/// but kept here so every marketing cache lives behind one warmup call.
pub(crate) fn warm(cfg: &MarketingCfg) {
    ROBOTS_CACHED.get_or_init(|| build_robots(cfg));
    SITEMAP_CACHED.get_or_init(|| Bytes::from(build_sitemap(cfg)));
    LLMS_CACHED.get_or_init(|| build_llms(cfg));
    LLMS_FULL_CACHED.get_or_init(|| build_llms_full(cfg));
}

fn build_robots(cfg: &MarketingCfg) -> Bytes {
    Bytes::from(format!(
        "User-agent: *\nAllow: /\nSitemap: {origin}/sitemap.xml\n",
        origin = cfg.canonical_origin
    ))
}

/// Curated index for assistants — the `llms.txt` convention: title,
/// one-line summary, prose overview, then link sections. Built from the
/// same tables that drive the router and sitemap, so it never drifts.
fn build_llms(cfg: &MarketingCfg) -> Bytes {
    let origin = &cfg.canonical_origin;
    let mut s = String::new();
    s.push_str(&format!("# {BRAND}\n\n> {TAGLINE}\n\n{LLMS_OVERVIEW}\n\n"));

    s.push_str("## Product\n");
    s.push_str(&format!(
        "- [Homepage]({origin}): Product overview, features and pricing.\n"
    ));
    s.push_str(&format!(
        "- [Start free]({app}): Sign in and add your first monitor.\n\n",
        app = cfg.app_url,
    ));

    s.push_str("## Use cases\n");
    for l in landings::LANDINGS
        .iter()
        .filter(|l| !l.path.starts_with("/compare/"))
    {
        s.push_str(&format!(
            "- [{title}]({origin}{path}): {desc}\n",
            title = l.title,
            path = l.path,
            desc = l.meta_description,
        ));
    }
    s.push('\n');

    s.push_str("## Comparisons\n");
    for l in landings::LANDINGS
        .iter()
        .filter(|l| l.path.starts_with("/compare/"))
    {
        s.push_str(&format!(
            "- [{title}]({origin}{path}): {desc}\n",
            title = l.title,
            path = l.path,
            desc = l.meta_description,
        ));
    }
    s.push('\n');

    if cfg.blog_enabled {
        let posts = list_published();
        if !posts.is_empty() {
            s.push_str("## Blog\n");
            for p in posts {
                s.push_str(&format!(
                    "- [{title}]({origin}/blog/{slug}): {excerpt}\n",
                    title = p.title,
                    slug = p.slug,
                    excerpt = p.excerpt,
                ));
            }
            s.push('\n');
        }
    }

    s.push_str("## Developers & automation\n");
    s.push_str(&format!(
        "- [MCP server]({MCP_URL}): Connect an LLM client (Claude, IDEs) to read monitors and incidents and take fenced actions. OAuth one-click.\n"
    ));
    s.push_str(&format!(
        "- [Terraform provider]({TERRAFORM_URL}): Manage monitors, status pages and notification channels as config-as-code.\n\n"
    ));

    s.push_str("## Optional\n");
    s.push_str(&format!(
        "- [Full text]({origin}/llms-full.txt): Every marketing page and blog post inlined.\n"
    ));
    for route in legal::ROUTES {
        s.push_str(&format!(
            "- [{name}]({origin}{path})\n",
            name = route.title,
            path = route.path,
        ));
    }

    Bytes::from(s)
}

/// Long-form companion to [`build_llms`]: the overview, a machine-readable
/// facts table, and the full body of every landing page and blog post in
/// one document, so an assistant can answer without fetching each URL.
fn build_llms_full(cfg: &MarketingCfg) -> Bytes {
    let origin = &cfg.canonical_origin;
    let mut s = String::new();
    s.push_str(&format!("# {BRAND}\n\n> {TAGLINE}\n\n{LLMS_OVERVIEW}\n\n"));

    s.push_str("## Facts\n");
    for (k, v) in LLMS_FACTS {
        s.push_str(&format!("- {k}: {v}\n"));
    }
    s.push('\n');

    for l in landings::LANDINGS {
        s.push_str(&format!("---\n\n## {}\n", l.title));
        s.push_str(&format!("URL: {origin}{}\n\n", l.path));
        s.push_str(&format!("{}\n\n{}\n\n", l.h1, l.lede));
        if !l.features.is_empty() {
            s.push_str("What you get:\n");
            for f in l.features {
                s.push_str(&format!("- {}: {}\n", f.label, f.value));
            }
            s.push('\n');
        }
        for sec in l.sections {
            s.push_str(&format!("### {}\n{}\n\n", sec.heading, sec.body));
        }
    }

    if cfg.blog_enabled {
        for p in list_published() {
            s.push_str(&format!("---\n\n## Blog: {}\n", p.title));
            s.push_str(&format!("URL: {origin}/blog/{}\n", p.slug));
            s.push_str(&format!("Date: {}\n", p.date));
            if !p.tags.is_empty() {
                s.push_str(&format!("Tags: {}\n", p.tags.join(", ")));
            }
            s.push_str(&format!("\n{}\n\n", p.body_md));
        }
    }

    Bytes::from(s)
}

fn build_sitemap(cfg: &MarketingCfg) -> String {
    let origin = &cfg.canonical_origin;
    // Only the blog index borrows a date (it changes on publish); the home page
    // and landings have no per-page change tracking, so they omit lastmod rather
    // than borrow an unrelated blog date.
    let blog_lastmod: Option<String> = if cfg.blog_enabled {
        list_published()
            .iter()
            .map(|p| p.updated.clone().unwrap_or_else(|| p.date.clone()))
            .max()
    } else {
        None
    };
    let mut urls: Vec<(String, Option<String>)> = vec![
        (origin.clone(), None),
        (format!("{origin}/pricing"), None),
        (format!("{origin}/blog"), blog_lastmod),
    ];
    if cfg.blog_enabled {
        for post in list_published() {
            urls.push((
                format!("{origin}/blog/{}", post.slug),
                Some(post.updated.clone().unwrap_or_else(|| post.date.clone())),
            ));
        }
    }
    for landing in landings::LANDINGS {
        urls.push((
            format!("{origin}{}", landing.path),
            Some(landing.lastmod.to_string()),
        ));
    }
    for route in legal::ROUTES {
        urls.push((format!("{origin}{}", route.path), None));
    }
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for (loc, lastmod) in urls {
        body.push_str("  <url>\n    <loc>");
        body.push_str(&xml_escape(&loc));
        body.push_str("</loc>\n");
        if let Some(d) = lastmod {
            body.push_str("    <lastmod>");
            body.push_str(&xml_escape(&d));
            body.push_str("</lastmod>\n");
        }
        body.push_str("  </url>\n");
    }
    body.push_str("</urlset>\n");
    body
}

fn plain_text(body: Bytes, content_type: HeaderValue) -> Response {
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, STATIC_CACHE_CONTROL),
        ],
        body,
    )
        .into_response()
}

fn absolute_asset(canonical_origin: &str, path: &str) -> String {
    format!("{canonical_origin}{path}")
}

fn xml_escape(s: &str) -> Cow<'_, str> {
    if !s
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn og_image_is_absolute_https() {
        let og = OpenGraph::default_for("Hi", "https://uptimepage.dev");
        assert!(og.image.starts_with("https://"), "got {}", og.image);
        assert!(og.url.starts_with("https://"));
    }

    #[test]
    fn json_ld_renders_valid_json() {
        let jl = json_ld_organization("https://uptimepage.dev");
        let v: serde_json::Value = serde_json::from_str(jl.as_str()).unwrap();
        assert_eq!(v["@type"], "Organization");
        assert_eq!(v["url"], "https://uptimepage.dev");
    }

    #[test]
    fn json_ld_item_list_orders_positions() {
        let items = vec!["Uptime Kuma".to_string(), "Gatus".to_string()];
        let jl = json_ld_item_list("https://uptimepage.dev", "best-tools", "Best tools", &items);
        let v: serde_json::Value = serde_json::from_str(jl.as_str()).unwrap();
        assert_eq!(v["@type"], "ItemList");
        assert_eq!(v["url"], "https://uptimepage.dev/blog/best-tools");
        assert_eq!(v["itemListElement"][0]["position"], 1);
        assert_eq!(v["itemListElement"][1]["name"], "Gatus");
    }

    #[test]
    fn json_ld_software_application_is_free() {
        let jl = json_ld_software_application("https://uptimepage.dev");
        let v: serde_json::Value = serde_json::from_str(jl.as_str()).unwrap();
        assert_eq!(v["@type"], "SoftwareApplication");
        assert_eq!(v["offers"]["price"], "0");
        assert_eq!(v["offers"]["priceCurrency"], "USD");
    }

    #[test]
    fn json_ld_faqpage_carries_questions() {
        let jl = json_ld_faqpage(&[("Q1?", "A1"), ("Q2?", "A2")]);
        let v: serde_json::Value = serde_json::from_str(jl.as_str()).unwrap();
        assert_eq!(v["@type"], "FAQPage");
        assert_eq!(v["mainEntity"].as_array().unwrap().len(), 2);
        assert_eq!(v["mainEntity"][0]["acceptedAnswer"]["text"], "A1");
    }

    #[test]
    fn json_ld_escapes_script_breakout() {
        let ld = json_ld_blog(
            "https://x.test",
            &[("</script><script>alert(1)</script>", "s", "2026-01-01")],
        );
        let out = ld.as_str();
        assert!(!out.contains("</script>"), "raw </script> leaked: {out}");
        assert!(out.contains("\\u003c"), "expected escaped <, got: {out}");
        let v: serde_json::Value = serde_json::from_str(out).expect("still valid JSON");
        assert_eq!(v["@type"], "Blog");
        assert_eq!(
            v["blogPost"][0]["headline"],
            "</script><script>alert(1)</script>"
        );
    }

    #[test]
    fn xml_escape_handles_ampersand() {
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }

    #[test]
    fn xml_escape_borrows_when_clean() {
        let s = "no-escapes-needed";
        assert!(matches!(xml_escape(s), Cow::Borrowed(_)));
    }
}
