use std::net::SocketAddr;
use std::time::Instant;

use chrono::Utc;
use tokio::net::TcpStream;
use tokio::time::timeout;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, TcpCheck};
use crate::http_client::HttpClients;

pub async fn execute_tcp_check(
    target_id: Uuid,
    check: &TcpCheck,
    clients: &HttpClients,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let outcome = timeout(check.timeout, connect_via_guard(check, clients)).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match outcome {
        Ok(Ok(_stream)) => CheckResult {
            target_id,
            timestamp: started_at,
            status: CheckStatus::Up,
            duration_ms,
            dns_ms: None,
            connect_ms: Some(duration_ms as u16),
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: None,
        },
        Ok(Err(err)) => CheckResult {
            target_id,
            timestamp: started_at,
            status: CheckStatus::Down,
            duration_ms,
            dns_ms: None,
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: None,
            error: Some(err.to_string()),
        },
        Err(_) => CheckResult::error_with_elapsed(target_id, started_at, duration_ms, "timeout"),
    }
}

async fn connect_via_guard(check: &TcpCheck, clients: &HttpClients) -> anyhow::Result<TcpStream> {
    let guard = clients.ssrf_guard();
    let mut last_err: Option<std::io::Error> = None;
    let mut tried = false;
    for ip in clients.resolver().resolve_addrs(&check.host).await? {
        if !guard.allow(ip) {
            continue;
        }
        tried = true;
        match TcpStream::connect(SocketAddr::new(ip, check.port)).await {
            Ok(s) => return Ok(s),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e.into());
    }
    if tried {
        Err(anyhow::anyhow!("no addresses for {}", check.host))
    } else {
        Err(anyhow::anyhow!("no allowed addresses for {}", check.host))
    }
}
