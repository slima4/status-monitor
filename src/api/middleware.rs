use axum::extract::Request;
use axum::http::{HeaderValue, Method};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::middleware::Next;
use axum::response::Response;

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
