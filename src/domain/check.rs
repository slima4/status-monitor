use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum CheckSpec {
    Http(HttpCheck),
    Tcp(TcpCheck),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExpectedStatus {
    Exact(u16),
    Range { min: u16, max: u16 },
    OneOf(Vec<u16>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpCheck {
    pub url: Url,
    pub method: HttpMethod,
    #[serde(with = "duration_ms")]
    pub timeout: Duration,
    pub follow_redirects: bool,
    pub max_redirects: u8,
    pub expected_status: ExpectedStatus,
    pub expected_body_contains: Option<String>,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub verify_tls: bool,
    pub basic_auth: Option<(String, String)>,
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpCheck {
    pub host: String,
    pub port: u16,
    #[serde(with = "duration_ms")]
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
