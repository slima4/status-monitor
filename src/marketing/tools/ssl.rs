//! SSL certificate checker: reads a public host's leaf certificate and reports
//! what it says.
//!
//! The only marketing surface that opens an outbound socket. Every other tool
//! either computes in the browser or renders a static page, so the guards that
//! make this safe live here rather than in a shared layer: a hostname parser
//! that refuses anything but a public DNS name, an SSRF filter over the
//! resolved addresses, a port allowlist, a per-IP rate limit and a hard
//! deadline. The certificate read itself is `security::cert_probe`, shared with
//! the `tls_cert` monitor so the two can never disagree about a chain.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use askama::Template;
use askama_web::WebTemplate;
use axum::Extension;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use hickory_resolver::{Resolver, TokioResolver};
use serde::{Deserialize, Serialize};

use crate::marketing::seo::{
    JsonLd, OpenGraph, json_ld_breadcrumb, json_ld_faqpage, json_ld_web_application,
    json_ld_webpage,
};
use crate::security::SsrfGuard;
use crate::security::cert_probe::{self, CertProbeError};
use crate::web::filters;

use super::super::config::{BRAND, MarketingCfg};
use super::super::pages::{CachedRender, cached_render, serve_cached};
use super::TOOL_CACHE_CONTROL;

pub const SSL_CHECKER_PATH: &str = "/tools/ssl-certificate-checker";
pub const SSL_PROBE_PATH: &str = "/tools/ssl-certificate-checker/probe";
const SSL_CHECKER_CREATED: &str = "2026-09-04";
pub const SSL_CHECKER_LASTMOD: &str = "2026-09-04";
pub const SSL_CHECKER_TITLE: &str = "SSL Certificate Checker: Expiry and Chain";
pub const SSL_CHECKER_DESCRIPTION: &str = "Read any public host's TLS certificate: days until expiry, who issued it, which names it covers and whether the chain is complete. Free, no sign-up.";

/// Ports that speak TLS immediately on connect. Without this list the endpoint
/// is a port scanner that reports which ports accept a connection.
const ALLOWED_PORTS: &[u16] = &[443, 465, 563, 636, 853, 989, 990, 993, 995, 8443, 9443];

/// Per-IP budget. Caddy already caps public pages per IP at the edge; this is
/// the app-side floor for deployments that do not front the site with it.
const PROBES_PER_MIN: u32 = 10;
const PROBE_BURST: u32 = 5;

/// A handshake that has not finished by now is telling the visitor what they
/// need to know: the host is not answering.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const PROBE_DEADLINE: Duration = Duration::from_secs(9);

/// Above this many tracked addresses, drop the ones whose budget has fully
/// refilled. Bounded growth without a background janitor.
const LIMITER_HIGH_WATER: usize = 10_000;

type ProbeLimiter = RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>;

static LIMITER: LazyLock<ProbeLimiter> = LazyLock::new(|| {
    let quota = Quota::per_minute(NonZeroU32::new(PROBES_PER_MIN).expect("nonzero"))
        .allow_burst(NonZeroU32::new(PROBE_BURST).expect("nonzero"));
    RateLimiter::dashmap(quota)
});

/// Our own resolver rather than `tokio::net::lookup_host`, which is
/// `getaddrinfo` on the blocking pool and cannot be cancelled: a nameserver
/// that simply never answers would hold a pool thread long past the deadline,
/// and the pool is shared with the rest of the process.
static RESOLVER: LazyLock<Option<TokioResolver>> = LazyLock::new(|| {
    let mut builder = Resolver::builder_tokio().ok()?;
    let opts = builder.options_mut();
    opts.timeout = DNS_TIMEOUT;
    opts.attempts = 1;
    builder.build().ok()
});

const DNS_TIMEOUT: Duration = Duration::from_secs(3);

const SSL_CHECKER_FAQS: &[(&str, &str)] = &[
    (
        "How many days before expiry should I renew?",
        "Thirty days is the usual alarm and fourteen the usual panic. Let's Encrypt certificates last ninety days and renew at sixty, so a certificate sitting below thirty means the renewal has already failed at least once and nobody read the mail about it.",
    ),
    (
        "The certificate is valid but my browser still complains. Why?",
        "Almost always an incomplete chain. The server is sending its own certificate without the intermediate that links it to a trusted root. Desktop browsers often paper over it from cache, while a fresh client, a mobile app or a server-to-server call fails outright. The chain length reported here is what the server actually sent.",
    ),
    (
        "What does the hostname match mean?",
        "A certificate is issued for a set of names. If the name you asked for is not among them, every client rejects it no matter how valid the dates are. This is the usual failure after a domain is added to a load balancer but not to the certificate.",
    ),
    (
        "Do you check the whole chain against a trust store?",
        "No, and deliberately. Validation is skipped so an expired or self-signed certificate can still be read and reported instead of the handshake refusing it and losing the dates. That is the case you most need to see.",
    ),
    (
        "Can I be told before it expires instead?",
        "Yes. A TLS certificate check reads the same certificate on a schedule and alerts at the day count you choose, which is the point: an expiry is the one outage you can know about weeks ahead. The button beside the result opens a monitor prefilled with the host you just checked.",
    ),
];

#[derive(Template, WebTemplate)]
#[template(path = "marketing/tool_ssl_checker.html")]
struct SslCheckerPage {
    app_url: String,
    canonical_url: String,
    og: OpenGraph,
    breadcrumb_json_ld: JsonLd,
    web_application_json_ld: JsonLd,
    webpage_json_ld: JsonLd,
    faq_json_ld: JsonLd,
    faqs: &'static [(&'static str, &'static str)],
    probe_path: &'static str,
    version: &'static str,
}

static SSL_CHECKER_CACHED: OnceLock<CachedRender> = OnceLock::new();

pub(super) fn render(cfg: &MarketingCfg) -> CachedRender {
    let canonical_url = format!("{}{}", cfg.canonical_origin, SSL_CHECKER_PATH);
    let mut og = OpenGraph::default_for(
        &format!("{SSL_CHECKER_TITLE} | {BRAND}"),
        &canonical_url,
        &cfg.canonical_origin,
    );
    og.description = SSL_CHECKER_DESCRIPTION.to_string();
    let page = SslCheckerPage {
        app_url: cfg.app_url.clone(),
        breadcrumb_json_ld: json_ld_breadcrumb(
            &cfg.canonical_origin,
            SSL_CHECKER_TITLE,
            SSL_CHECKER_PATH,
        ),
        web_application_json_ld: json_ld_web_application(
            &cfg.canonical_origin,
            SSL_CHECKER_TITLE,
            SSL_CHECKER_PATH,
            SSL_CHECKER_DESCRIPTION,
        ),
        webpage_json_ld: json_ld_webpage(
            &cfg.canonical_origin,
            SSL_CHECKER_PATH,
            SSL_CHECKER_TITLE,
            SSL_CHECKER_CREATED,
            SSL_CHECKER_LASTMOD,
            true,
        ),
        faq_json_ld: json_ld_faqpage(SSL_CHECKER_FAQS),
        faqs: SSL_CHECKER_FAQS,
        probe_path: SSL_PROBE_PATH,
        canonical_url,
        og,
        version: env!("CARGO_PKG_VERSION"),
    };
    let body = page
        .render()
        .unwrap_or_else(|e| format!("<!-- ssl-checker render failed: {e} -->"));
    cached_render(body)
}

pub(super) fn warm(cfg: &MarketingCfg) {
    SSL_CHECKER_CACHED.get_or_init(|| render(cfg));
}

pub(super) async fn page(State(cfg): State<Arc<MarketingCfg>>, headers: HeaderMap) -> Response {
    let cached = SSL_CHECKER_CACHED.get_or_init(|| render(&cfg));
    serve_cached(&headers, cached, &TOOL_CACHE_CONTROL)
}

#[derive(Debug, Deserialize)]
pub(super) struct ProbeQuery {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Serialize)]
struct ProbeReport {
    host: String,
    port: u16,
    resolved_ip: String,
    subject_common_name: Option<String>,
    issuer_common_name: Option<String>,
    issuer_organization: Option<String>,
    not_before: String,
    not_after: String,
    days_remaining: i64,
    /// Sent alongside the day count because that count truncates toward zero:
    /// a certificate six hours dead reports as 0 days, which reads as "expires
    /// today" for something already breaking every client.
    expired: bool,
    san_dns_names: Vec<String>,
    serial: String,
    name_matches: bool,
    self_signed: bool,
    chain_len: usize,
    handshake_ms: u32,
}

pub(super) async fn probe(
    State(cfg): State<Arc<MarketingCfg>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    q: Result<Query<ProbeQuery>, QueryRejection>,
) -> Response {
    // The page's own script reads every answer as JSON, so a malformed query
    // string must not fall through to axum's plain-text rejection: the parse
    // would throw and the visitor would be told the server was unreachable.
    let Ok(Query(q)) = q else {
        return fail(
            StatusCode::BAD_REQUEST,
            "That request could not be read. Check the host and port.",
        );
    };
    let Some(host) = q.host.as_deref().and_then(clean_host) else {
        return fail(
            StatusCode::BAD_REQUEST,
            "That does not look like a domain name. Enter a hostname such as example.com.",
        );
    };
    let port = q.port.unwrap_or(443);
    if !ALLOWED_PORTS.contains(&port) {
        return fail(
            StatusCode::BAD_REQUEST,
            "That port is not one this checker will open. TLS-on-connect ports only.",
        );
    }

    // Metered after parsing, because the budget exists to bound outbound
    // sockets: a typo costs nobody a connection and should not spend a turn.
    if !spend_budget(&cfg, &headers, peer) {
        return fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many checks from this address. Wait a minute and try again.",
        );
    }

    match tokio::time::timeout(PROBE_DEADLINE, run(&host, port)).await {
        Ok(Ok(report)) => (probe_headers(), Json(report)).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => fail(
            StatusCode::GATEWAY_TIMEOUT,
            "The host did not finish a handshake in time.",
        ),
    }
}

/// Collapses an IPv6 address to its /64. Any VPS is handed a routed /64 or
/// wider, so keying on the full address would hand one visitor an unbounded
/// number of independent budgets against the only route here that opens a
/// socket.
fn budget_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// Charges one probe to the visitor's address. Missing `ConnectInfo` means the
/// server was built without it, which would silently hand everyone the same
/// budget; an unspecified address keeps them metered together rather than not
/// at all.
fn spend_budget(
    cfg: &MarketingCfg,
    headers: &HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> bool {
    let peer = peer.map_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), |c| {
        (c.0).0.ip()
    });
    let client = crate::web::client_ip::extract(headers, peer, &cfg.trusted_proxies);
    let allowed = LIMITER.check_key(&budget_key(client)).is_ok();
    if LIMITER.len() > LIMITER_HIGH_WATER {
        LIMITER.retain_recent();
    }
    allowed
}

async fn run(host: &str, port: u16) -> Result<ProbeReport, ProbeFailure> {
    let addrs = resolve(host).await?;
    let facts = cert_probe::connect_and_read(&addrs, port, host, CONNECT_TIMEOUT)
        .await
        .map_err(describe)?;

    Ok(ProbeReport {
        host: host.to_owned(),
        port,
        resolved_ip: facts.peer.to_string(),
        subject_common_name: facts.subject_common_name,
        issuer_common_name: facts.issuer_common_name,
        issuer_organization: facts.issuer_organization,
        not_before: facts.not_before.to_rfc3339(),
        not_after: facts.not_after.to_rfc3339(),
        days_remaining: facts.days_remaining,
        expired: facts.not_after <= chrono::Utc::now(),
        san_dns_names: facts.san_dns_names,
        serial: facts.serial,
        name_matches: facts.name_matches,
        self_signed: facts.self_signed,
        chain_len: facts.chain_len,
        handshake_ms: facts.handshake_ms,
    })
}

/// Resolves, then keeps only addresses the SSRF guard allows. A name that
/// resolves entirely into private space is reported as unreachable rather than
/// as blocked: the distinction is only useful to someone probing us.
async fn resolve(host: &str) -> Result<Vec<IpAddr>, ProbeFailure> {
    let Some(resolver) = RESOLVER.as_ref() else {
        return Err(ProbeFailure::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "The checker has no resolver configured.",
        ));
    };
    let Ok(Ok(resolved)) = tokio::time::timeout(DNS_TIMEOUT, resolver.lookup_ip(host)).await else {
        return Err(ProbeFailure::new(
            StatusCode::BAD_GATEWAY,
            "That name does not resolve.",
        ));
    };
    let guard = SsrfGuard::strict();
    let addrs: Vec<IpAddr> = resolved.iter().filter(|ip| guard.allow(*ip)).collect();
    if addrs.is_empty() {
        return Err(ProbeFailure::new(
            StatusCode::BAD_GATEWAY,
            "That name does not resolve to a reachable public address.",
        ));
    }
    Ok(addrs)
}

/// Probe errors carry a platform errno and a rustls internal, neither of which
/// is meaningful to a visitor. Each becomes a sentence about the host.
fn describe(err: CertProbeError) -> ProbeFailure {
    let text = match err {
        CertProbeError::Connect(_) => "Nothing accepted a connection on that port.",
        CertProbeError::Handshake(_) => {
            "The host accepted the connection but did not complete a TLS handshake. It may not speak TLS on that port."
        }
        CertProbeError::NoChain => "The host completed a handshake without sending a certificate.",
        CertProbeError::Parse(_) => "The host sent a certificate that could not be parsed.",
        CertProbeError::Validity => "The certificate carries dates outside any usable range.",
        CertProbeError::ServerName(_) => "That name cannot be used in a TLS handshake.",
        CertProbeError::Timeout => "The host did not finish a handshake in time.",
    };
    ProbeFailure::new(StatusCode::BAD_GATEWAY, text)
}

struct ProbeFailure {
    status: StatusCode,
    error: &'static str,
}

impl ProbeFailure {
    fn new(status: StatusCode, error: &'static str) -> Self {
        Self { status, error }
    }
}

impl IntoResponse for ProbeFailure {
    fn into_response(self) -> Response {
        fail(self.status, self.error)
    }
}

fn fail(status: StatusCode, error: &'static str) -> Response {
    #[derive(Serialize)]
    struct Failure<'a> {
        error: &'a str,
    }
    (status, probe_headers(), Json(Failure { error })).into_response()
}

/// Never cached and never indexed: the answer is per-host and changes the day a
/// certificate is renewed.
fn probe_headers() -> [(header::HeaderName, HeaderValue); 2] {
    [
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, private"),
        ),
        (
            header::HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("noindex"),
        ),
    ]
}

/// Accepts what a person pastes — a full URL, a trailing dot, mixed case — and
/// yields a bare DNS name, or nothing. Address literals are refused: they carry
/// no SNI, and allowing them would turn the resolver guard into the only thing
/// standing between this endpoint and the internal network.
fn clean_host(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    if let Some((_, rest)) = s.split_once("://") {
        s = rest;
    }
    s = s
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .trim_end_matches('.');
    if s.is_empty() || s.contains(':') {
        return None;
    }

    // An internationalized name is a real name with a real certificate, and
    // the /start flow already accepts one through `Url::parse`. Fold it to
    // punycode here so both surfaces agree on what a domain is; the label
    // rules below then run on the ASCII form either way.
    let host = if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        idna::domain_to_ascii(s).ok()?
    };
    if host.len() > 253 {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    let well_formed = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    // An all-digit final label is an IPv4 literal, never a registrable name.
    let is_v4_literal = labels
        .last()
        .is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
    (well_formed && !is_v4_literal).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_what_a_person_pastes() {
        assert_eq!(
            clean_host("https://Acme.com/pricing?x=1").as_deref(),
            Some("acme.com")
        );
        assert_eq!(clean_host("  acme.com.  ").as_deref(), Some("acme.com"));
        assert_eq!(
            clean_host("mail.acme.co.uk").as_deref(),
            Some("mail.acme.co.uk")
        );
    }

    #[test]
    fn refuses_literals_and_malformed_names() {
        for raw in [
            "127.0.0.1",
            "192.168.1.10",
            "[::1]",
            "localhost",
            "acme.com:8443",
            "-acme.com",
            "acme..com",
            "",
        ] {
            assert!(clean_host(raw).is_none(), "{raw} should be refused");
        }
    }

    /// Same answer the /start flow gives, which folds through `Url::parse`.
    #[test]
    fn folds_an_internationalized_name_to_punycode() {
        assert_eq!(
            clean_host("münchen.de").as_deref(),
            Some("xn--mnchen-3ya.de")
        );
    }

    #[test]
    fn port_allowlist_excludes_plaintext_services() {
        for port in [22, 80, 3306, 5432, 6379, 11211] {
            assert!(
                !ALLOWED_PORTS.contains(&port),
                "port {port} must not be probeable"
            );
        }
        assert!(ALLOWED_PORTS.contains(&443));
    }
}
