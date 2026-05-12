use std::time::Instant;

use bytes::Bytes;
use chrono::Utc;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes as HBytes;
use hyper::header::{ACCEPT_ENCODING, AUTHORIZATION, HOST, HeaderName, HeaderValue, USER_AGENT};
use hyper::{Method, Request, Uri};
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, ExpectedStatus, HttpCheck, HttpMethod};
use crate::http_client::HttpClients;

pub async fn execute_http_check(
    target_id: Uuid,
    check: &HttpCheck,
    clients: &HttpClients,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let client = clients.pick(check.verify_tls);
    let _active = clients.pool_stats().inflight_guard();

    let uri: Uri = match check.url.as_str().parse() {
        Ok(u) => u,
        Err(_) => {
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "invalid url",
            );
        }
    };

    let body = check.body.clone().unwrap_or_default();
    let body_bytes: HBytes = HBytes::from(body.into_bytes());

    let mut builder = Request::builder()
        .method(map_method(check.method))
        .uri(&uri);

    if let Some(host_hdr) = host_header(&uri) {
        builder = builder.header(HOST, host_hdr);
    }
    builder = builder
        .header(USER_AGENT, clients.user_agent())
        .header(ACCEPT_ENCODING, "gzip, br");

    for (k, v) in &check.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(k.as_str()), HeaderValue::try_from(v)) {
            builder = builder.header(name, val);
        }
    }
    if let Some((user, pass)) = &check.basic_auth {
        let raw = format!("{user}:{pass}");
        let encoded = base64_encode(raw.as_bytes());
        if let Ok(val) = HeaderValue::try_from(format!("Basic {encoded}")) {
            builder = builder.header(AUTHORIZATION, val);
        }
    }
    if let Some(token) = &check.bearer_token
        && let Ok(val) = HeaderValue::try_from(format!("Bearer {token}"))
    {
        builder = builder.header(AUTHORIZATION, val);
    }

    let req = match builder.body(Full::new(body_bytes)) {
        Ok(r) => r,
        Err(err) => {
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                format!("request build failed: {err}"),
            );
        }
    };

    let send_fut = client.request(req);
    let response = match tokio::time::timeout(check.timeout, send_fut).await {
        Err(_) => {
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                "timeout",
            );
        }
        Ok(Ok(r)) => r,
        Ok(Err(err)) => {
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                classify_hyper_error(&err),
            );
        }
    };

    let ttfb_elapsed_ms = start.elapsed().as_millis();
    clients.ttfb_ms.record(ttfb_elapsed_ms as f64);
    let ttfb_ms = ttfb_elapsed_ms.min(u16::MAX as u128) as u16;

    let status_code = response.status().as_u16();
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
                started_at,
                start.elapsed().as_millis() as u32,
                "body timeout",
            );
            r.ttfb_ms = Some(ttfb_ms);
            return r;
        }
        Ok(Ok(c)) => c,
        Ok(Err(err)) => {
            let mut r = CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                format!("body read failed: {err}"),
            );
            r.ttfb_ms = Some(ttfb_ms);
            return r;
        }
    };

    let raw = collected.to_bytes();
    let decoded = match decode_body(&raw, content_encoding.as_deref()) {
        Ok(b) => b,
        Err(err) => {
            let mut r = CheckResult::error_with_elapsed(
                target_id,
                started_at,
                start.elapsed().as_millis() as u32,
                format!("decode failed: {err}"),
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

fn decode_body(raw: &HBytes, encoding: Option<&str>) -> std::io::Result<Bytes> {
    use std::io::Read;
    match encoding {
        Some("gzip") => {
            let mut decoder = GzDecoder::new(raw.as_ref());
            let mut out = Vec::with_capacity(raw.len());
            decoder.read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        Some("br") => {
            let mut out = Vec::with_capacity(raw.len());
            let mut decoder = brotli::Decompressor::new(raw.as_ref(), 4096);
            decoder.read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        _ => Ok(raw.clone()),
    }
}

fn host_header(uri: &Uri) -> Option<String> {
    let host = uri.host()?;
    let scheme = uri.scheme_str().unwrap_or("http");
    let default_port = if scheme == "https" { 443 } else { 80 };
    match uri.port_u16() {
        Some(p) if p != default_port => Some(format!("{host}:{p}")),
        _ => Some(host.to_string()),
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
        "connect"
    } else {
        "transport"
    }
}

fn base64_encode(input: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHA[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHA[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
