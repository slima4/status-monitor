use std::error::Error as _;
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use chrono::Utc;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes as HBytes;
use hyper::header::{
    ACCEPT_ENCODING, AUTHORIZATION, HeaderName, HeaderValue, LOCATION, USER_AGENT,
};
use hyper::{Method, Request, Uri};
use metrics::counter;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, ExpectedStatus, HttpCheck, HttpMethod};
use crate::http_client::HttpClients;
use crate::observability::metrics::names;

/// Hard ceiling on redirect hops, and the fallback when a check enables
/// following but leaves `max_redirects` at 0. The API schema documents
/// `maximum = 10`; that annotation is not validated server-side, so this
/// runtime clamp is the actual enforcement of the bound.
const MAX_REDIRECT_HOPS: u8 = 10;

pub async fn execute_http_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &HttpCheck,
    clients: &HttpClients,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let client = clients.pick(check.verify_tls);
    let _active = clients.pool_stats().inflight_guard();

    let origin = check.url.origin();
    let max_hops = effective_max_redirects(check);

    // Each hop reconnects through the same SSRF-guarded connector + DNS
    // resolver, so a `Location` pointing at an internal address is rejected
    // at connect time exactly like a directly-configured one — no separate
    // per-hop revalidation needed.
    let mut current = check.url.clone();
    let mut method = check.method;
    let mut send_body = check.body.clone();

    for hop in 0..=max_hops {
        let uri: Uri = match current.as_str().parse() {
            Ok(u) => u,
            Err(_) => {
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    "invalid url",
                );
            }
        };

        // Strip credentials when a redirect leaves the original origin so a
        // hostile or misconfigured `Location` can't harvest the configured
        // basic-auth / bearer token.
        let same_origin = current.origin() == origin;
        let req = match build_request(method, &uri, check, &send_body, same_origin, clients) {
            Ok(r) => r,
            Err(err) => {
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    format!("request build failed: {err}"),
                );
            }
        };

        let remaining = check.timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return CheckResult::error_with_elapsed(
                target_id,
                org_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "timeout",
            );
        }

        let response = match tokio::time::timeout(remaining, client.request(req)).await {
            Err(_) => {
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    "timeout",
                );
            }
            Ok(Ok(r)) => r,
            Ok(Err(err)) => {
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    classify_hyper_error(&err),
                );
            }
        };

        let status_code = response.status().as_u16();

        if check.follow_redirects && is_redirect(status_code) {
            if hop == max_hops {
                counter!(names::CHECK_REDIRECTS, "outcome" => "limit_exceeded").increment(1);
                tracing::warn!(
                    target_id = %target_id,
                    hops = max_hops,
                    "redirect limit exceeded"
                );
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    "too many redirects",
                );
            }

            let next = match response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| current.join(loc).ok())
            {
                Some(u) => u,
                None => {
                    counter!(names::CHECK_REDIRECTS, "outcome" => "invalid_location").increment(1);
                    tracing::warn!(
                        target_id = %target_id,
                        hop,
                        "redirect with missing or unparseable Location"
                    );
                    return CheckResult::error_with_elapsed(
                        target_id,
                        org_id,
                        started_at,
                        start.elapsed().as_millis() as u32,
                        "invalid redirect location",
                    );
                }
            };
            if !matches!(next.scheme(), "http" | "https") {
                counter!(names::CHECK_REDIRECTS, "outcome" => "blocked_scheme").increment(1);
                tracing::warn!(
                    target_id = %target_id,
                    hop,
                    scheme = next.scheme(),
                    "redirect to unsupported scheme"
                );
                return CheckResult::error_with_elapsed(
                    target_id,
                    org_id,
                    started_at,
                    start.elapsed().as_millis() as u32,
                    "unsupported redirect scheme",
                );
            }

            // 307/308 preserve the method and body; 301/302/303 degrade to a
            // bodyless GET (the universal browser behaviour, and the safe
            // choice for a monitor — never silently replay a POST).
            if !matches!(status_code, 307 | 308) {
                method = HttpMethod::Get;
                send_body = None;
            }

            counter!(names::CHECK_REDIRECTS, "outcome" => "followed").increment(1);
            tracing::debug!(
                target_id = %target_id,
                hop,
                status = status_code,
                // Host only — never the full target/redirect URL (may carry
                // tokens in the path/query).
                to_host = next.host_str().unwrap_or("?"),
                "following redirect"
            );

            // The 3xx body is intentionally not drained before drop: it is
            // tiny and the next hop almost always targets a different host
            // (apex→www, http→https), so there is no pooled connection to
            // reuse — draining would only add latency to this hot path.
            current = next;
            continue;
        }

        return finalize(
            target_id,
            org_id,
            check,
            started_at,
            start,
            response,
            status_code,
            clients,
        )
        .await;
    }

    // `for 0..=max_hops` always returns from inside the loop (final response
    // or the limit-exceeded branch); this is unreachable but keeps the
    // function total without an `unwrap`/`panic`.
    CheckResult::error_with_elapsed(
        target_id,
        org_id,
        started_at,
        start.elapsed().as_millis() as u32,
        "too many redirects",
    )
}

/// Collect, decode, and score the final (non-redirect) response.
// Private probe helper: the args are inherent (identity + check + timing
// + response + clients); grouping them would only move the tuple around.
#[allow(clippy::too_many_arguments)]
async fn finalize(
    target_id: Uuid,
    org_id: Uuid,
    check: &HttpCheck,
    started_at: chrono::DateTime<Utc>,
    start: Instant,
    response: hyper::Response<hyper::body::Incoming>,
    status_code: u16,
    clients: &HttpClients,
) -> CheckResult {
    let ttfb_elapsed_ms = start.elapsed().as_millis();
    clients.ttfb_ms.record(ttfb_elapsed_ms as f64);
    let ttfb_ms = ttfb_elapsed_ms.min(u16::MAX as u128) as u16;

    let content_encoding = response
        .headers()
        .get(hyper::header::CONTENT_ENCODING)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_ascii_lowercase());

    let body_remaining = check.timeout.saturating_sub(start.elapsed());
    let body_fut = response.into_body().collect();
    let collected = match tokio::time::timeout(body_remaining, body_fut).await {
        Err(_) => {
            let mut r = CheckResult::error_with_elapsed(
                target_id,
                org_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "body timeout",
            );
            r.ttfb_ms = Some(ttfb_ms);
            return r;
        }
        Ok(Ok(c)) => c,
        Ok(Err(_)) => {
            let mut r = CheckResult::error_with_elapsed(
                target_id,
                org_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "body",
            );
            r.ttfb_ms = Some(ttfb_ms);
            return r;
        }
    };

    let raw = collected.to_bytes();
    let decoded = match decode_body(&raw, content_encoding.as_deref()) {
        Ok(b) => b,
        Err(_) => {
            let mut r = CheckResult::error_with_elapsed(
                target_id,
                org_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "decode",
            );
            r.ttfb_ms = Some(ttfb_ms);
            return r;
        }
    };

    let size = decoded.len() as u32;
    let duration_ms = start.elapsed().as_millis() as u32;

    let status_ok = match_status(status_code, &check.expected_status);
    let body_ok = match &check.expected_body_contains {
        Some(needle) => std::str::from_utf8(&decoded)
            .map(|s| s.contains(needle))
            .unwrap_or(false),
        None => true,
    };

    let (status, error) = if !status_ok {
        (
            CheckStatus::Down,
            Some(format!("unexpected status {status_code}")),
        )
    } else if !body_ok {
        (CheckStatus::Down, Some("body match failed".to_string()))
    } else {
        (CheckStatus::Up, None)
    };

    CheckResult {
        target_id,
        org_id,
        timestamp: started_at,
        status,
        duration_ms,
        dns_ms: None,
        connect_ms: None,
        tls_ms: None,
        ttfb_ms: Some(ttfb_ms),
        response_code: Some(status_code),
        response_size: Some(size),
        error,
    }
}

fn build_request(
    method: HttpMethod,
    uri: &Uri,
    check: &HttpCheck,
    body: &Option<String>,
    include_auth: bool,
    clients: &HttpClients,
) -> Result<Request<Full<HBytes>>, hyper::http::Error> {
    let body_bytes: HBytes = match body {
        Some(b) => HBytes::from(b.clone().into_bytes()),
        None => HBytes::new(),
    };

    let mut builder = Request::builder()
        .method(map_method(method))
        .uri(uri)
        .header(USER_AGENT, clients.user_agent())
        .header(ACCEPT_ENCODING, "gzip, br");

    let bodyless = body.is_none();
    for (k, v) in &check.headers {
        // Drop a caller-set Authorization header on a cross-origin hop for
        // the same reason the configured credentials are dropped.
        if !include_auth && k.eq_ignore_ascii_case("authorization") {
            continue;
        }
        // After a 301/302/303 degrades to a bodyless GET, a caller-set
        // content-framing header would describe a body that no longer
        // exists — a malformed request strict origins/WAFs reject (a
        // spurious Down on exactly the canonical-redirect targets this
        // feature exists for). Hyper sets its own `content-length: 0`.
        if bodyless
            && (k.eq_ignore_ascii_case("content-type")
                || k.eq_ignore_ascii_case("content-length")
                || k.eq_ignore_ascii_case("transfer-encoding"))
        {
            continue;
        }
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(k.as_str()), HeaderValue::try_from(v)) {
            builder = builder.header(name, val);
        }
    }
    if include_auth {
        if let Some((user, pass)) = &check.basic_auth {
            let encoded = BASE64_STANDARD.encode(format!("{user}:{pass}"));
            if let Ok(val) = HeaderValue::try_from(format!("Basic {encoded}")) {
                builder = builder.header(AUTHORIZATION, val);
            }
        }
        if let Some(token) = &check.bearer_token
            && let Ok(val) = HeaderValue::try_from(format!("Bearer {token}"))
        {
            builder = builder.header(AUTHORIZATION, val);
        }
    }

    builder.body(Full::new(body_bytes))
}

/// Effective hop budget: `0` disables following entirely; an enabled check
/// with `max_redirects == 0` falls back to [`MAX_REDIRECT_HOPS`], and any
/// configured value is clamped to that same ceiling.
fn effective_max_redirects(check: &HttpCheck) -> u8 {
    if !check.follow_redirects {
        return 0;
    }
    match check.max_redirects {
        0 => MAX_REDIRECT_HOPS,
        n => n.min(MAX_REDIRECT_HOPS),
    }
}

fn is_redirect(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307 | 308)
}

fn decode_body(raw: &HBytes, encoding: Option<&str>) -> std::io::Result<Bytes> {
    use std::io::Read;
    match encoding {
        // Typical gzip/brotli ratios on text are 3-5×; pre-size larger to avoid
        // re-allocation in the decoder loop.
        Some("gzip") => {
            let mut out = Vec::with_capacity(raw.len().saturating_mul(4));
            GzDecoder::new(raw.as_ref()).read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        Some("br") => {
            let mut out = Vec::with_capacity(raw.len().saturating_mul(4));
            brotli::Decompressor::new(raw.as_ref(), 4096).read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        _ => Ok(raw.clone()),
    }
}

fn map_method(m: HttpMethod) -> Method {
    match m {
        HttpMethod::Get => Method::GET,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Post => Method::POST,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Options => Method::OPTIONS,
    }
}

fn match_status(code: u16, expected: &ExpectedStatus) -> bool {
    match expected {
        ExpectedStatus::Exact(c) => code == *c,
        ExpectedStatus::Range { min, max } => code >= *min && code <= *max,
        ExpectedStatus::OneOf(list) => list.contains(&code),
    }
}

fn classify_hyper_error(err: &hyper_util::client::legacy::Error) -> &'static str {
    if err.is_connect() {
        // Source chain often carries an io::Error or our own "tcp connect timeout".
        let msg = err
            .source()
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        if msg.contains("tcp connect timeout") || msg.to_lowercase().contains("timeout") {
            "timeout"
        } else {
            "connect"
        }
    } else {
        "transport"
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use url::Url;

    use super::*;

    fn check(url: &str, follow: bool, max: u8) -> HttpCheck {
        HttpCheck {
            url: Url::parse(url).unwrap(),
            method: HttpMethod::Get,
            timeout: Duration::from_secs(3),
            follow_redirects: follow,
            max_redirects: max,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: None,
            headers: Default::default(),
            body: None,
            verify_tls: true,
            basic_auth: None,
            bearer_token: None,
        }
    }

    #[test]
    fn effective_budget_off_when_not_following() {
        assert_eq!(effective_max_redirects(&check("https://x/", false, 5)), 0);
    }

    #[test]
    fn effective_budget_falls_back_and_clamps() {
        assert_eq!(
            effective_max_redirects(&check("https://x/", true, 0)),
            MAX_REDIRECT_HOPS
        );
        assert_eq!(effective_max_redirects(&check("https://x/", true, 3)), 3);
        assert_eq!(
            effective_max_redirects(&check("https://x/", true, 250)),
            MAX_REDIRECT_HOPS
        );
    }

    #[test]
    fn redirect_status_set() {
        for c in [301, 302, 303, 307, 308] {
            assert!(is_redirect(c));
        }
        for c in [200, 204, 300, 304, 305, 400, 500] {
            assert!(!is_redirect(c));
        }
    }
}
