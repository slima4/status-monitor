use std::time::Instant;

use chrono::Utc;
use tokio::net::TcpStream;
use tokio::time::timeout;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, TcpCheck};

pub async fn execute_tcp_check(target_id: Uuid, check: &TcpCheck) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let result = timeout(
        check.timeout,
        TcpStream::connect((check.host.as_str(), check.port)),
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u32;

    match result {
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
