use std::time::Instant;

use chrono::Utc;
use reqwest::Method;
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

    let mut req = clients
        .pick(check.verify_tls)
        .request(map_method(check.method), check.url.clone())
        .timeout(check.timeout);

    for (k, v) in &check.headers {
        req = req.header(k, v);
    }
    if let Some((user, pass)) = &check.basic_auth {
        req = req.basic_auth(user, Some(pass));
    }
    if let Some(token) = &check.bearer_token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = &check.body {
        req = req.body(body.clone());
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            let elapsed = start.elapsed().as_millis() as u32;
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                elapsed,
                classify_reqwest_error(&err),
            );
        }
    };

    let status_code = response.status().as_u16();
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(err) => {
            let elapsed = start.elapsed().as_millis() as u32;
            return CheckResult::error_with_elapsed(
                target_id,
                started_at,
                elapsed,
                classify_reqwest_error(&err),
            );
        }
    };
    let size = bytes.len() as u32;
    let duration_ms = start.elapsed().as_millis() as u32;

    let status_ok = match_status(status_code, &check.expected_status);
    let body_ok = match &check.expected_body_contains {
        Some(needle) => std::str::from_utf8(&bytes)
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
        ttfb_ms: None,
        response_code: Some(status_code),
        response_size: Some(size),
        error,
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

fn classify_reqwest_error(err: &reqwest::Error) -> &'static str {
    if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connect"
    } else if err.is_request() {
        "request"
    } else if err.is_body() {
        "body"
    } else if err.is_decode() {
        "decode"
    } else {
        "transport"
    }
}
