use std::str::FromStr;
use std::time::Instant;

use chrono::Utc;
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RecordType;
use serde::Serialize;
use tokio::time::timeout;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, DnsCheck};
use crate::http_client::{HttpClients, build_single_resolver};

pub async fn execute_dns_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &DnsCheck,
    clients: &HttpClients,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let outcome = timeout(check.timeout, run_check(check, clients)).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match outcome {
        Ok(Ok(verdict)) => CheckResult {
            target_id,
            org_id,
            timestamp: started_at,
            status: verdict.status,
            duration_ms,
            dns_ms: Some(duration_ms.min(u16::MAX as u32) as u16),
            connect_ms: None,
            tls_ms: None,
            ttfb_ms: None,
            response_code: None,
            response_size: Some(verdict.details_json.len() as u32),
            error: match verdict.status {
                CheckStatus::Up => None,
                _ => Some(verdict.details_json),
            },
        },
        Ok(Err(err)) => CheckResult {
            target_id,
            org_id,
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
        Err(_) => {
            CheckResult::error_with_elapsed(target_id, org_id, started_at, duration_ms, "timeout")
        }
    }
}

#[derive(Debug)]
pub struct DnsVerdict {
    pub status: CheckStatus,
    pub details_json: String,
}

async fn run_check(check: &DnsCheck, clients: &HttpClients) -> anyhow::Result<DnsVerdict> {
    // Custom-resolver path rebuilds a single-server resolver on every
    // tick *by design*: a check pointed at `1.1.1.1` must measure that
    // server's view, not the shared cache's. Don't "optimise" by
    // caching — it silently masks the failure modes the check exists to
    // detect.
    let owned_resolver;
    let resolver: &TokioResolver = match &check.resolver {
        Some(addr) => {
            owned_resolver = build_single_resolver(addr, check.timeout)?;
            &owned_resolver
        }
        None => clients.resolver().inner(),
    };

    let record_type = RecordType::from_str(check.record_type.as_str())
        .map_err(|e| anyhow::anyhow!("invalid record type: {e}"))?;
    let lookup = match resolver.lookup(check.domain.as_str(), record_type).await {
        Ok(l) => l,
        Err(e) if e.is_nx_domain() || e.is_no_records_found() => {
            return Ok(classify(check, &[]));
        }
        Err(e) => return Err(e.into()),
    };
    let answers: Vec<String> = lookup
        .answers()
        .iter()
        .map(|r| r.data.to_string())
        .collect();
    Ok(classify(check, &answers))
}

fn classify(check: &DnsCheck, answers: &[String]) -> DnsVerdict {
    let matched: Option<bool> = check
        .expected_contains
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|needle| answers.iter().any(|a| a.contains(needle)));

    // Empty answer set is Down regardless of expected_contains — that's
    // the same signal as NXDOMAIN; treating it as Up would let the check
    // silently pass after a record removal.
    let status = if answers.is_empty() {
        CheckStatus::Down
    } else {
        match matched {
            Some(false) => CheckStatus::Degraded,
            _ => CheckStatus::Up,
        }
    };

    #[derive(Serialize)]
    struct Details<'a> {
        domain: &'a str,
        record_type: &'a str,
        answers: &'a [String],
        expected_contains: Option<&'a str>,
        matched: Option<bool>,
    }
    let details_json = serde_json::to_string(&Details {
        domain: &check.domain,
        record_type: check.record_type.as_str(),
        answers,
        expected_contains: check.expected_contains.as_deref(),
        matched,
    })
    .expect("infallible serialize for fixed struct");
    DnsVerdict {
        status,
        details_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DnsRecordType;
    use std::time::Duration;

    fn check(expected: Option<&str>) -> DnsCheck {
        DnsCheck {
            domain: "example.com".into(),
            record_type: DnsRecordType::A,
            resolver: None,
            expected_contains: expected.map(str::to_string),
            timeout: Duration::from_secs(3),
        }
    }

    #[test]
    fn empty_answers_are_down() {
        let v = classify(&check(None), &[]);
        assert_eq!(v.status, CheckStatus::Down);
    }

    #[test]
    fn answers_without_expectation_are_up() {
        let v = classify(&check(None), &["192.0.2.1".into()]);
        assert_eq!(v.status, CheckStatus::Up);
    }

    #[test]
    fn matching_substring_is_up() {
        let v = classify(&check(Some("192.0.2.1")), &["192.0.2.1".into()]);
        assert_eq!(v.status, CheckStatus::Up);
    }

    #[test]
    fn mismatched_substring_is_degraded() {
        let v = classify(&check(Some("203.0.113.7")), &["192.0.2.1".into()]);
        assert_eq!(v.status, CheckStatus::Degraded);
    }

    #[test]
    fn empty_expected_string_is_ignored() {
        let v = classify(&check(Some("")), &["192.0.2.1".into()]);
        assert_eq!(v.status, CheckStatus::Up);
    }

    #[test]
    fn details_matched_is_null_when_no_expectation() {
        let v = classify(&check(None), &[]);
        assert!(
            v.details_json.contains("\"matched\":null"),
            "got: {}",
            v.details_json
        );
    }

    #[test]
    fn details_matched_is_true_when_substring_present() {
        let v = classify(&check(Some("192.0.2.1")), &["192.0.2.1".into()]);
        assert!(
            v.details_json.contains("\"matched\":true"),
            "got: {}",
            v.details_json
        );
    }

    #[test]
    fn details_matched_is_false_when_substring_absent() {
        let v = classify(&check(Some("203.0.113.7")), &["192.0.2.1".into()]);
        assert!(
            v.details_json.contains("\"matched\":false"),
            "got: {}",
            v.details_json
        );
    }
}
