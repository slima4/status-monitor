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
    JsonLd, OpenGraph, json_ld_blog, json_ld_blog_posting, json_ld_breadcrumb, json_ld_faqpage,
    json_ld_item_list,
};
use crate::web::filters;

const RELATED_LIMIT: usize = 3;
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
    /// Question/answer pairs mirroring the post's visible FAQ; emits
    /// `FAQPage` JSON-LD.
    pub faqs: Vec<(String, String)>,
    /// Origin-relative path to a post-specific social card; the shared
    /// site card is used when absent.
    pub og_image: Option<String>,
    pub body_html: String,
    /// Source markdown, pre-render — inlined verbatim into `llms-full.txt`.
    pub body_md: String,
    /// Local images the post renders, for the image sitemap.
    pub images: Vec<PostImage>,
}

#[derive(Debug, Clone)]
pub struct PostImage {
    /// Unfingerprinted: a path the page doesn't render indexes as a second image.
    pub path: String,
    pub alt: String,
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
    #[serde(default)]
    faqs: Vec<FaqEntry>,
    og_image: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FaqEntry {
    q: String,
    a: String,
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
    if let Some(img) = &fm.og_image {
        anyhow::ensure!(
            img.starts_with("/static/"),
            "og_image must be a /static/-rooted path, got {img:?}"
        );
    }
    Ok(Post {
        slug: fm.slug.unwrap_or_else(|| stem.to_string()),
        title: fm.title,
        date: fm.date,
        updated: fm.updated,
        excerpt: fm.excerpt,
        tags: fm.tags,
        draft: fm.draft,
        list_items: fm.list_items,
        faqs: fm.faqs.into_iter().map(|f| (f.q, f.a)).collect(),
        og_image: fm.og_image,
        body_html: render(body),
        body_md: body.trim().to_string(),
        images: collect_images(body),
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
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, super::md::wrap_tables(parser(markdown)));
    ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .add_allowed_classes("div", &["mk-table-scroll"])
        .add_tag_attributes("div", &["tabindex"])
        .clean(&html)
        .to_string()
}

/// Shared so an extension can't apply to the render but not the sitemap.
fn parser(markdown: &str) -> pulldown_cmark::Parser<'_> {
    let mut opts = pulldown_cmark::Options::empty();
    opts.insert(pulldown_cmark::Options::ENABLE_TABLES);
    opts.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    pulldown_cmark::Parser::new_ext(markdown, opts)
}

/// Mirrors pulldown-cmark's `raw_text`, which fills the rendered `alt`:
/// everything up to the matching close folds into one string, so a nested
/// image belongs to its parent's alt rather than standing on its own.
/// Remote images are not ours to submit; alt-less ones have nothing to say.
fn collect_images(markdown: &str) -> Vec<PostImage> {
    use pulldown_cmark::{Event, Tag};

    let mut out: Vec<PostImage> = Vec::new();
    let mut open: Option<(String, String, usize)> = None;
    for ev in parser(markdown) {
        let Some((_, alt, depth)) = open.as_mut() else {
            if let Event::Start(Tag::Image { dest_url, .. }) = ev {
                open = Some((dest_url.into_string(), String::new(), 0));
            }
            continue;
        };
        match ev {
            Event::Start(_) => *depth += 1,
            Event::End(_) if *depth > 0 => *depth -= 1,
            Event::End(_) => {
                let (path, alt, _) = open.take().expect("matched above");
                let alt = alt.trim().to_string();
                if path.starts_with("/static/") && !alt.is_empty() {
                    out.push(PostImage { path, alt });
                }
            }
            Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) => alt.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::Rule => alt.push(' '),
            _ => {}
        }
    }
    out
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
    slug: String,
    canonical_url: String,
    app_url: String,
    og: OpenGraph,
    json_ld: JsonLd,
    item_list_ld: Option<JsonLd>,
    faq_ld: Option<JsonLd>,
    title: String,
    date: String,
    updated: Option<String>,
    author: &'static Author,
    tags: Vec<String>,
    body_html: String,
    related: Vec<RelatedLink>,
    version: &'static str,
}

fn render_index(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = format!("{}/blog", cfg.canonical_origin);
    let og = OpenGraph::default_for(
        &format!("{BRAND} Blog"),
        &canonical_url,
        &cfg.canonical_origin,
    );
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

/// A cross-link surfaced beneath a post to keep readers on-site and
/// spread internal link equity.
#[derive(Debug, Clone)]
pub struct RelatedLink {
    pub slug: String,
    pub title: String,
    pub date: String,
}

/// Rank other published posts by shared-tag overlap, newest first as the
/// tiebreak, backfilling with recent posts so every article links out
/// even when its tags are unique.
fn related_posts(post: &Post, limit: usize) -> Vec<RelatedLink> {
    let mut scored: Vec<(usize, &'static Post)> = list_published()
        .into_iter()
        .filter(|p| p.slug != post.slug)
        .map(|p| {
            let overlap = p.tags.iter().filter(|t| post.tags.contains(t)).count();
            (overlap, p)
        })
        .collect();
    scored.sort_by_key(|&(overlap, _)| std::cmp::Reverse(overlap));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| RelatedLink {
            slug: p.slug.clone(),
            title: p.title.clone(),
            date: p.date.clone(),
        })
        .collect()
}

fn render_post(cfg: &MarketingCfg, post: &Post) -> CachedRender {
    let canonical_url = format!("{}/blog/{}", cfg.canonical_origin, post.slug);
    let og = OpenGraph::for_post(
        &cfg.canonical_origin,
        &post.title,
        &post.excerpt,
        &post.slug,
        post.og_image.as_deref(),
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
    let faq_ld = (!post.faqs.is_empty()).then(|| {
        let pairs: Vec<(&str, &str)> = post
            .faqs
            .iter()
            .map(|(q, a)| (q.as_str(), a.as_str()))
            .collect();
        json_ld_faqpage(&pairs)
    });
    let body = BlogPostPage {
        slug: post.slug.clone(),
        canonical_url,
        app_url: cfg.app_url.clone(),
        og,
        json_ld,
        item_list_ld,
        faq_ld,
        title: post.title.clone(),
        date: post.date.clone(),
        updated: post.updated.clone().filter(|u| u != &post.date),
        author: &AUTHOR,
        tags: post.tags.clone(),
        body_html: post.body_html.clone(),
        related: related_posts(post, RELATED_LIMIT),
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

    /// Asserts against the rendered `alt`, the only description that counts.
    #[track_caller]
    fn assert_alt_matches_render(markdown: &str, expected: &str) {
        let images = collect_images(markdown);
        assert_eq!(images.len(), 1, "{markdown:?} -> {images:?}");
        assert_eq!(images[0].alt, expected);
        assert!(
            render(markdown).contains(&format!("alt=\"{expected}\"")),
            "rendered alt disagrees: {}",
            render(markdown)
        );
    }

    #[test]
    fn image_alt_folds_a_line_break_into_a_space() {
        assert_alt_matches_render("![line one\nline two](/static/a.webp)", "line one line two");
    }

    #[test]
    fn image_alt_drops_strikethrough_markers() {
        assert_alt_matches_render("![before ~~after~~](/static/a.webp)", "before after");
    }

    #[test]
    fn nested_image_folds_into_its_parents_alt() {
        let images = collect_images("![outer ![inner](/static/b.webp) tail](/static/a.webp)");
        assert_eq!(
            images.len(),
            1,
            "the inner image is alt text, not an image of its own: {images:?}"
        );
        assert_eq!(images[0].path, "/static/a.webp");
        assert_eq!(images[0].alt, "outer inner tail");
    }

    #[test]
    fn collect_images_skips_remote_and_alt_less_images() {
        let images = collect_images(
            "![hosted elsewhere](https://example.com/a.webp)\n\n![](/static/b.webp)\n\n![kept](/static/c.webp)",
        );
        let paths: Vec<&str> = images.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["/static/c.webp"]);
    }

    #[test]
    fn og_image_assets_exist_at_social_card_size() {
        for post in load_posts() {
            let Some(img) = &post.og_image else { continue };
            let rel = img.strip_prefix('/').unwrap();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{}: og_image {img} unreadable: {e}", post.slug));
            assert_eq!(
                &bytes[..8],
                b"\x89PNG\r\n\x1a\n",
                "{}: og_image must be a PNG",
                post.slug
            );
            let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            assert_eq!((w, h), (1200, 630), "{}: social card size", post.slug);
        }
    }

    #[test]
    fn og_image_outside_static_is_rejected() {
        let raw = "+++\ntitle = \"x\"\ndate = \"2026-01-01\"\nog_image = \"https://cdn.example.com/x.png\"\n+++\nbody";
        assert!(parse_post(raw, "x").is_err());
    }

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
    fn faq_front_matter_mirrors_visible_answers() {
        for post in load_posts() {
            for (q, a) in &post.faqs {
                assert!(!q.is_empty() && !a.is_empty(), "{}: empty FAQ", post.slug);
                // Answers may flatten inline links, so match the opening
                // sentence rather than the whole string: catches drift
                // between schema and the visible copy without false negatives.
                let lead = a.split(['.', '?', '!']).next().unwrap_or(a).trim();
                assert!(
                    post.body_md.contains(lead),
                    "{}: FAQ answer {q:?} does not track the visible copy; \
                     schema would mismatch the page",
                    post.slug
                );
            }
        }
    }

    #[test]
    fn renderer_wraps_tables_in_scroll_div() {
        let html = render("| a |\n| - |\n| 1 |");
        let open = html.find("mk-table-scroll");
        assert!(html.contains("tabindex"), "got: {html}");
        let table_end = html.find("</table>");
        assert!(open.is_some() && table_end.is_some(), "got: {html}");
        assert!(open < table_end, "got: {html}");
        assert!(html[table_end.unwrap()..].contains("</div>"), "got: {html}");
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
    fn posts_fit_serp_limits() {
        for p in all() {
            assert!(
                p.title.len() <= 65,
                "{}: title {} chars > 65",
                p.slug,
                p.title.len()
            );
            assert!(
                p.excerpt.len() <= 160,
                "{}: excerpt {} chars > 160",
                p.slug,
                p.excerpt.len()
            );
        }
    }

    #[test]
    fn every_post_links_out_to_related() {
        let posts = list_published();
        let others = posts.len().saturating_sub(1);
        if others == 0 {
            return;
        }
        let want = RELATED_LIMIT.min(others);
        for p in &posts {
            let related = related_posts(p, RELATED_LIMIT);
            assert_eq!(
                related.len(),
                want,
                "{}: expected {want} related links, got {}",
                p.slug,
                related.len()
            );
            assert!(
                related.iter().all(|r| r.slug != p.slug),
                "{}: related links must not include the post itself",
                p.slug
            );
        }
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
