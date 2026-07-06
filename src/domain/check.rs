use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum CheckSpec {
    Http(HttpCheck),
    Tcp(TcpCheck),
    Ping(PingCheck),
    Heartbeat(HeartbeatCheck),
    TlsCert(TlsCertCheck),
    DomainExpiry(DomainExpiryCheck),
    Dns(DnsCheck),
}

impl CheckSpec {
    /// Every kind string `kind()` can return. Bounded set — safe as a metric
    /// label and lets inventory emit a 0 for kinds with no enabled monitors.
    pub const ALL_KINDS: [&'static str; 7] = [
        "http",
        "tcp",
        "ping",
        "heartbeat",
        "dns",
        "tls_cert",
        "domain_expiry",
    ];

    pub fn kind(&self) -> &'static str {
        match self {
            CheckSpec::Http(_) => "http",
            CheckSpec::Tcp(_) => "tcp",
            CheckSpec::Ping(_) => "ping",
            CheckSpec::Heartbeat(_) => "heartbeat",
            CheckSpec::Dns(_) => "dns",
            CheckSpec::TlsCert(_) => "tls_cert",
            CheckSpec::DomainExpiry(_) => "domain_expiry",
        }
    }

    /// A passive kind evaluates in-memory state instead of probing the
    /// network: no circuit breaker, no host throttle, never runs on agents.
    pub fn is_passive(&self) -> bool {
        matches!(self, CheckSpec::Heartbeat(_))
    }
}

/// Per-kind check-interval floor. Expiry state (tls_cert / domain_expiry)
/// moves slowly, so hourly minimum. Heartbeat's interval is its evaluation
/// cadence, which can't be finer than the grace it judges, so a minute floor.
pub fn min_interval_secs_for_kind(kind: &str) -> u64 {
    match kind {
        "tls_cert" | "domain_expiry" => 3_600,
        "heartbeat" => 60,
        _ => 10,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExpectedStatus {
    Exact(u16),
    Range {
        #[schema(minimum = 100, maximum = 599)]
        min: u16,
        #[schema(minimum = 100, maximum = 599)]
        max: u16,
    },
    OneOf(Vec<u16>),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HttpCheck {
    #[schema(value_type = String, format = "uri", example = "https://example.com/healthz")]
    pub url: Url,
    pub method: HttpMethod,
    /// Request timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, minimum = 100, maximum = 60000, example = 5000)]
    pub timeout: Duration,
    pub follow_redirects: bool,
    #[schema(maximum = 10)]
    pub max_redirects: u8,
    pub expected_status: ExpectedStatus,
    #[schema(nullable = true)]
    pub expected_body_contains: Option<String>,
    pub headers: HashMap<String, String>,
    #[schema(nullable = true)]
    pub body: Option<String>,
    pub verify_tls: bool,
    /// On read, returns `["***","***"]` if set. On write, send real values or omit the field.
    #[schema(value_type = Option<[String; 2]>, nullable = true)]
    pub basic_auth: Option<(String, String)>,
    /// On read, returns `"***"` if set. On write, send real value or omit the field.
    #[schema(nullable = true)]
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TcpCheck {
    #[schema(example = "db.example.com")]
    pub host: String,
    #[schema(minimum = 1, maximum = 65535, example = 5432)]
    pub port: u16,
    /// Connect timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, minimum = 100, maximum = 60000, example = 3000)]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PingCheck {
    #[schema(example = "gateway.example.com")]
    pub host: String,
    /// Echo-reply timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, minimum = 100, maximum = 60000, example = 3000)]
    pub timeout: Duration,
}

/// Inbound dead-man's-switch: the customer's system pings a token URL; the
/// scheduled evaluation opens an incident once the last ping is older than
/// `period + grace`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HeartbeatCheck {
    /// Expected ping cadence in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, minimum = 60000, example = 300000)]
    pub period: Duration,
    /// Extra allowance past `period` before the monitor counts as down,
    /// in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, example = 60000)]
    pub grace: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TlsCertCheck {
    pub host: String,
    #[schema(minimum = 1, maximum = 65535)]
    pub port: u16,
    /// SNI to send if different from `host` (e.g. when the cert is served
    /// against a virtual host name).
    #[serde(default)]
    #[schema(nullable = true)]
    pub server_name: Option<String>,
    pub warn_days: u32,
    pub critical_days: u32,
    /// Connect timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64)]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DomainExpiryCheck {
    pub domain: String,
    pub warn_days: u32,
    pub critical_days: u32,
    /// Query timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64)]
    pub timeout: Duration,
}

impl DomainExpiryCheck {
    /// Registrable domain to surface in the UI, only when it actually reduces
    /// the input (`app.example.co.uk` → `example.co.uk`); an apex yields `None`.
    pub fn reduced_domain_hint(&self) -> Option<String> {
        reduced_domain_hint(&self.domain)
    }
}

/// Registrable domain (public suffix + one label) for the RDAP query. An
/// unrecognised suffix falls through normalised so the registry returns a
/// precise error instead of a silent wrong lookup.
pub fn registered_domain(domain: &str) -> String {
    resolve_registrable(&normalize_domain(domain))
}

/// Registrable domain only when it differs from the normalised input — the
/// signal that a real subdomain was reduced, for UI hints (mixed-case or a
/// trailing dot alone is not a reduction).
pub fn reduced_domain_hint(domain: &str) -> Option<String> {
    let normalized = normalize_domain(domain);
    let registered = resolve_registrable(&normalized);
    (registered != normalized).then_some(registered)
}

fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn resolve_registrable(normalized: &str) -> String {
    psl::domain_str(normalized)
        .map(str::to_owned)
        .unwrap_or_else(|| normalized.to_owned())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    A,
    Aaaa,
    Cname,
    Mx,
    Ns,
    Txt,
    Soa,
    Ptr,
    Caa,
    Srv,
}

impl DnsRecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            DnsRecordType::A => "A",
            DnsRecordType::Aaaa => "AAAA",
            DnsRecordType::Cname => "CNAME",
            DnsRecordType::Mx => "MX",
            DnsRecordType::Ns => "NS",
            DnsRecordType::Txt => "TXT",
            DnsRecordType::Soa => "SOA",
            DnsRecordType::Ptr => "PTR",
            DnsRecordType::Caa => "CAA",
            DnsRecordType::Srv => "SRV",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DnsCheck {
    /// Name to resolve (FQDN; trailing dot tolerated).
    #[schema(example = "api.example.com")]
    pub domain: String,
    pub record_type: DnsRecordType,
    /// Optional custom resolver as `ip` or `ip:port` (e.g. `1.1.1.1`,
    /// `8.8.8.8:53`). `None` uses the process default resolver.
    #[serde(default)]
    #[schema(nullable = true, example = "1.1.1.1")]
    pub resolver: Option<String>,
    /// Optional substring that must appear in at least one answer value.
    /// Empty answers, NXDOMAIN, or a missing substring all fail the check.
    #[serde(default)]
    #[schema(nullable = true, example = "192.0.2.1")]
    pub expected_contains: Option<String>,
    /// Query timeout in milliseconds.
    #[serde(with = "duration_ms")]
    #[schema(value_type = u64, minimum = 100, maximum = 60000, example = 3000)]
    pub timeout: Duration,
}

mod duration_ms {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let ms = u64::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Adding a CheckSpec variant breaks this exhaustive match, forcing ALL_KINDS
    // (and the count asserted below) to be updated in the same change.
    #[allow(dead_code)]
    fn variant_guard(spec: &CheckSpec) {
        match spec {
            CheckSpec::Http(_)
            | CheckSpec::Tcp(_)
            | CheckSpec::Ping(_)
            | CheckSpec::Heartbeat(_)
            | CheckSpec::Dns(_)
            | CheckSpec::TlsCert(_)
            | CheckSpec::DomainExpiry(_) => {}
        }
    }

    #[test]
    fn all_kinds_unique_and_complete() {
        let mut seen = std::collections::HashSet::new();
        for k in CheckSpec::ALL_KINDS {
            assert!(seen.insert(k), "duplicate kind in ALL_KINDS: {k}");
        }
        assert_eq!(CheckSpec::ALL_KINDS.len(), 7);
    }

    #[test]
    fn registered_domain_reduces_subdomains() {
        assert_eq!(registered_domain("app.example.dev"), "example.dev");
        assert_eq!(registered_domain("example.dev"), "example.dev");
        assert_eq!(registered_domain("fra.my-app.com"), "my-app.com");
    }

    #[test]
    fn registered_domain_keeps_multi_level_suffixes_intact() {
        assert_eq!(registered_domain("shop.com.ua"), "shop.com.ua");
        assert_eq!(registered_domain("www.shop.com.ua"), "shop.com.ua");
        assert_eq!(registered_domain("sub.example.co.uk"), "example.co.uk");
    }

    #[test]
    fn registered_domain_normalises_case_and_trailing_dot() {
        assert_eq!(registered_domain("APP.Uptimepage.DEV."), "uptimepage.dev");
    }

    #[test]
    fn reduced_domain_hint_none_for_apex_or_normalisation_only() {
        assert_eq!(reduced_domain_hint("example.com"), None);
        assert_eq!(reduced_domain_hint("Example.com."), None);
        assert_eq!(reduced_domain_hint("  example.co.uk  "), None);
    }

    #[test]
    fn reduced_domain_hint_some_for_real_subdomain() {
        assert_eq!(
            reduced_domain_hint("app.example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            reduced_domain_hint("www.shop.com.ua").as_deref(),
            Some("shop.com.ua")
        );
    }
}
