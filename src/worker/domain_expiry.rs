use std::time::Instant;

use chrono::Utc;
use serde::Serialize;
use tokio::sync::OnceCell;
use tokio::time::timeout;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckStatus, DomainExpiryCheck};
use crate::http_outbound::build_outbound_client;
use crate::worker::rdap::RdapClient;

/// Shared RDAP client kept in a process-static so all domain_expiry checks
/// reuse one cached bootstrap map and one connection pool. A handle on
/// `HttpClients` would be more idiomatic, but the RDAP outbound flow
/// deliberately bypasses both the phase-timing connector and the SSRF guard
/// that `HttpClients` carries — keeping it separate avoids leaking those
/// semantics through. Built lazily on first invocation.
static RDAP: OnceCell<RdapClient> = OnceCell::const_new();

pub async fn execute_domain_expiry_check(
    target_id: Uuid,
    org_id: Uuid,
    check: &DomainExpiryCheck,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();
    let client = RDAP
        .get_or_init(|| async {
            // RDAP destinations are derived from the IANA bootstrap, not from
            // user-supplied input, so the strict guard is the correct default —
            // a registry that resolves to a private IP would be a rebinding
            // attempt against an internal target via a third-party referrer.
            RdapClient::new(build_outbound_client(crate::security::SsrfGuard::strict()))
        })
        .await;

    let outcome = timeout(check.timeout, run_check(check, client)).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match outcome {
        Ok(Ok(verdict)) => CheckResult {
            target_id,
            org_id,
            timestamp: started_at,
            status: verdict.status,
            duration_ms,
            dns_ms: None,
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
            status: CheckStatus::Error,
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
pub struct DomainVerdict {
    pub status: CheckStatus,
    pub details_json: String,
}

pub async fn run_check(
    check: &DomainExpiryCheck,
    client: &RdapClient,
) -> anyhow::Result<DomainVerdict> {
    let answer = client.lookup_expiration(&check.domain).await?;
    Ok(classify(check, answer.expiration, answer.registrar))
}

fn classify(
    check: &DomainExpiryCheck,
    expiration: chrono::DateTime<Utc>,
    registrar: Option<String>,
) -> DomainVerdict {
    let days_remaining = (expiration - Utc::now()).num_days();
    let status = crate::worker::classify_days(days_remaining, check.warn_days, check.critical_days);

    #[derive(Serialize)]
    struct Details<'a> {
        domain: &'a str,
        days_remaining: i64,
        expiration_date: String,
        registrar: Option<&'a str>,
    }
    let details_json = serde_json::to_string(&Details {
        domain: &check.domain,
        days_remaining,
        expiration_date: expiration.to_rfc3339(),
        registrar: registrar.as_deref(),
    })
    .expect("infallible serialize for fixed struct");
    DomainVerdict {
        status,
        details_json,
    }
}
