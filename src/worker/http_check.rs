use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use chrono::Utc;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes as HBytes;
use hyper::header::{
    ACCEPT_ENCODING, AUTHORIZATION, HeaderName, HeaderValue, LOCATION, RETRY_AFTER, USER_AGENT,
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

/// Cap on the raw HTTP response body the check will collect. Status pages —
/// the dominant target shape — are usually well under 100 KiB; 1 MiB is the
/// "huge response, but worth keeping the bytes" knee. Anything bigger is
/// recorded as a `body` failure rather than allowed to allocate freely.
const MAX_RAW_BODY_BYTES: usize = 1 << 20;

/// Cap on decompressed body size. Bounds the gzip / brotli expansion ratio
/// against a hostile target that returns a tiny compressed body that explodes
/// on decode (a "zip bomb"). 8 MiB tolerates the ~8× expansion that real
/// HTML/JSON pages hit; anything past that is a `decode` failure.
const MAX_DECODED_BODY_BYTES: usize = 8 << 20;

/// Body snippet cap for the verbose test-check response.
const PROBE_BODY_SNIPPET_BYTES: usize = 1024;
/// Header-pair cap (after dedup-by-name) for the verbose test-check response.
const PROBE_HEADERS_MAX: usize = 20;
/// Header names whose values must not appear in the verbose response.
const PROBE_REDACT_HEADERS: &[&str] = &[
    "set-cookie",
    "authorization",
    "cookie",
    "proxy-authorization",
    "x-auth-token",
    "x-api-key",
    "x-csrf-token",
    "x-amz-security-token",
];

/// Captured HTTP-level detail for the test-check endpoint.
#[derive(Debug, Clone, Default)]
pub(crate) struct HttpProbe {
    pub response_headers_preview: Vec<crate::api::types::HeaderPreview>,
    pub response_body_snippet: Option<String>,
}

pub async fn execute_http_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &HttpCheck,
    clients: &HttpClients,
) -> CheckResult {
    do_http_check(target_id, org_id, check, clients, false)
        .await
        .0
}

/// Verbose variant for the operator test-check UI. Same result plus a
/// probe of headers + body snippet (sensitive values redacted, body capped).
pub(crate) async fn execute_http_check_probe(
    target_id: Uuid,
    org_id: Uuid,
    check: &HttpCheck,
    clients: &HttpClients,
) -> (CheckResult, HttpProbe) {
    let (r, p) = do_http_check(target_id, org_id, check, clients, true).await;
    (r, p.unwrap_or_default())
}

async fn do_http_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &HttpCheck,
    clients: &HttpClients,
    capture: bool,
) -> (CheckResult, Option<HttpProbe>) {
    let started_at = Utc::now();
    let start = Instant::now();

    let client = clients.pick(check.verify_tls);
    let _active = clients.pool_stats().inflight_guard();

    let origin = check.url.origin();
    let max_hops = effective_max_redirects(check);

    let early = |err: &str| -> (CheckResult, Option<HttpProbe>) {
        (
            CheckResult::error_with_elapsed(
                target_id,
                org_id,
                started_at,
                start.elapsed().as_millis() as u32,
                err.to_owned(),
            ),
            None,
        )
    };

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
            Err(_) => return early("invalid url"),
        };

        // Strip credentials when a redirect leaves the original origin so a
        // hostile or misconfigured `Location` can't harvest the configured
        // basic-auth / bearer token.
        let same_origin = current.origin() == origin;
        let req = match build_request(method, &uri, check, &send_body, same_origin, clients) {
            Ok(r) => r,
            Err(err) => return early(&format!("request build failed: {err}")),
        };

        let remaining = check.timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return early("timeout");
        }

        let response = match tokio::time::timeout(remaining, client.request(req)).await {
            Err(_) => return early("timeout"),
            Ok(Ok(r)) => r,
            Ok(Err(err)) => return early(classify_hyper_error(&err)),
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
                return early("too many redirects");
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
                    return early("invalid redirect location");
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
                return early("unsupported redirect scheme");
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
            capture,
        )
        .await;
    }

    // `for 0..=max_hops` always returns from inside the loop (final response
    // or the limit-exceeded branch); this is unreachable but keeps the
    // function total without an `unwrap`/`panic`.
    early("too many redirects")
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
    capture: bool,
) -> (CheckResult, Option<HttpProbe>) {
    let ttfb_elapsed_ms = start.elapsed().as_millis();
    clients.ttfb_ms.record(ttfb_elapsed_ms as f64);
    let ttfb_ms = ttfb_elapsed_ms.min(u16::MAX as u128) as u16;

    // Header preview is snapshotted before the response body is consumed:
    // hyper bodies are streams and the headers handle becomes unavailable
    // once we move the response into Limited.
    let headers_preview = capture.then(|| snapshot_headers(response.headers()));
    let retry_after_raw = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .filter(|s| sanitised_retry_after(s))
        .map(|s| s.to_string());

    let err_with_ttfb = |reason: &'static str| -> (CheckResult, Option<HttpProbe>) {
        let mut r = CheckResult::error_with_elapsed(
            target_id,
            org_id,
            started_at,
            start.elapsed().as_millis() as u32,
            reason,
        );
        r.ttfb_ms = Some(ttfb_ms);
        let probe = headers_preview.clone().map(|h| HttpProbe {
            response_headers_preview: h,
            response_body_snippet: None,
        });
        (r, probe)
    };

    // HEAD body is empty; the origin still sets Content-Encoding from the
    // GET variant, so any decode attempt fails on the empty stream.
    let decoded = if check.method == HttpMethod::Head {
        Bytes::new()
    } else {
        let content_encoding = response
            .headers()
            .get(hyper::header::CONTENT_ENCODING)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_ascii_lowercase());
        let body_remaining = check.timeout.saturating_sub(start.elapsed());
        // Bound at the byte-budget *before* allocation: Limited streams frames
        // and errors mid-read once the cap is exceeded, so an oversized response
        // never sits fully in memory.
        let body_fut = Limited::new(response.into_body(), MAX_RAW_BODY_BYTES).collect();
        let collected = match tokio::time::timeout(body_remaining, body_fut).await {
            Err(_) => return err_with_ttfb("body timeout"),
            Ok(Ok(c)) => c,
            Ok(Err(_)) => return err_with_ttfb("body"),
        };
        let raw = collected.to_bytes();
        match decode_body(&raw, content_encoding.as_deref()) {
            Ok(b) => b,
            Err(_) => return err_with_ttfb("decode"),
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

    let (status, error) =
        classify_outcome(status_code, status_ok, body_ok, retry_after_raw.as_deref());

    let probe = capture.then(|| HttpProbe {
        response_headers_preview: headers_preview.unwrap_or_default(),
        response_body_snippet: Some(snapshot_body(&decoded)),
    });

    (
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
        },
        probe,
    )
}

/// First N unique-by-name response headers, sensitive values redacted.
/// Multi-value headers (Set-Cookie, Link) collapse to one entry so the cap
/// isn't burnt on a single repeated name.
fn snapshot_headers(headers: &hyper::HeaderMap) -> Vec<crate::api::types::HeaderPreview> {
    use crate::api::types::HeaderPreview;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<HeaderPreview> = Vec::with_capacity(PROBE_HEADERS_MAX);
    for (k, v) in headers.iter() {
        let name = k.as_str().to_owned();
        let lower = name.to_ascii_lowercase();
        if !seen.insert(lower.clone()) {
            continue;
        }
        let value = if PROBE_REDACT_HEADERS.iter().any(|r| *r == lower) {
            "***REDACTED***".to_owned()
        } else {
            v.to_str().unwrap_or("<non-ascii>").to_owned()
        };
        out.push(HeaderPreview { name, value });
        if out.len() >= PROBE_HEADERS_MAX {
            break;
        }
    }
    out
}

/// First [`PROBE_BODY_SNIPPET_BYTES`] of the decoded body as UTF-8 (lossy
/// for binary responses, which surface as replacement characters). Snaps
/// the cut back to a UTF-8 codepoint boundary so multi-byte chars aren't
/// chopped mid-sequence into a replacement character.
fn snapshot_body(decoded: &[u8]) -> String {
    let mut end = decoded.len().min(PROBE_BODY_SNIPPET_BYTES);
    while end > 0 && (decoded[end - 1] & 0b1100_0000) == 0b1000_0000 {
        end -= 1;
    }
    String::from_utf8_lossy(&decoded[..end]).into_owned()
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
    match encoding {
        // Typical gzip/brotli ratios on text are 3-5×; pre-size larger to
        // avoid re-allocation in the decoder loop. Read +1 past the cap so a
        // body that exactly *fills* the budget is still detected as "over"
        // and rejected rather than silently truncated to a misleading length.
        Some("gzip") => decode_capped(GzDecoder::new(raw.as_ref()), raw.len()),
        Some("br") => decode_capped(brotli::Decompressor::new(raw.as_ref(), 4096), raw.len()),
        _ => Ok(raw.clone()),
    }
}

fn decode_capped<R: std::io::Read>(reader: R, raw_len: usize) -> std::io::Result<Bytes> {
    use std::io::Read;
    // Pre-size for the typical 4× expansion ratio without exceeding the cap.
    let capacity = raw_len.saturating_mul(4).min(MAX_DECODED_BODY_BYTES);
    let mut out = Vec::with_capacity(capacity);
    reader
        .take((MAX_DECODED_BODY_BYTES as u64) + 1)
        .read_to_end(&mut out)?;
    if out.len() > MAX_DECODED_BODY_BYTES {
        return Err(std::io::Error::other(format!(
            "decoded body exceeded {MAX_DECODED_BODY_BYTES} bytes"
        )));
    }
    Ok(Bytes::from(out))
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

/// Retry-After per RFC 9110: delta-seconds or HTTP-date — both ASCII +
/// alphanumeric + a handful of punctuation. Anything outside this set is
/// untrusted upstream input that downstream notifiers (Slack-flavoured
/// markdown, e.g.) would otherwise render verbatim.
fn sanitised_retry_after(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b' ' | b',' | b':' | b'.' | b'+' | b'-')
        })
}

/// 429 / 503 means "we're throttling you, try later" — distinct from a real
/// outage. Map it to `Degraded` so the monitor reflects back-pressure without
/// opening an incident. A user who explicitly accepts 429/503 in
/// `expected_status` is honored first.
fn classify_outcome(
    status_code: u16,
    status_ok: bool,
    body_ok: bool,
    retry_after_raw: Option<&str>,
) -> (CheckStatus, Option<String>) {
    if !status_ok && matches!(status_code, 429 | 503) {
        let detail = retry_after_raw
            .map(|v| format!(" (Retry-After: {v})"))
            .unwrap_or_default();
        return (
            CheckStatus::Degraded,
            Some(format!("rate-limited {status_code}{detail}")),
        );
    }
    if !status_ok {
        return (
            CheckStatus::Down,
            Some(format!("unexpected status {status_code}")),
        );
    }
    if !body_ok {
        return (CheckStatus::Down, Some("body match failed".to_string()));
    }
    (CheckStatus::Up, None)
}

fn match_status(code: u16, expected: &ExpectedStatus) -> bool {
    match expected {
        ExpectedStatus::Exact(c) => code == *c,
        ExpectedStatus::Range { min, max } => code >= *min && code <= *max,
        ExpectedStatus::OneOf(list) => list.contains(&code),
    }
}

fn classify_hyper_error(err: &hyper_util::client::legacy::Error) -> &'static str {
    if has_timeout_in_chain(err) {
        return "timeout";
    }
    if err.is_connect() {
        return "connect";
    }
    "transport"
}

// Phrase set is narrow ("timed out" / "deadline") so `connect_timeout=…`
// in a config-field error string does not flip a connect failure to a
// timeout. Bare "timeout" substring is deliberately omitted.
fn has_timeout_in_chain(err: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(err), |e| e.source()).any(|e| {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::TimedOut
        {
            return true;
        }
        let s = e.to_string().to_ascii_lowercase();
        s.contains("timed out") || s.contains("deadline")
    })
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
    fn sanitised_retry_after_rejects_markdown_injection() {
        assert!(sanitised_retry_after("30"));
        assert!(sanitised_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"));
        assert!(!sanitised_retry_after("<https://evil.example|free>"));
        assert!(!sanitised_retry_after("*bold*"));
        assert!(!sanitised_retry_after(""));
        assert!(!sanitised_retry_after(&"a".repeat(65)));
    }

    #[test]
    fn classify_outcome_marks_429_with_retry_after_as_degraded() {
        let (s, err) = classify_outcome(429, false, true, Some("30"));
        assert!(matches!(s, CheckStatus::Degraded));
        assert_eq!(err.unwrap(), "rate-limited 429 (Retry-After: 30)");
    }

    #[test]
    fn classify_outcome_marks_503_without_retry_after_as_degraded() {
        let (s, err) = classify_outcome(503, false, true, None);
        assert!(matches!(s, CheckStatus::Degraded));
        assert_eq!(err.unwrap(), "rate-limited 503");
    }

    #[test]
    fn classify_outcome_honors_user_expecting_429() {
        let (s, err) = classify_outcome(429, true, true, Some("60"));
        assert!(matches!(s, CheckStatus::Up));
        assert!(err.is_none());
    }

    #[test]
    fn classify_outcome_real_5xx_stays_down() {
        let (s, err) = classify_outcome(500, false, true, None);
        assert!(matches!(s, CheckStatus::Down));
        assert_eq!(err.unwrap(), "unexpected status 500");
    }

    #[test]
    fn classify_outcome_body_mismatch_is_down() {
        let (s, err) = classify_outcome(200, true, false, None);
        assert!(matches!(s, CheckStatus::Down));
        assert_eq!(err.unwrap(), "body match failed");
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

    #[test]
    fn decode_body_passes_through_when_no_encoding() {
        let raw = HBytes::from_static(b"hello world");
        let out = decode_body(&raw, None).expect("identity must succeed");
        assert_eq!(out.as_ref(), b"hello world");
    }

    #[test]
    fn decode_body_rejects_gzip_bomb_over_decoded_cap() {
        use std::io::Write;
        // Tiny on-wire payload that expands past MAX_DECODED_BODY_BYTES.
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let chunk = vec![0u8; 64 * 1024];
        let mut written = 0usize;
        while written <= MAX_DECODED_BODY_BYTES + chunk.len() {
            encoder.write_all(&chunk).unwrap();
            written += chunk.len();
        }
        let compressed = encoder.finish().unwrap();
        let raw = HBytes::from(compressed);
        let err = decode_body(&raw, Some("gzip")).expect_err("bomb must be rejected");
        assert!(
            err.to_string().contains("exceeded"),
            "expected exceeded error, got: {err}"
        );
    }

    #[derive(Debug)]
    struct ChainErr {
        msg: &'static str,
        src: Option<Box<dyn std::error::Error + 'static>>,
    }
    impl std::fmt::Display for ChainErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.msg)
        }
    }
    impl std::error::Error for ChainErr {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.src.as_deref()
        }
    }

    #[test]
    fn has_timeout_in_chain_matches_root_message() {
        let e = ChainErr {
            msg: "operation timed out",
            src: None,
        };
        assert!(has_timeout_in_chain(&e));
    }

    #[test]
    fn has_timeout_in_chain_walks_to_source() {
        let leaf = ChainErr {
            msg: "deadline elapsed",
            src: None,
        };
        let root = ChainErr {
            msg: "hyper error",
            src: Some(Box::new(leaf)),
        };
        assert!(has_timeout_in_chain(&root));
    }

    #[test]
    fn has_timeout_in_chain_returns_false_without_marker() {
        let leaf = ChainErr {
            msg: "connection reset",
            src: None,
        };
        let root = ChainErr {
            msg: "transport error",
            src: Some(Box::new(leaf)),
        };
        assert!(!has_timeout_in_chain(&root));
    }

    #[test]
    fn has_timeout_in_chain_does_not_match_config_field_substring() {
        // `connect_timeout=5s` in an error message would have matched the
        // earlier permissive `"timeout"` substring; the narrowed phrase set
        // does not.
        let e = ChainErr {
            msg: "connect_timeout=5s exceeded budget",
            src: None,
        };
        assert!(!has_timeout_in_chain(&e));
    }

    #[test]
    fn has_timeout_in_chain_detects_io_error_kind() {
        let io = std::io::Error::new(std::io::ErrorKind::TimedOut, "x");
        let root = ChainErr {
            msg: "outer",
            src: Some(Box::new(io)),
        };
        assert!(has_timeout_in_chain(&root));
    }

    #[test]
    fn decode_body_accepts_gzip_within_cap() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder
            .write_all(b"compressible compressible compressible")
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let raw = HBytes::from(compressed);
        let out = decode_body(&raw, Some("gzip")).expect("small gzip must succeed");
        assert!(out.starts_with(b"compressible"));
    }
}
