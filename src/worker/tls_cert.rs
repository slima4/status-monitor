use std::sync::Arc;
use std::time::Instant;

use chrono::{TimeZone, Utc};
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, ServerName};
use serde::Serialize;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use uuid::Uuid;
use x509_parser::prelude::*;

use crate::domain::{CheckResult, CheckStatus, TlsCertCheck};
use crate::http_client::HttpClients;
use crate::http_client::client::NoVerify;
use crate::worker::connect_via_guard;

pub async fn execute_tls_cert_check(
    target_id: Uuid,
    check: &TlsCertCheck,
    clients: &HttpClients,
) -> CheckResult {
    let started_at = Utc::now();
    let start = Instant::now();

    let outcome = timeout(check.timeout, run_check(check, clients)).await;
    let duration_ms = start.elapsed().as_millis() as u32;

    match outcome {
        Ok(Ok(probe)) => CheckResult {
            target_id,
            timestamp: started_at,
            status: probe.verdict.status,
            duration_ms,
            dns_ms: None,
            connect_ms: None,
            tls_ms: Some(probe.handshake_ms.min(u16::MAX as u32) as u16),
            ttfb_ms: None,
            response_code: None,
            response_size: Some(probe.verdict.details_json.len() as u32),
            // `error` doubles as the structured details payload for cert
            // checks. Up results stay None (matching every other check
            // type's convention); Degraded/Down carries the JSON document
            // documented in docs/api.md.
            error: match probe.verdict.status {
                CheckStatus::Up => None,
                _ => Some(probe.verdict.details_json),
            },
        },
        Ok(Err(err)) => CheckResult {
            target_id,
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
        Err(_) => CheckResult::error_with_elapsed(target_id, started_at, duration_ms, "timeout"),
    }
}

struct ProbeOutcome {
    verdict: CertVerdict,
    handshake_ms: u32,
}

struct CertVerdict {
    status: CheckStatus,
    details_json: String,
}

async fn run_check(check: &TlsCertCheck, clients: &HttpClients) -> anyhow::Result<ProbeOutcome> {
    let stream = connect_via_guard(&check.host, check.port, clients).await?;
    let server_name = check.server_name.as_deref().unwrap_or(&check.host);
    let dns_name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| anyhow::anyhow!("invalid server_name '{server_name}': {e}"))?;

    let tls_config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(tls_config));

    let handshake_start = Instant::now();
    let tls = connector.connect(dns_name, stream).await?;
    let handshake_ms = handshake_start.elapsed().as_millis() as u32;

    let (_io, session) = tls.get_ref();
    let chain = session
        .peer_certificates()
        .ok_or_else(|| anyhow::anyhow!("server returned no certificate chain"))?;
    let leaf = chain
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty certificate chain"))?;
    let verdict = classify_leaf(leaf, check)?;
    Ok(ProbeOutcome {
        verdict,
        handshake_ms,
    })
}

fn classify_leaf(leaf: &CertificateDer<'_>, check: &TlsCertCheck) -> anyhow::Result<CertVerdict> {
    let (_, parsed) = X509Certificate::from_der(leaf.as_ref())
        .map_err(|e| anyhow::anyhow!("parsing leaf certificate: {e}"))?;

    let not_after_ts = parsed.validity().not_after.timestamp();
    let not_after = Utc
        .timestamp_opt(not_after_ts, 0)
        .single()
        .ok_or_else(|| anyhow::anyhow!("notAfter out of range: {not_after_ts}"))?;
    let days_remaining = (not_after - Utc::now()).num_days();

    let subject_cn = first_cn(parsed.subject()).unwrap_or_else(|| "<no CN>".into());
    let issuer_cn = first_cn(parsed.issuer()).unwrap_or_else(|| "<no CN>".into());

    let status = crate::worker::classify_days(days_remaining, check.warn_days, check.critical_days);

    #[derive(Serialize)]
    struct Details<'a> {
        days_remaining: i64,
        not_after: String,
        subject_common_name: &'a str,
        issuer_common_name: &'a str,
    }
    let details_json = serde_json::to_string(&Details {
        days_remaining,
        not_after: not_after.to_rfc3339(),
        subject_common_name: &subject_cn,
        issuer_common_name: &issuer_cn,
    })
    .expect("infallible serialize for fixed struct");

    Ok(CertVerdict {
        status,
        details_json,
    })
}

fn first_cn(name: &X509Name<'_>) -> Option<String> {
    name.iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok().map(str::to_owned))
}
