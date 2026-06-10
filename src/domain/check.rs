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
    TlsCert(TlsCertCheck),
    DomainExpiry(DomainExpiryCheck),
    Dns(DnsCheck),
}

impl CheckSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            CheckSpec::Http(_) => "http",
            CheckSpec::Tcp(_) => "tcp",
            CheckSpec::Dns(_) => "dns",
            CheckSpec::TlsCert(_) => "tls_cert",
            CheckSpec::DomainExpiry(_) => "domain_expiry",
        }
    }
}

/// Per-kind check-interval floor: expiry state (tls_cert / domain_expiry)
/// moves slowly, so those probes are hourly at minimum. The API validates
/// against it and the monitor form surfaces it to the client.
pub fn min_interval_secs_for_kind(kind: &str) -> u64 {
    match kind {
        "tls_cert" | "domain_expiry" => 3_600,
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
