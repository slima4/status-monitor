//! The load-bearing decoupling check: every marketing route serves 2xx
//! when no Postgres / ClickHouse handle is in scope. The marketing
//! module takes its own `MarketingCfg` (not `AppState`), so this test
//! constructs that config directly and exercises each route through the
//! returned `axum::Router` — no pool, no client, no `AppState`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::util::ServiceExt;

use uptimepage::marketing::{self, MarketingCfg, landings};

fn router() -> axum::Router {
    marketing::router(MarketingCfg {
        app_url: "https://app.uptimepage.dev".into(),
        canonical_origin: "https://uptimepage.dev".into(),
        blog_enabled: true,
        mcp_url: Some("https://mcp.uptimepage.dev/mcp".into()),
    })
}

async fn get(path: &str) -> (StatusCode, String, axum::http::HeaderMap) {
    let resp = router()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::HOST, "uptimepage.dev")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("collect body");
    let body = String::from_utf8(bytes.to_vec()).unwrap_or_default();
    (status, body, headers)
}

#[tokio::test]
async fn landing_renders_without_db() {
    let (status, body, headers) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Uptimepage"));
    assert!(
        body.contains("https://app.uptimepage.dev/login"),
        "CTA should link to app_url"
    );
    let cache_control = headers
        .get(header::CACHE_CONTROL)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(
        cache_control.contains("max-age="),
        "marketing landing must set Cache-Control, got {cache_control:?}"
    );
    assert!(
        headers.contains_key(header::ETAG),
        "marketing landing must set a strong ETag"
    );
}

#[tokio::test]
async fn pricing_renders_without_db() {
    let (status, body, headers) = get("/pricing").await;
    assert_eq!(status, StatusCode::OK);
    // Real content, not the render-failure comment fallback served as a 200.
    assert!(body.contains("founding"), "pricing must render the tiers");
    assert!(
        body.contains("1,000"),
        "founding total should render with a thousands separator"
    );
    assert!(
        body.contains("https://app.uptimepage.dev/login"),
        "pricing CTA should link to app_url"
    );
    assert!(
        !body.contains("render failed"),
        "pricing render must not fall back to the error comment"
    );
    assert!(
        headers.contains_key(header::ETAG),
        "pricing must set a strong ETag"
    );
}

#[tokio::test]
async fn architecture_page_renders_and_is_csp_clean() {
    let (status, body, headers) = get("/architecture").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("How Uptimepage is built"),
        "architecture page must render its hero"
    );
    assert!(
        body.contains(r#"id="cols""#) && body.contains(r#"id="wires""#),
        "architecture page must render the map skeleton the script populates"
    );
    assert!(
        body.contains("/static/js/architecture/flows.js")
            && body.contains("/static/architecture/flows.css"),
        "map styles and script must be external assets, not inline"
    );
    // The prod marketing CSP forbids inline styles and scripts. Guard it here
    // so a future edit that inlines either fails the build, not production.
    assert!(
        !body.contains("<script>"),
        "no inline script (CSP: script-src 'self')"
    );
    assert!(
        !body.contains("<style"),
        "no inline style block (CSP: style-src 'self')"
    );
    assert!(
        !body.contains("style="),
        "no inline style attributes (CSP: style-src 'self')"
    );
    assert!(
        body.contains(
            r#"property="og:image" content="https://uptimepage.dev/static/marketing/og-architecture.png""#
        ),
        "og:image must be the dedicated architecture card, rooted at the origin"
    );
    assert!(
        body.contains(r#"href="https://app.uptimepage.dev""#),
        "the Start free CTA must link to app_url"
    );
    assert!(
        !body.contains("render failed"),
        "must not serve the render-error fallback"
    );
    assert!(
        headers.contains_key(header::ETAG),
        "architecture page must set a strong ETag"
    );
}

#[tokio::test]
async fn blog_index_renders_without_db() {
    let (status, body, _) = get("/blog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("from the workshop"));
}

#[tokio::test]
async fn known_post_renders_without_db() {
    let (status, body, _) = get("/blog/boring-uptime").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Why your uptime monitor should be boring"));
}

#[tokio::test]
async fn post_with_a_figure_loads_only_its_own_script() {
    let (status, body, _) = get("/blog/stop-false-uptime-alerts").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"class="mk-embed-quorum""#),
        "the figure's mount must survive sanitising all the way to the page"
    );
    assert!(
        body.contains("/static/js/marketing/quorum.js"),
        "a post that embeds the figure must load the script that fills it"
    );

    let (_, other, _) = get("/blog/boring-uptime").await;
    assert!(
        !other.contains("quorum.js"),
        "a post without the figure must not pay for its script"
    );
}

#[tokio::test]
async fn landing_with_figures_mounts_them_and_loads_only_its_own_scripts() {
    let (status, body, _) = get("/browser-login-monitoring").await;
    assert_eq!(status, StatusCode::OK);
    for mount in [
        "mk-embed-flow-gap",
        "mk-embed-flow-record",
        "mk-embed-flow-evidence",
    ] {
        assert!(
            body.contains(mount),
            "{mount} must reach the page as a mount"
        );
    }
    for script in ["flow_gap.js", "flow_record.js", "flow_evidence.js"] {
        assert!(
            body.contains(script),
            "a page mounting a figure must load the script that fills it: {script}"
        );
    }

    let (_, other, _) = get("/uptime-monitoring-for-developers").await;
    assert!(
        !other.contains("mk-embed-flow-"),
        "a landing without figures must not pay for their scripts"
    );
}

#[tokio::test]
async fn unknown_blog_post_returns_branded_404() {
    let (status, body, _) = get("/blog/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Not Found"));
}

#[tokio::test]
async fn arbitrary_path_returns_branded_404() {
    let (status, body, _) = get("/this-page-does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Not Found"));
}

#[tokio::test]
async fn robots_txt_points_at_sitemap() {
    let (status, body, headers) = get("/robots.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("User-agent: *"));
    assert!(body.contains("https://uptimepage.dev/sitemap.xml"));
    assert!(
        body.contains("Content-Signal: search=yes, ai-input=yes, ai-train=yes"),
        "AI usage preferences must be declared, got {body:?}"
    );
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/plain"), "got {ct:?}");
}

async fn get_as_markdown(path: &str) -> (StatusCode, String, axum::http::HeaderMap) {
    let resp = router()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::HOST, "uptimepage.dev")
                .header(header::ACCEPT, "text/markdown")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("collect body");
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap_or_default(),
        headers,
    )
}

#[tokio::test]
async fn agents_get_the_markdown_source_of_a_doc() {
    let (status, body, headers) = get_as_markdown("/docs/api").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/markdown; charset=utf-8"
    );
    assert_eq!(headers.get(header::VARY).unwrap(), "accept");
    assert!(headers.contains_key("x-markdown-tokens"));
    assert!(
        body.starts_with("# REST API"),
        "got {:?}",
        &body[..40.min(body.len())]
    );
    assert!(!body.contains("<html"), "must not be the rendered page");
}

#[tokio::test]
async fn agents_get_markdown_for_posts_and_the_landing_page() {
    let (_, post, _) = get_as_markdown("/blog/boring-uptime").await;
    assert!(post.starts_with("# Why your uptime monitor should be boring"));

    let (_, landing, _) = get_as_markdown("/").await;
    assert!(landing.starts_with("# Uptimepage"));
}

#[tokio::test]
async fn browsers_still_get_html_and_a_vary_header() {
    let (status, body, headers) = get("/docs/api").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<html"), "browsers keep the rendered page");
    assert_eq!(
        headers.get(header::VARY).unwrap(),
        "accept",
        "caches must key both representations"
    );
    assert!(!headers.contains_key("x-markdown-tokens"));
}

#[tokio::test]
async fn compression_does_not_drop_the_accept_vary() {
    let resp = router()
        .oneshot(
            Request::builder()
                .uri("/docs/api")
                .header(header::HOST, "uptimepage.dev")
                .header(header::ACCEPT, "text/markdown")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    let vary: Vec<_> = resp
        .headers()
        .get_all(header::VARY)
        .iter()
        .map(|v| v.to_str().unwrap().to_ascii_lowercase())
        .collect();
    assert!(
        vary.iter()
            .any(|v| v.contains("accept") && !v.contains("accept-encoding")
                || v.split(',').any(|p| p.trim() == "accept")),
        "Accept dropped from Vary: {vary:?}"
    );
}

#[tokio::test]
async fn api_catalog_is_served_as_a_linkset() {
    let (status, body, headers) = get("/.well-known/api-catalog").await;
    assert_eq!(status, StatusCode::OK);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/linkset+json"), "got {ct:?}");
    assert!(ct.contains("rfc9727"), "profile parameter missing: {ct:?}");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("linkset json");
    assert!(doc["linkset"][0]["item"].is_array());
}

#[tokio::test]
async fn apex_points_at_the_mcp_hosts_server_card() {
    let (status, _, headers) = get("/.well-known/mcp/server-card.json").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        headers.get(header::LOCATION).unwrap(),
        "https://mcp.uptimepage.dev/.well-known/mcp/server-card.json"
    );
}

#[tokio::test]
async fn pages_advertise_the_catalog_over_link() {
    let (_, _, headers) = get("/").await;
    let link = headers
        .get(header::LINK)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    assert!(
        link.contains("<https://uptimepage.dev/.well-known/api-catalog>; rel=\"api-catalog\""),
        "got {link:?}"
    );
    assert!(link.contains("rel=\"service-desc\""), "got {link:?}");
    assert!(link.contains("rel=\"service-doc\""), "got {link:?}");

    let (_, _, docs) = get("/docs/api").await;
    assert!(docs.contains_key(header::LINK), "doc pages need it too");
}

#[tokio::test]
async fn sitemap_lists_blog_and_landing() {
    let (status, body, headers) = get("/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/xml"), "got {ct:?}");
    assert!(body.contains("<urlset"), "sitemap must be a urlset");
    assert!(
        body.contains("<loc>https://uptimepage.dev</loc>"),
        "landing must be in sitemap"
    );
    assert!(
        body.contains("<loc>https://uptimepage.dev/blog</loc>"),
        "blog index must be in sitemap"
    );
    assert!(
        body.contains("<loc>https://uptimepage.dev/pricing</loc>"),
        "pricing page must be in sitemap"
    );
    assert!(
        body.contains("<loc>https://uptimepage.dev/blog/boring-uptime</loc>"),
        "published post must be in sitemap"
    );
}

#[tokio::test]
async fn llms_txt_renders() {
    let (status, body, _) = get("/llms.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("# Uptimepage"));
    assert!(body.contains("## Use cases"), "must list landing pages");
    assert!(
        body.contains("https://uptimepage.dev/status-page-for-saas"),
        "must link a landing page"
    );
    assert!(
        body.contains("https://uptimepage.dev/llms-full.txt"),
        "must point to the full-text companion"
    );
    assert!(
        body.contains("https://mcp.uptimepage.dev/mcp"),
        "must surface the MCP server"
    );
    assert!(
        body.contains("registry.terraform.io/providers/uptimepage/uptimepage"),
        "must surface the Terraform provider"
    );
}

#[tokio::test]
async fn uptime_sla_tool_renders_without_db() {
    let (status, body, headers) = get("/tools/uptime-sla-calculator").await;
    assert_eq!(status, StatusCode::OK);
    // Server-rendered default state: 99.9% resolves to these before any JS.
    assert!(
        body.contains("43m 12s"),
        "must render the 99.9% monthly downtime server-side"
    );
    assert!(
        body.contains("8h 45m 36s"),
        "must render the 99.9% yearly downtime in the reference table"
    );
    assert!(
        body.contains("js/marketing/uptime_sla"),
        "must load the calculator script"
    );
    assert!(
        body.contains("uptime percentage calculator"),
        "must cover the secondary keyword"
    );
    assert!(
        body.contains("99.8611%"),
        "reverse widget must render its default uptime server-side"
    );
    assert!(
        body.contains("WebApplication") && body.contains("isAccessibleForFree"),
        "must carry the free WebApplication schema"
    );
    assert!(
        body.contains("https://app.uptimepage.dev/login"),
        "tool CTA should link to app_url"
    );
    assert!(
        headers.contains_key(header::ETAG),
        "tool page must set a strong ETag"
    );
}

#[tokio::test]
async fn cron_tool_renders_without_db() {
    let (status, body, headers) = get("/tools/cron-expression-generator").await;
    assert_eq!(status, StatusCode::OK);
    // Default expression + its authored description render server-side.
    assert!(
        body.contains("*/15 9-17 * * 1-5"),
        "must render the default expression"
    );
    assert!(
        body.contains("Monday through Friday"),
        "must render the default plain-English description server-side"
    );
    assert!(
        body.contains("js/marketing/cron"),
        "must load the cron script"
    );
    assert!(
        body.contains("Every 5 minutes"),
        "must render the reference table"
    );
    assert!(
        body.contains("WebApplication") && body.contains("isAccessibleForFree"),
        "must carry the free WebApplication schema"
    );
    assert!(
        headers.contains_key(header::ETAG),
        "tool page must set a strong ETag"
    );
}

#[tokio::test]
async fn incident_update_tool_renders_without_db() {
    let (status, body, headers) = get("/tools/incident-update-generator").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Incident update message generator"));
    assert!(body.contains("Investigating API issues"));
    assert!(body.contains("js/marketing/incident_update"));
    assert!(body.contains("WebApplication") && body.contains("isAccessibleForFree"));
    assert!(body.contains("Incident communication FAQ"));
    assert!(body.contains("Start with a common incident"));
    assert!(body.contains("Message quality"));
    assert!(body.contains("Email / support"));
    assert!(body.contains("What should customers do?"));
    assert!(body.contains("https://app.uptimepage.dev/login"));
    assert!(headers.contains_key(header::ETAG));
}

#[tokio::test]
async fn og_image_is_rooted_at_origin_on_subpages() {
    for path in [
        "/pricing",
        "/tools/cron-expression-generator",
        "/vs/uptimerobot",
    ] {
        let (status, body, _) = get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            body.contains(
                r#"property="og:image" content="https://uptimepage.dev/static/marketing/og.png""#
            ),
            "{path} must root og:image at the origin, not the page URL"
        );
    }
}

#[tokio::test]
async fn blog_post_og_image_overrides_the_shared_card() {
    let (status, body, _) = get("/blog/is-98-uptime-good").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(
            r#"property="og:image" content="https://uptimepage.dev/static/marketing/og-98-uptime.png""#
        ),
        "og_image front-matter must replace the shared card"
    );
    assert!(
        body.contains(
            r#"name="twitter:image" content="https://uptimepage.dev/static/marketing/og-98-uptime.png""#
        ),
        "twitter:image must follow the override"
    );

    let (status, body, _) = get("/blog/error-budgets-explained").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(
            r#"property="og:image" content="https://uptimepage.dev/static/marketing/og.png""#
        ),
        "posts without og_image must keep the shared card"
    );
}

#[tokio::test]
async fn sitemap_lists_the_tools() {
    let (status, body, _) = get("/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("https://uptimepage.dev/tools/uptime-sla-calculator"),
        "sitemap must list the SLA tool"
    );
    assert!(
        body.contains("https://uptimepage.dev/tools/cron-expression-generator"),
        "sitemap must list the cron tool"
    );
    assert!(
        body.contains("https://uptimepage.dev/tools/incident-update-generator"),
        "sitemap must list the incident update tool"
    );
}

/// Spot-checking landings by hand-typed path lets a retired page rot a test
/// instead of failing it. Drive the render off the table that mounts them.
#[tokio::test]
async fn every_landing_renders_and_its_links_resolve() {
    for l in landings::LANDINGS {
        let (status, _, _) = get(l.path).await;
        assert_eq!(status, StatusCode::OK, "{}", l.path);

        for r in l.resources {
            if !r.href.starts_with('/') {
                continue;
            }
            let (status, _, _) = get(r.href).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{} links to {}, which does not resolve",
                l.path,
                r.href
            );
        }
    }
}

#[tokio::test]
async fn retired_automation_path_redirects_to_terraform() {
    let (status, _, headers) = get("/automation").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        headers.get(header::LOCATION).and_then(|h| h.to_str().ok()),
        Some("/terraform-uptime-monitoring"),
        "the retired path must keep its equity on the page that absorbed it"
    );
}

#[tokio::test]
async fn terraform_landing_renders() {
    let (status, body, _) = get("/terraform-uptime-monitoring").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Uptime monitoring you declare in Terraform"));
    assert!(
        body.contains("uptimepage/uptimepage"),
        "must name the provider"
    );
    assert!(
        body.contains("uptimepage_target") && body.contains("expected_status"),
        "must show the HCL snippet"
    );
    assert!(
        body.contains("href=\"https://registry.terraform.io/providers/uptimepage/uptimepage\""),
        "must link the Terraform Registry"
    );
}

#[tokio::test]
async fn llms_full_txt_renders() {
    let (status, body, _) = get("/llms-full.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("# Uptimepage"));
    assert!(body.contains("## Facts"), "must include the facts table");
    assert!(
        body.contains("## Blog: "),
        "must inline published blog posts"
    );
    assert!(
        body.contains("A status page your SaaS customers actually trust"),
        "must inline landing-page copy"
    );
}

#[tokio::test]
async fn etag_is_stable_and_returns_304() {
    let (_, _, headers) = get("/").await;
    let etag = headers
        .get(header::ETAG)
        .expect("ETag present")
        .to_str()
        .expect("ETag ascii")
        .to_string();
    let resp = router()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::HOST, "uptimepage.dev")
                .header(header::IF_NONE_MATCH, etag.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router call");
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn marketing_serves_fingerprinted_assets() {
    // The dispatcher routes the whole apex/www host to the marketing
    // router, so marketing must own its own /static/{*path} route.
    // Without it, every <link href="/static/css/app.css?v=...">
    // emitted by a marketing template falls through to the marketing
    // 404 — page renders unstyled.
    let (status, _, headers) = get("/static/css/app.css").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "static assets must be served on the marketing host"
    );
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/css"), "got content-type {ct:?}");
}

#[tokio::test]
async fn legal_pages_render_without_db() {
    for route in uptimepage::marketing::legal::ROUTES {
        let (status, body, headers) = get(route.path).await;
        assert_eq!(status, StatusCode::OK, "{}", route.path);
        let heading = format!("<h1>{}</h1>", route.title);
        assert!(
            body.contains(&heading),
            "{} body missing rendered heading {heading:?}",
            route.path
        );
        assert!(
            headers.contains_key(header::ETAG),
            "{} must set a strong ETag",
            route.path
        );
    }
}

#[tokio::test]
async fn sitemap_lists_legal_routes() {
    let (status, body, _) = get("/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    for route in uptimepage::marketing::legal::ROUTES {
        let loc = format!("<loc>https://uptimepage.dev{}</loc>", route.path);
        assert!(body.contains(&loc), "sitemap missing {loc}");
    }
}

#[tokio::test]
async fn every_doc_page_renders_without_db() {
    for doc in uptimepage::marketing::docs::DOCS {
        let path = doc.path();
        let (status, body, _) = get(&path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(doc.title), "{path} missing its title");
        assert!(
            body.contains("mk-doc-nav__link"),
            "{path} rendered without the docs sidebar"
        );
    }
}

#[tokio::test]
async fn docs_index_renders_without_db() {
    let (status, body, _) = get("/docs").await;
    assert_eq!(status, StatusCode::OK);
    for doc in uptimepage::marketing::docs::DOCS {
        assert!(
            body.contains(&format!("href=\"{}\"", doc.path())),
            "docs index missing {}",
            doc.slug
        );
    }
}

#[tokio::test]
async fn unknown_doc_page_returns_branded_404() {
    let (status, body, _) = get("/docs/not-a-page").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Uptimepage"));
}

#[tokio::test]
async fn sitemap_and_llms_list_every_doc() {
    let (_, sitemap, _) = get("/sitemap.xml").await;
    let (_, llms, _) = get("/llms.txt").await;
    for doc in uptimepage::marketing::docs::DOCS {
        let loc = format!("<loc>https://uptimepage.dev{}</loc>", doc.path());
        assert!(sitemap.contains(&loc), "sitemap missing {loc}");
        assert!(
            llms.contains(&format!("https://uptimepage.dev{}", doc.path())),
            "llms.txt missing {}",
            doc.slug
        );
    }
}

#[tokio::test]
async fn llms_full_inlines_product_docs_but_not_self_hosting() {
    let (status, body, _) = get("/llms-full.txt").await;
    assert_eq!(status, StatusCode::OK);
    for doc in uptimepage::marketing::docs::DOCS {
        let heading = format!("## Docs: {}", doc.title);
        let self_hosting = doc.section == uptimepage::marketing::docs::Section::SelfHosting;
        assert_eq!(
            body.contains(&heading),
            !self_hosting,
            "{}: unexpected llms-full inlining",
            doc.slug
        );
    }
    // A distinctive line from a guide body, proving the source is inlined
    // rather than just the heading.
    assert!(
        body.contains("A monitor going down is only useful if it reaches someone"),
        "llms-full must carry doc bodies"
    );
}

#[tokio::test]
async fn cookie_does_not_change_response_body() {
    // Cookie isolation: marketing serves identical bytes whether or not
    // a `_sm_session` cookie tags along. No Vary: Cookie, no
    // Set-Cookie. Without this the apex CDN cache would be fractured by
    // session ID — a privacy + cacheability failure mode.
    let plain = router()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::HOST, "uptimepage.dev")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let with_cookie = router()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::HOST, "uptimepage.dev")
                .header(header::COOKIE, "_sm_session=fake-session-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plain.status(), with_cookie.status());
    assert!(
        with_cookie.headers().get(header::SET_COOKIE).is_none(),
        "marketing must not Set-Cookie"
    );
    let vary = with_cookie
        .headers()
        .get(header::VARY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        !vary.to_ascii_lowercase().contains("cookie"),
        "marketing must not Vary: Cookie, got {vary:?}"
    );
    let plain_bytes = axum::body::to_bytes(plain.into_body(), 1 << 20)
        .await
        .unwrap();
    let with_bytes = axum::body::to_bytes(with_cookie.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(plain_bytes, with_bytes);
}

/// The sidebar is baked into every page's cached render, so a nav that
/// differs between pages means a stale cache somewhere rather than a
/// content change. Also pins exactly one entry marked current.
#[tokio::test]
async fn docs_nav_is_identical_across_pages() {
    let mut seen: Option<(String, String)> = None;
    for doc in uptimepage::marketing::docs::DOCS {
        let (_, body, _) = get(&doc.path()).await;
        let start = body.find("mk-doc-nav__body").expect("sidebar");
        let end = body[start..].find("</nav>").expect("sidebar end") + start;
        let sidebar = &body[start..end];
        let active = sidebar.matches("is-active").count();
        assert_eq!(active, 1, "{}: {active} active nav entries", doc.slug);
        // Exactly-one-active would still hold if every page marked the same
        // wrong entry, so pin which one.
        let active_href = sidebar
            .split("<a ")
            .find(|tag| tag.contains("is-active"))
            .and_then(|tag| tag.split("href=\"").nth(1))
            .and_then(|rest| rest.split('"').next())
            .expect("active nav entry carries an href");
        assert_eq!(
            active_href,
            doc.path(),
            "{}: sidebar marks the wrong entry current",
            doc.slug
        );
        let neutral = sidebar
            .replace(" is-active", "")
            .replace(" aria-current=\"page\"", "");
        let count = sidebar.matches("mk-doc-nav__link").count();
        match &seen {
            None => {
                assert_eq!(
                    count,
                    uptimepage::marketing::docs::DOCS.len(),
                    "sidebar must link every published page"
                );
                seen = Some((neutral, doc.slug.to_string()));
            }
            Some((first, first_slug)) => assert_eq!(
                &neutral, first,
                "{} nav differs from {first_slug}",
                doc.slug
            ),
        }
    }
}
