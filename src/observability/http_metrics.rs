//! Per-route HTTP metrics middleware.
//!
//! Records `http_requests_total`, `http_request_duration_ms`, and
//! `http_responses_inflight` for every routed request. The route label is
//! `MatchedPath` (the path-pattern with placeholders like `/orgs/:id`), not
//! the concrete URL — cardinality stays bounded by the router's static
//! route table, regardless of how many distinct orgs or targets the
//! customers add.
//!
//! Method is clamped to the canonical HTTP verb set; status is bucketed to
//! `2xx`/`3xx`/`4xx`/`5xx`/`other`. Both keep label cardinality on this hot
//! path tractable — an attacker spraying `Method::from_bytes(b"AAAA…")` or
//! exotic status codes can't grow the Prometheus series count.
//!
//! Health-probe paths (`/healthz`, `/readyz`) short-circuit. Caddy active-
//! health and the deploy gate poll them in a tight loop; counting every
//! probe would dominate `http_requests_total` and pollute the SLO ratios
//! the dashboards compute. The same suppression already exists in the
//! access-log + trace span paths — keeping it consistent.
//!
//! Duration is measured up to response *head* construction (when
//! `next.run` returns), not body completion. There are no streaming
//! routes today; if one is added, this becomes head-build latency and
//! the metric name should be revisited.

use std::sync::LazyLock;
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use metrics::{Gauge, counter, gauge, histogram};

use crate::api::handlers::health::is_health_path;
use crate::observability::metrics::names;

static INFLIGHT: LazyLock<Gauge> = LazyLock::new(|| gauge!(names::HTTP_RESPONSES_INFLIGHT));

/// RAII guard so the inflight gauge stays accurate across early-return
/// and `?`-propagation. Release builds use `panic = "abort"` (see
/// `Cargo.toml`), so a panicking handler restarts the process and the
/// gauge resets — Drop is the recovery path for non-panic failures, not
/// for panics.
struct InflightGuard;

impl InflightGuard {
    fn new() -> Self {
        INFLIGHT.increment(1.0);
        Self
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        INFLIGHT.decrement(1.0);
    }
}

pub async fn middleware(req: Request, next: Next) -> Response {
    // Skip health-probe noise: same contract as access_log + TraceLayer.
    if is_health_path(req.uri().path()) {
        return next.run(req).await;
    }

    let method = method_label(req.method());
    // MatchedPath is populated by axum's routing before this middleware
    // runs. Fallback handlers (e.g. catch-all 404s) carry no MatchedPath
    // and bucket under `<unmatched>` — bot-scan noise stays bounded to
    // one label series rather than one per URL.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_string());

    let _guard = InflightGuard::new();
    let started = Instant::now();
    let response = next.run(req).await;
    // `as_secs_f64() * 1000.0` retains sub-millisecond precision; the
    // u128 → f64 cast on `as_millis` would floor everything below 1 ms
    // to zero and collapse fast-path latencies onto a single bucket.
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let status = status_class(response.status());

    histogram!(
        names::HTTP_REQUEST_DURATION_MS,
        "method" => method,
        "route" => route.clone(),
    )
    .record(elapsed_ms);
    counter!(
        names::HTTP_REQUESTS_TOTAL,
        "method" => method,
        "route" => route,
        "status" => status,
    )
    .increment(1);

    response
}

/// Clamps the request method to a fixed set of canonical verbs. Anything
/// outside the set — including attacker-crafted `Method::from_bytes(...)`
/// values that axum's `any(handler)` routes would otherwise pass through
/// — buckets to `"other"`, so the `method` label can't be used as an
/// unbounded cardinality vector.
fn method_label(method: &Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "other",
    }
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() / 100 {
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::{method_label, status_class};
    use axum::http::{Method, StatusCode};

    #[test]
    fn status_class_buckets_by_hundreds() {
        assert_eq!(status_class(StatusCode::OK), "2xx");
        assert_eq!(status_class(StatusCode::CREATED), "2xx");
        assert_eq!(status_class(StatusCode::FOUND), "3xx");
        assert_eq!(status_class(StatusCode::BAD_REQUEST), "4xx");
        assert_eq!(status_class(StatusCode::NOT_FOUND), "4xx");
        assert_eq!(status_class(StatusCode::INTERNAL_SERVER_ERROR), "5xx");
        assert_eq!(status_class(StatusCode::SERVICE_UNAVAILABLE), "5xx");
        assert_eq!(status_class(StatusCode::CONTINUE), "other");
    }

    #[test]
    fn method_label_clamps_to_canonical_verbs() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::POST), "POST");
        assert_eq!(method_label(&Method::PUT), "PUT");
        assert_eq!(method_label(&Method::PATCH), "PATCH");
        assert_eq!(method_label(&Method::DELETE), "DELETE");
        assert_eq!(method_label(&Method::HEAD), "HEAD");
        assert_eq!(method_label(&Method::OPTIONS), "OPTIONS");
    }

    #[test]
    fn method_label_buckets_unknown_verbs_under_other() {
        let custom = Method::from_bytes(b"PROPFIND").expect("valid token");
        assert_eq!(method_label(&custom), "other");
        // Attacker-style unbounded value — must NOT escape the bucket.
        let fuzz = Method::from_bytes(b"AAAAAAAA").expect("valid token");
        assert_eq!(method_label(&fuzz), "other");
    }
}
