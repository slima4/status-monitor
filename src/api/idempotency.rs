use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header::HeaderName};
use axum::middleware::Next;
use axum::response::Response;
use dashmap::DashMap;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

/// Per-spec §5.7: `POST /targets/bulk` and `POST /targets/bulk-action` accept
/// an optional `Idempotency-Key` header. The server replays the prior response
/// for 24h when keyed by `(header value, body hash)`.
pub const TTL: Duration = Duration::from_secs(24 * 3600);
pub const HEADER_NAME: HeaderName = HeaderName::from_static("idempotency-key");

const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct CachedResponse {
    inserted: Instant,
    status: u16,
    content_type: Option<HeaderValue>,
    body: Bytes,
}

/// Thread-safe cache mapping `(idempotency-key, body-hash)` → recorded
/// response. Entries are pruned by a background task; lookups skip stale
/// entries without waiting for the pruner.
pub struct IdempotencyCache {
    map: DashMap<String, CachedResponse>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self {
            map: DashMap::new(),
        }
    }

    fn lookup(&self, key: &str) -> Option<CachedResponse> {
        let entry = self.map.get(key)?;
        if entry.value().inserted.elapsed() >= TTL {
            return None;
        }
        Some(entry.value().clone())
    }

    fn store(&self, key: String, value: CachedResponse) {
        self.map.insert(key, value);
    }

    fn prune(&self) {
        self.map.retain(|_, v| v.inserted.elapsed() < TTL);
    }
}

impl Default for IdempotencyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawns a background task that prunes expired cache entries every hour.
/// Exits when `shutdown` fires.
pub fn spawn_pruner(cache: Arc<IdempotencyCache>, shutdown: CancellationToken) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(3600));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = ticker.tick() => cache.prune(),
            }
        }
    });
}

/// axum middleware: if `Idempotency-Key` is set, replay the cached response
/// for the `(key, body-hash)` tuple — or capture the downstream response and
/// cache it. Pass-through when the header is absent.
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
    let cache_key = format!("{key_header}:{:016x}", hasher.finish());

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
            inserted: Instant::now(),
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
