use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::Next;
use axum::response::Response;

const JSON_WITH_CHARSET: HeaderValue = HeaderValue::from_static("application/json; charset=utf-8");

/// Rewrites bare `application/json` Content-Type headers to include
/// `charset=utf-8`. axum's `Json` extractor emits the bare form; the spec
/// (§5.9) requires the charset suffix on every API response.
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
