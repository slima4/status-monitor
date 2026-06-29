use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header, header::HeaderName};
use axum::middleware::Next;
use axum::response::Response;
use moka::sync::Cache;

/// `POST /targets/bulk` and `POST /targets/bulk-action` accept an optional
/// `Idempotency-Key` header. The server replays the prior response for 24h,
/// keyed by `(caller credential, header value, body hash)`.
pub const TTL: Duration = Duration::from_secs(24 * 3600);
pub const HEADER_NAME: HeaderName = HeaderName::from_static("idempotency-key");

const MAX_BODY: usize = 8 * 1024 * 1024;
/// Total cached response bytes before LRU eviction (weighed by body size).
const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone)]
struct CachedResponse {
    status: u16,
    content_type: Option<HeaderValue>,
    body: Bytes,
}

/// Response cache keyed by `(caller credential, idempotency-key, body-hash)`,
/// bounded by a 24h TTL and a total-bytes weigher.
pub struct IdempotencyCache {
    cache: Cache<String, CachedResponse>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self {
            cache: Cache::builder()
                .time_to_live(TTL)
                .weigher(|_k, v: &CachedResponse| v.body.len().min(u32::MAX as usize) as u32)
                .max_capacity(MAX_CACHE_BYTES)
                .build(),
        }
    }

    fn lookup(&self, key: &str) -> Option<CachedResponse> {
        self.cache.get(key)
    }

    fn store(&self, key: String, value: CachedResponse) {
        self.cache.insert(key, value);
    }
}

impl Default for IdempotencyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// axum middleware: if `Idempotency-Key` is set, replay the cached response
/// for the `(caller, key, body-hash)` tuple — or capture the downstream
/// response and cache it. Pass-through when the header is absent.
pub async fn middleware(
    State(cache): State<Arc<IdempotencyCache>>,
    req: Request,
    next: Next,
) -> Response {
    let key_header = req
        .headers()
        .get(&HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());
    let Some(key_header) = key_header else {
        return next.run(req).await;
    };

    // Scope the key to the caller's credential — bearer token or session
    // cookie — so two tenants reusing the same Idempotency-Key never collide.
    let mut caller_hasher = DefaultHasher::new();
    for name in [header::AUTHORIZATION, header::COOKIE] {
        req.headers()
            .get(&name)
            .map(|v| v.as_bytes())
            .unwrap_or_default()
            .hash(&mut caller_hasher);
    }
    let caller_hash = caller_hasher.finish();

    let (parts, body) = req.into_parts();
    let body_bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            // Body too large or read failure — delegate downstream so the
            // handler returns its usual 413 / 400 instead of swallowing it.
            return next.run(Request::from_parts(parts, Body::empty())).await;
        }
    };

    let mut hasher = DefaultHasher::new();
    body_bytes.hash(&mut hasher);
    let cache_key = format!("{caller_hash:016x}:{key_header}:{:016x}", hasher.finish());

    if let Some(cached) = cache.lookup(&cache_key) {
        return rebuild_response(cached);
    }

    let req = Request::from_parts(parts, Body::from(body_bytes));
    let resp = next.run(req).await;

    // Capture the response body so it can be replayed. Failures to read the
    // body return a 500 — the handler succeeded but the response is corrupt.
    let (resp_parts, resp_body) = resp.into_parts();
    let resp_bytes = match axum::body::to_bytes(resp_body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "response body too large")
                .into_response_local();
        }
    };
    let content_type = resp_parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .cloned();

    cache.store(
        cache_key,
        CachedResponse {
            status: resp_parts.status.as_u16(),
            content_type,
            body: resp_bytes.clone(),
        },
    );

    Response::from_parts(resp_parts, Body::from(resp_bytes))
}

fn rebuild_response(cached: CachedResponse) -> Response {
    let mut resp = Response::new(Body::from(cached.body));
    *resp.status_mut() = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    if let Some(ct) = cached.content_type {
        resp.headers_mut()
            .insert(axum::http::header::CONTENT_TYPE, ct);
    }
    resp
}

// Local helper trait to keep error path concise without importing axum's response prelude.
trait IntoResponseLocal {
    fn into_response_local(self) -> Response;
}

impl IntoResponseLocal for (StatusCode, &'static str) {
    fn into_response_local(self) -> Response {
        let mut resp = Response::new(Body::from(self.1));
        *resp.status_mut() = self.0;
        resp
    }
}
