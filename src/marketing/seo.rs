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
use super::config::{BRAND, META_DESCRIPTION, MarketingCfg, TAGLINE};
use super::legal;
use super::pages::{APPLICATION_XML, TEXT_PLAIN};

const STATIC_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("public, max-age=86400");

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
pub struct JsonLd(pub String);

impl JsonLd {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn json_ld_organization(canonical_origin: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Organization",
        "name": BRAND,
        "url": canonical_origin,
        "logo": absolute_asset(canonical_origin, "/static/img/favicon.svg"),
    });
    JsonLd(payload.to_string())
}

pub fn json_ld_website(canonical_origin: &str) -> JsonLd {
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "name": BRAND,
        "url": canonical_origin,
    });
    JsonLd(payload.to_string())
}

pub fn json_ld_blog_posting(
    canonical_origin: &str,
    title: &str,
    excerpt: &str,
    slug: &str,
    date_iso: &str,
) -> JsonLd {
    let url = format!("{canonical_origin}/blog/{slug}");
    let payload = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "BlogPosting",
        "headline": title,
        "description": excerpt,
        "datePublished": date_iso,
        "mainEntityOfPage": url,
        "author": {
            "@type": "Organization",
            "name": BRAND,
            "url": canonical_origin,
        },
    });
    JsonLd(payload.to_string())
}

static ROBOTS_CACHED: OnceLock<Bytes> = OnceLock::new();
static SITEMAP_CACHED: OnceLock<Bytes> = OnceLock::new();
static LLMS_CACHED: OnceLock<Bytes> = OnceLock::new();

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

/// Warm the static text caches at boot. Sitemap is the only non-trivial
/// one (iterates published posts + legal routes); robots/llms are cheap
/// but kept here so every marketing cache lives behind one warmup call.
pub(crate) fn warm(cfg: &MarketingCfg) {
    ROBOTS_CACHED.get_or_init(|| build_robots(cfg));
    SITEMAP_CACHED.get_or_init(|| Bytes::from(build_sitemap(cfg)));
    LLMS_CACHED.get_or_init(|| build_llms(cfg));
}

fn build_robots(cfg: &MarketingCfg) -> Bytes {
    Bytes::from(format!(
        "User-agent: *\nAllow: /\nSitemap: {origin}/sitemap.xml\n",
        origin = cfg.canonical_origin
    ))
}

fn build_llms(cfg: &MarketingCfg) -> Bytes {
    Bytes::from(format!(
        "# {brand}\n\n> {tagline}\n\nHomepage: {origin}\nBlog: {origin}/blog\nApp: {app}\n",
        brand = BRAND,
        tagline = TAGLINE,
        origin = cfg.canonical_origin,
        app = cfg.app_url,
    ))
}

fn build_sitemap(cfg: &MarketingCfg) -> String {
    let origin = &cfg.canonical_origin;
    let mut urls: Vec<(String, Option<String>)> =
        vec![(origin.clone(), None), (format!("{origin}/blog"), None)];
    if cfg.blog_enabled {
        for post in list_published() {
            urls.push((
                format!("{origin}/blog/{}", post.slug),
                Some(post.date.clone()),
            ));
        }
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
    fn xml_escape_handles_ampersand() {
        assert_eq!(xml_escape("a&b<c>\"d"), "a&amp;b&lt;c&gt;&quot;d");
    }

    #[test]
    fn xml_escape_borrows_when_clean() {
        let s = "no-escapes-needed";
        assert!(matches!(xml_escape(s), Cow::Borrowed(_)));
    }
}
