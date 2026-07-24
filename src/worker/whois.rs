//! WHOIS (port 43) registration lookup, for TLDs the RDAP bootstrap misses.
//!
//! Server names come from a static table, never from the queried name. The
//! resolved addresses still go through the shared SSRF guard: the table fixes
//! which host we look up, not which IP the resolver hands back.

use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::error::{AppError, Result};
use crate::http_client::HttpClients;
use crate::worker::connect_via_guard;
use crate::worker::registration::{RegistrationAnswer, RegistrationError};

const WHOIS_PORT: u16 = 43;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Caps a hostile or broken server that would otherwise stream until timeout.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Add a TLD only after confirming its live WHOIS carries a parseable expiry.
const WHOIS_SERVERS: &[(&str, &str)] = &[
    ("co", "whois.registry.co"),
    ("it", "whois.nic.it"),
    ("la", "whois.nic.la"),
    ("me", "whois.nic.me"),
    ("nu", "whois.iis.nu"),
    ("sh", "whois.nic.sh"),
    ("so", "whois.nic.so"),
    ("st", "whois.nic.st"),
    ("us", "whois.nic.us"),
];

/// These registries omit expiry by policy, so the check can never succeed.
const NO_PUBLIC_EXPIRY: &[&str] = &["de", "eu", "gg"];

/// Ordered by preference: the registry's value beats the registrar's copy.
const EXPIRY_LABELS: &[&str] = &[
    "registry expiry date",
    "expiry date",
    "expiration date",
    "expire date",
    "registrar registration expiration date",
    "expires",
    "expire",
];

pub fn whois_server(tld: &str) -> Option<&'static str> {
    WHOIS_SERVERS
        .iter()
        .find(|(t, _)| *t == tld)
        .map(|(_, server)| *server)
}

pub fn publishes_no_expiry(tld: &str) -> bool {
    NO_PUBLIC_EXPIRY.contains(&tld)
}

#[derive(Default)]
pub struct WhoisClient;

impl WhoisClient {
    pub fn new() -> Self {
        Self
    }

    pub async fn lookup_expiration(
        &self,
        registrable: &str,
        tld: &str,
        clients: &HttpClients,
    ) -> Result<RegistrationAnswer> {
        if publishes_no_expiry(tld) {
            return Err(RegistrationError::NoPublicExpiry {
                tld: tld.to_owned(),
            }
            .into());
        }
        let server = whois_server(tld).ok_or_else(|| {
            AppError::from(RegistrationError::TldUnsupported {
                tld: tld.to_owned(),
            })
        })?;

        let body = query(server, registrable, clients).await?;
        parse_answer(&body)
    }
}

async fn query(server: &str, registrable: &str, clients: &HttpClients) -> Result<String> {
    let mut stream: TcpStream = connect_via_guard(server, WHOIS_PORT, clients)
        .await
        .with_context(|| format!("connecting to WHOIS server {server}"))?;

    let request = format!("{registrable}\r\n");
    timeout(READ_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .map_err(|_| AppError::Other(anyhow!("WHOIS write to {server} timed out")))?
        .with_context(|| format!("sending WHOIS query to {server}"))?;

    let mut buf = Vec::with_capacity(8 * 1024);
    let read = timeout(
        READ_TIMEOUT,
        stream.take(MAX_RESPONSE_BYTES as u64).read_to_end(&mut buf),
    )
    .await
    .map_err(|_| AppError::Other(anyhow!("WHOIS read from {server} timed out")))?;
    read.with_context(|| format!("reading WHOIS response from {server}"))?;

    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Takes the highest-priority label that is PRESENT, then parses it. Falling
/// through on a parse failure would silently answer with the registrar's copy
/// while the registry's own value sat there unread.
fn parse_answer(body: &str) -> Result<RegistrationAnswer> {
    let (label, raw) = EXPIRY_LABELS
        .iter()
        .find_map(|label| field(body, label).map(|v| (*label, v)))
        .ok_or_else(|| AppError::Other(anyhow!("no expiration date in WHOIS response")))?;

    let expiration = parse_timestamp(&raw)
        .ok_or_else(|| AppError::Other(anyhow!("unparseable WHOIS '{label}' value: {raw}")))?;

    Ok(RegistrationAnswer {
        expiration,
        registrar: field(body, "registrar"),
    })
}

/// Matches the label alone, so registry-specific casing and padding do not
/// matter.
fn field(body: &str, label: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.trim().eq_ignore_ascii_case(label) {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    // Registries that omit the zone designator publish UTC.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_expiry_date_and_registrar() {
        let body = "Domain Name: POSTYOURSTARTUP.CO\n\
                    Registry Expiry Date: 2027-07-22T23:59:59.0Z\n\
                    Registrar: NameCheap, Inc.\n";
        let answer = parse_answer(body).expect("parses");
        assert_eq!(answer.expiration.to_rfc3339(), "2027-07-22T23:59:59+00:00");
        assert_eq!(answer.registrar.as_deref(), Some("NameCheap, Inc."));
    }

    #[test]
    fn registry_value_wins_over_registrar_copy() {
        let body = "Registrar Registration Expiration Date: 2026-07-22T19:30:52.57Z\n\
                    Registry Expiry Date: 2027-07-22T23:59:59.0Z\n";
        let answer = parse_answer(body).expect("parses");
        assert_eq!(answer.expiration.to_rfc3339(), "2027-07-22T23:59:59+00:00");
    }

    #[test]
    fn parses_zoneless_and_date_only_forms() {
        assert!(parse_answer("expires: 2027-01-02 03:04:05\n").is_ok());
        assert!(parse_answer("expire: 2027-01-02\n").is_ok());
    }

    /// An unparseable authoritative value must fail loudly. Falling through to
    /// the registrar's copy would report a different date as if it were the
    /// registry's.
    #[test]
    fn unparseable_top_priority_value_does_not_fall_through() {
        let body = "Registry Expiry Date: 22-Jul-2027\n\
                    Registrar Registration Expiration Date: 2026-07-22T19:30:52.57Z\n";
        let err = parse_answer(body).expect_err("must not silently use the registrar copy");
        assert!(err.to_string().contains("22-Jul-2027"), "got: {err}");
    }

    /// Verbatim lines from each live registry: upstream label changes fail
    /// here rather than in production.
    #[test]
    fn parses_every_supported_registry_format() {
        let samples = [
            ("co", "Registry Expiry Date: 2027-07-22T23:59:59.0Z"),
            ("sh", "Registry Expiry Date: 2027-05-01T00:00:02Z"),
            ("so", "Registry Expiry Date: 2031-10-31T00:00:00Z"),
            ("la", "Registry Expiry Date: 2026-11-20T23:59:59.0Z"),
            ("it", "Expire Date:        2026-12-31"),
            ("st", "Expiration Date: 2035-06-19"),
            ("nu", "expires:          2032-01-17"),
            ("me", "Registry Expiry Date: 2034-04-29T17:53:02Z"),
            ("us", "Registry Expiry Date: 2027-04-17T23:59:59Z"),
        ];
        for (tld, line) in samples {
            assert!(
                parse_answer(&format!("{line}\n")).is_ok(),
                ".{tld} format no longer parses: {line}"
            );
        }
    }

    #[test]
    fn missing_expiry_is_an_error() {
        assert!(parse_answer("Domain Name: DENIC.DE\nStatus: connect\n").is_err());
    }

    #[test]
    fn label_match_is_exact_not_substring() {
        let body = "Registrar URL: https://example.test\nRegistrar: Real Registrar\n";
        assert_eq!(field(body, "registrar").as_deref(), Some("Real Registrar"));
    }

    /// Ignored so the suite stays offline; run when changing the table or parser.
    #[tokio::test]
    #[ignore]
    async fn live_lookup_returns_expiry_for_every_supported_tld() {
        let clients = crate::http_client::build_clients(
            &crate::config::HttpClientConfig {
                tcp_keepalive_secs: 30,
                user_agent: "Uptimepage/test".into(),
            },
            &crate::config::CheckerConfig {
                max_concurrent_checks: 100,
                default_timeout_ms: 5_000,
                connect_timeout_ms: 2_000,
                default_check_interval_secs: 60,
                per_host_max_inflight: tokio::sync::Semaphore::MAX_PERMITS,
                rdap_max_inflight: tokio::sync::Semaphore::MAX_PERMITS,
            },
            &crate::config::DnsConfig {
                cache_size: 64,
                positive_ttl_secs: 60,
                negative_ttl_secs: 10,
                servers: vec!["1.1.1.1:53".into()],
            },
            // Strict guard on purpose: the live registries must clear the same
            // filter production applies.
            &crate::config::SecurityConfig {
                allow_private_targets: false,
                credentials_kek_base64: secrecy::SecretString::from(String::new()),
                trusted_proxies: vec![],
            },
        )
        .expect("clients build");
        let client = WhoisClient::new();
        let floor = Utc::now() - chrono::Duration::days(365 * 5);
        for (domain, tld) in [
            ("postyourstartup.co", "co"),
            ("nic.sh", "sh"),
            ("nic.so", "so"),
            ("nic.la", "la"),
            ("nic.it", "it"),
            ("nic.st", "st"),
            ("iis.nu", "nu"),
            ("about.me", "me"),
            ("about.us", "us"),
        ] {
            let answer = client
                .lookup_expiration(domain, tld, &clients)
                .await
                .unwrap_or_else(|e| panic!("live .{tld} lookup failed: {e}"));
            assert!(answer.expiration > floor, ".{tld} expiry is implausible");
        }
    }

    #[test]
    fn server_table_covers_known_gaps_and_excludes_expiryless_registries() {
        assert_eq!(whois_server("co"), Some("whois.registry.co"));
        assert_eq!(whois_server("nu"), Some("whois.iis.nu"));
        for tld in ["de", "eu", "gg"] {
            assert!(whois_server(tld).is_none(), ".{tld} must have no server");
            assert!(publishes_no_expiry(tld), ".{tld} publishes no expiry");
        }
        assert!(!publishes_no_expiry("co"));
    }
}
