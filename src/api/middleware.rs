use axum::extract::Request;
use axum::http::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::url::{safe_redirect_target, url_encode};

const JSON_WITH_CHARSET: HeaderValue = HeaderValue::from_static("application/json; charset=utf-8");
const NO_STORE: HeaderValue = HeaderValue::from_static("no-store");
const DASHBOARD_CACHE: HeaderValue = HeaderValue::from_static("private, max-age=5");
const READ_CACHE: HeaderValue = HeaderValue::from_static("private, max-age=10");
const PUBLIC_CACHE: HeaderValue =
    HeaderValue::from_static("public, max-age=10, stale-while-revalidate=30");

/// Rewrites bare `application/json` Content-Type headers to include
/// `charset=utf-8`. axum's `Json` extractor emits the bare form; downstream
/// clients (and the contract surfaced via OpenAPI) expect the charset suffix
/// on every JSON response.
pub async fn json_charset(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let needs_rewrite = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s == "application/json");
    if needs_rewrite {
        resp.headers_mut().insert(CONTENT_TYPE, JSON_WITH_CHARSET);
    }
    resp
}

/// True when the request is a top-level browser navigation. Modern browsers
/// stamp `Sec-Fetch-Mode: navigate` on address-bar / link / form loads; `fetch`
/// and `XHR` send `cors` / `same-origin`. Older browsers (Safari < 16.4) and
/// in-app webviews omit `Sec-Fetch-*`, so fall back to an HTML-preferring
/// `Accept` — which `fetch`/SDK callers (`application/json`, `*/*`) don't send.
pub fn is_browser_navigation(headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok()) {
        Some(mode) => mode.eq_ignore_ascii_case("navigate"),
        None => headers
            .get(ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html")),
    }
}

/// A browser navigation that hits an operator API route without a session would
/// render the bare JSON error envelope in the tab. When such a navigation comes
/// back 401, swap it for a redirect to the login page that returns the user
/// here afterward. Scoped to `/api/v1/*`: programmatic 401s carry no navigate
/// marker, and public / docs / health routes are left untouched so their own
/// errors surface intact.
pub async fn navigation_login_redirect(req: Request, next: Next) -> Response {
    // Capture the return target only for navigations — keeps the per-request
    // alloc off the hot path (every authenticated API call skips it).
    let target = is_browser_navigation(req.headers())
        .then(|| req.uri().path_and_query().map(|pq| pq.as_str().to_string()))
        .flatten();
    let resp = next.run(req).await;
    if resp.status() == StatusCode::UNAUTHORIZED
        && let Some(pq) = target.as_deref()
        && pq.starts_with("/api/v1")
    {
        let next = safe_redirect_target(pq).unwrap_or("/");
        let location = format!("/login?redirect_after={}", url_encode(next));
        return (StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response();
    }
    resp
}

/// Stamps a default `Cache-Control` value on `/api/v1/*` responses unless the
/// handler already set one. Mutations get `no-store`; read endpoints get a
/// short private TTL matching their server-side cache horizon (dashboard cache
/// = 5 s, everything else read-only = 10 s).
pub async fn cache_control(req: Request, next: Next) -> Response {
    let value = cache_control_for(req.method(), req.uri().path());
    let mut resp = next.run(req).await;
    if let Some(v) = value
        && !resp.headers().contains_key(CACHE_CONTROL)
    {
        resp.headers_mut().insert(CACHE_CONTROL, v);
    }
    resp
}

fn cache_control_for(method: &Method, path: &str) -> Option<HeaderValue> {
    if !path.starts_with("/api/v1/") {
        return None;
    }
    if method != Method::GET && method != Method::HEAD {
        return Some(NO_STORE);
    }
    if path == DASHBOARD_PATH {
        return Some(DASHBOARD_CACHE);
    }
    Some(READ_CACHE)
}

/// Kept in sync with the route declaration in `routes.rs`.
const DASHBOARD_PATH: &str = "/api/v1/dashboard/summary";

/// Cache-Control middleware applied only to the public-status surface.
/// Sets `public, max-age=10, stale-while-revalidate=30` unless the handler
/// already emitted its own value.
pub async fn public_cache_control(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    if !resp.headers().contains_key(CACHE_CONTROL) {
        resp.headers_mut().insert(CACHE_CONTROL, PUBLIC_CACHE);
    }
    resp
}
