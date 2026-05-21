//! The load-bearing decoupling check: every marketing route serves 2xx
//! when no Postgres / ClickHouse handle is in scope. The marketing
//! module takes its own `MarketingCfg` (not `AppState`), so this test
//! constructs that config directly and exercises each route through the
//! returned `axum::Router` — no pool, no client, no `AppState`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::util::ServiceExt;

use status_monitor::marketing::{self, MarketingCfg};

fn router() -> axum::Router {
    marketing::router(MarketingCfg {
        app_url: "https://app.uptimepage.dev".into(),
        canonical_origin: "https://uptimepage.dev".into(),
        blog_enabled: true,
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
async fn blog_index_renders_without_db() {
    let (status, body, _) = get("/blog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("From the workshop"));
}

#[tokio::test]
async fn known_post_renders_without_db() {
    let (status, body, _) = get("/blog/boring-uptime").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Why your uptime monitor should be boring"));
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
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/plain"), "got {ct:?}");
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
        body.contains("<loc>https://uptimepage.dev/blog/boring-uptime</loc>"),
        "published post must be in sitemap"
    );
}

#[tokio::test]
async fn llms_txt_renders() {
    let (status, body, _) = get("/llms.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with("# Uptimepage"));
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
    for (path, expected_heading) in [
        ("/terms", "Terms of Service"),
        ("/privacy", "Privacy Policy"),
        ("/cookies", "Cookie Policy"),
        ("/impressum", "Impressum"),
        ("/abuse-policy", "Abuse Policy"),
        ("/security-policy", "Security Policy"),
    ] {
        let (status, body, headers) = get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            body.contains(expected_heading),
            "{path} missing heading {expected_heading:?}"
        );
        assert!(
            headers.contains_key(header::ETAG),
            "{path} must set a strong ETag"
        );
    }
}

#[tokio::test]
async fn sitemap_lists_legal_routes() {
    let (status, body, _) = get("/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    for path in [
        "/terms",
        "/privacy",
        "/cookies",
        "/impressum",
        "/abuse-policy",
        "/security-policy",
    ] {
        let loc = format!("<loc>https://uptimepage.dev{path}</loc>");
        assert!(body.contains(&loc), "sitemap missing {loc}");
    }
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
