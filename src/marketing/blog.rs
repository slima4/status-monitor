//! Build-time compiled blog. Posts under `src/marketing/content/blog/`
//! are read at compile time via `include_dir!`, parsed and rendered to
//! sanitised HTML once at startup, and held in a `OnceLock<Vec<Post>>`
//! along with their fully-rendered page bodies + ETags.
//!
//! The Markdown renderer is vendored locally on purpose — blog content
//! arrives as third-party PR input, so every byte goes through
//! `ammonia::clean` before reaching a template. The legal-page renderer
//! is deliberately unsanitised (first-party tables) and must not be
//! reused here.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::response::Response;
use include_dir::{Dir, include_dir};
use serde::Deserialize;

use super::config::{AUTHOR, Author, BRAND, MarketingCfg};
use super::pages::{CachedRender, cached_render, not_found, serve_cached};
use super::seo::{
    JsonLd, OpenGraph, json_ld_blog, json_ld_blog_posting, json_ld_breadcrumb, json_ld_item_list,
};
use crate::web::filters;

const POST_CACHE_CONTROL: HeaderValue =
    HeaderValue::from_static("public, max-age=600, stale-while-revalidate=86400");
const INDEX_CACHE_CONTROL: HeaderValue =
    HeaderValue::from_static("public, max-age=300, stale-while-revalidate=86400");

static BLOG_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/marketing/content/blog");

#[derive(Debug, Clone)]
pub struct Post {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub updated: Option<String>,
    pub excerpt: String,
    pub tags: Vec<String>,
    pub draft: bool,
    /// Ranked item names for list-format posts; emits `ItemList` JSON-LD.
    pub list_items: Vec<String>,
    pub body_html: String,
    /// Source markdown, pre-render — inlined verbatim into `llms-full.txt`.
    pub body_md: String,
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    title: String,
    date: String,
    updated: Option<String>,
    slug: Option<String>,
    #[serde(default)]
    excerpt: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    list_items: Vec<String>,
}

static POSTS: OnceLock<Vec<Post>> = OnceLock::new();
static RENDERED_INDEX: OnceLock<HashMap<String, CachedRender>> = OnceLock::new();
static INDEX_CACHED: OnceLock<CachedRender> = OnceLock::new();

/// Parse + render every published post once. Idempotent; the renders
/// land in static caches the request handlers read directly.
pub fn init() -> &'static [Post] {
    POSTS.get_or_init(load_posts).as_slice()
}

pub fn all() -> &'static [Post] {
    init()
}

pub fn list_published() -> Vec<&'static Post> {
    let drafts_visible = cfg!(debug_assertions);
    init()
        .iter()
        .filter(|p| drafts_visible || !p.draft)
        .collect()
}

/// Warm post-load + index + per-post render caches at boot. Cheap
/// relative to the cold-first-request penalty (parse + sanitise +
/// askama + SHA-256 on every published post).
pub(crate) fn warm(cfg: &MarketingCfg) {
    init();
    INDEX_CACHED.get_or_init(|| render_index(cfg));
    RENDERED_INDEX.get_or_init(|| build_post_index(cfg));
}

fn build_post_index(cfg: &MarketingCfg) -> HashMap<String, CachedRender> {
    list_published()
        .into_iter()
        .map(|p| (p.slug.clone(), render_post(cfg, p)))
        .collect()
}

fn load_posts() -> Vec<Post> {
    let mut out: Vec<Post> = Vec::new();
    for f in BLOG_DIR.files() {
        let path = f.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let raw = match f.contents_utf8() {
            Some(s) => s,
            None => continue,
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("post")
            .to_string();
        match parse_post(raw, &stem) {
            Ok(post) => out.push(post),
            Err(e) => {
                tracing::error!(file = %path.display(), error = %e, "blog post parse failed");
            }
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

fn parse_post(raw: &str, stem: &str) -> anyhow::Result<Post> {
    let (front, body) = split_front_matter(raw)
        .ok_or_else(|| anyhow::anyhow!("missing TOML front-matter block"))?;
    let fm: FrontMatter = toml::from_str(front)?;
    Ok(Post {
        slug: fm.slug.unwrap_or_else(|| stem.to_string()),
        title: fm.title,
        date: fm.date,
        updated: fm.updated,
        excerpt: fm.excerpt,
        tags: fm.tags,
        draft: fm.draft,
        list_items: fm.list_items,
        body_html: render(body),
        body_md: body.trim().to_string(),
    })
}

fn split_front_matter(raw: &str) -> Option<(&str, &str)> {
    let stripped = raw.strip_prefix("+++\n")?;
    let end = stripped.find("\n+++\n")?;
    let front = &stripped[..end];
    let body = &stripped[end + "\n+++\n".len()..];
    Some((front, body))
}

/// Sanitising Markdown render. CommonMark + tables → HTML → ammonia
/// allowlist. Third-party PR content never reaches a browser as raw
/// HTML; every `<script>`, `onerror=`, and `javascript:` href is
/// dropped before the bytes leave this function.
pub fn render(markdown: &str) -> String {
    let mut opts = pulldown_cmark::Options::empty();
    opts.insert(pulldown_cmark::Options::ENABLE_TABLES);
    opts.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(markdown, opts);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .clean(&html)
        .to_string()
}

#[derive(Template, WebTemplate)]
#[template(path = "marketing/blog_index.html")]
struct BlogIndexPage {
    canonical_url: String,
    app_url: String,
    og: OpenGraph,
    blog_ld: JsonLd,
    breadcrumb_ld: JsonLd,
    posts: Vec<PostSummary>,
    version: &'static str,
}

#[derive(Debug, Clone)]
pub struct PostSummary {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub excerpt: String,
    pub tags: Vec<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "marketing/blog_post.html")]
struct BlogPostPage {
    canonical_url: String,
    app_url: String,
    og: OpenGraph,
    json_ld: JsonLd,
    item_list_ld: Option<JsonLd>,
    title: String,
    date: String,
    updated: Option<String>,
    author: &'static Author,
    tags: Vec<String>,
    body_html: String,
    version: &'static str,
}

fn render_index(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = format!("{}/blog", cfg.canonical_origin);
    let og = OpenGraph::default_for(&format!("{BRAND} Blog"), &canonical_url);
    let published = list_published();
    let ld_posts: Vec<(&str, &str, &str)> = published
        .iter()
        .map(|p| (p.title.as_str(), p.slug.as_str(), p.date.as_str()))
        .collect();
    let blog_ld = json_ld_blog(&cfg.canonical_origin, &ld_posts);
    let breadcrumb_ld = json_ld_breadcrumb(&cfg.canonical_origin, "Blog", "/blog");
    let posts: Vec<PostSummary> = published
        .into_iter()
        .map(|p| PostSummary {
            slug: p.slug.clone(),
            title: p.title.clone(),
            date: p.date.clone(),
            excerpt: p.excerpt.clone(),
            tags: p.tags.clone(),
        })
        .collect();
    let body = BlogIndexPage {
        canonical_url,
        app_url: cfg.app_url.clone(),
        og,
        blog_ld,
        breadcrumb_ld,
        posts,
        version: env!("CARGO_PKG_VERSION"),
    }
    .render()
    .unwrap_or_else(|e| format!("<!-- blog index render failed: {e} -->"));
    cached_render(body)
}

fn render_post(cfg: &MarketingCfg, post: &Post) -> CachedRender {
    let canonical_url = format!("{}/blog/{}", cfg.canonical_origin, post.slug);
    let og = OpenGraph::for_post(
        &cfg.canonical_origin,
        &post.title,
        &post.excerpt,
        &post.slug,
    );
    let date_modified = post.updated.as_deref().unwrap_or(&post.date);
    let json_ld = json_ld_blog_posting(
        &cfg.canonical_origin,
        &post.title,
        &post.excerpt,
        &post.slug,
        &post.date,
        date_modified,
        &og.image,
    );
    let item_list_ld = (!post.list_items.is_empty()).then(|| {
        json_ld_item_list(
            &cfg.canonical_origin,
            &post.slug,
            &post.title,
            &post.list_items,
        )
    });
    let body = BlogPostPage {
        canonical_url,
        app_url: cfg.app_url.clone(),
        og,
        json_ld,
        item_list_ld,
        title: post.title.clone(),
        date: post.date.clone(),
        updated: post.updated.clone().filter(|u| u != &post.date),
        author: &AUTHOR,
        tags: post.tags.clone(),
        body_html: post.body_html.clone(),
        version: env!("CARGO_PKG_VERSION"),
    }
    .render()
    .unwrap_or_else(|e| format!("<!-- blog post render failed: {e} -->"));
    cached_render(body)
}

pub async fn index(State(cfg): State<Arc<MarketingCfg>>, headers: HeaderMap) -> Response {
    let cached = INDEX_CACHED.get_or_init(|| render_index(&cfg));
    serve_cached(&headers, cached, &INDEX_CACHE_CONTROL)
}

pub async fn post(
    State(cfg): State<Arc<MarketingCfg>>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Response {
    let cache = RENDERED_INDEX.get_or_init(|| build_post_index(&cfg));
    match cache.get(&slug) {
        Some(cached) => serve_cached(&headers, cached, &POST_CACHE_CONTROL),
        None => not_found(State(cfg)).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_items_appear_in_post_body() {
        for post in load_posts() {
            for item in &post.list_items {
                assert!(
                    post.body_md.contains(item.as_str()),
                    "{}: ItemList entry {item:?} not found in the article body",
                    post.slug
                );
            }
        }
    }

    #[test]
    fn renderer_strips_script_tag() {
        let html = render("<script>alert(1)</script>");
        assert!(!html.contains("<script"), "got: {html}");
        assert!(!html.to_lowercase().contains("alert"));
    }

    #[test]
    fn renderer_strips_onerror() {
        let html = render("![x](javascript:1)\n\n<img src=x onerror=alert(1)>");
        assert!(!html.contains("onerror"), "got: {html}");
        assert!(!html.contains("javascript:"), "got: {html}");
    }

    #[test]
    fn renderer_strips_iframe_and_svg_onload() {
        let html = render("<iframe src=//evil></iframe>\n<svg onload=alert(1)></svg>");
        assert!(!html.contains("<iframe"), "got: {html}");
        assert!(!html.contains("onload"), "got: {html}");
    }

    #[test]
    fn renderer_keeps_safe_link() {
        let html = render("[hi](https://example.com)");
        assert!(html.contains("href=\"https://example.com"));
        assert!(html.contains("rel=\"noopener noreferrer\""));
    }

    #[test]
    fn split_front_matter_extracts_block() {
        let raw = "+++\ntitle = \"x\"\ndate = \"2026-05-20\"\n+++\nbody\n";
        let (front, body) = split_front_matter(raw).expect("front matter");
        assert!(front.contains("title"));
        assert!(body.starts_with("body"));
    }

    #[test]
    fn parse_post_succeeds_on_minimal_doc() {
        let raw = "+++\ntitle = \"Hi\"\ndate = \"2026-05-20\"\n+++\nbody\n";
        let post = parse_post(raw, "hi").expect("parse");
        assert_eq!(post.slug, "hi");
        assert_eq!(post.title, "Hi");
        assert!(!post.draft);
    }

    #[test]
    fn posts_load_and_sort_by_date_desc() {
        let posts = all();
        if posts.is_empty() {
            return;
        }
        for w in posts.windows(2) {
            assert!(w[0].date >= w[1].date, "blog index must be date-desc");
        }
    }
}
