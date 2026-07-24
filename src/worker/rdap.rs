use std::collections::HashMap;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::OnceCell;
use url::Url;

use crate::error::Result;
use crate::http_outbound::{OutboundHttpClient, get_json};
use crate::worker::registration::{RegistrationAnswer, RegistrationError};

/// IANA-published RDAP bootstrap registry for DNS. Maps TLDs to one or more
/// RDAP server base URLs. Public and rarely changes.
const IANA_BOOTSTRAP_URL: &str = "https://data.iana.org/rdap/dns.json";

/// Checked before the bootstrap, so a gap there cannot strand a live TLD.
const RDAP_OVERRIDES: &[(&str, &str)] = &[("io", "https://rdap.identitydigital.services/rdap/")];

pub struct RdapClient {
    http: OutboundHttpClient,
    bootstrap_url: String,
    bootstrap: OnceCell<BootstrapMap>,
}

impl RdapClient {
    pub fn new(http: OutboundHttpClient) -> Self {
        Self {
            http,
            bootstrap_url: IANA_BOOTSTRAP_URL.to_owned(),
            bootstrap: OnceCell::new(),
        }
    }

    /// Constructor used by integration tests to point the client at a fixture
    /// bootstrap registry hosted by a local axum server. Not `#[cfg(test)]`
    /// because integration tests live in separate binaries that don't see the
    /// crate's test cfg.
    #[doc(hidden)]
    pub fn with_bootstrap_url(http: OutboundHttpClient, bootstrap_url: String) -> Self {
        Self {
            http,
            bootstrap_url,
            bootstrap: OnceCell::new(),
        }
    }

    /// `registrable` is the public suffix plus one label: expiry is a property
    /// of that name, not of a subdomain. The caller reduces it.
    pub async fn lookup_expiration(
        &self,
        registrable: &str,
        tld: &str,
    ) -> Result<RegistrationAnswer> {
        let base = match rdap_override(tld) {
            Some(base) => base.to_owned(),
            None => self
                .bootstrap()
                .await?
                .by_tld
                .get(tld)
                .cloned()
                .ok_or_else(|| {
                    crate::error::AppError::from(RegistrationError::TldUnsupported {
                        tld: tld.to_owned(),
                    })
                })?,
        };
        let mut url =
            Url::parse(&base).with_context(|| format!("invalid RDAP base url '{base}'"))?;
        // RDAP base URLs are typically registered with a trailing slash; the
        // path is `domain/<name>`. Normalise either way.
        if !url.path().ends_with('/') {
            let new_path = format!("{}/", url.path());
            url.set_path(&new_path);
        }
        let url = url
            .join(&format!("domain/{registrable}"))
            .with_context(|| format!("building RDAP request for '{registrable}'"))?;

        let resp: RdapDomainResponse = get_json(&self.http, &url).await?;
        parse_answer(&resp).ok_or_else(|| {
            crate::error::AppError::Other(anyhow!("no expiration event in RDAP response"))
        })
    }

    async fn bootstrap(&self) -> Result<&BootstrapMap> {
        self.bootstrap
            .get_or_try_init(|| async { fetch_bootstrap(&self.http, &self.bootstrap_url).await })
            .await
    }
}

fn rdap_override(tld: &str) -> Option<&'static str> {
    RDAP_OVERRIDES
        .iter()
        .find(|(t, _)| *t == tld)
        .map(|(_, base)| *base)
}

#[derive(Debug)]
pub struct BootstrapMap {
    by_tld: HashMap<String, String>,
}

#[derive(Deserialize)]
struct RawBootstrap {
    services: Vec<(Vec<String>, Vec<String>)>,
}

async fn fetch_bootstrap(client: &OutboundHttpClient, url: &str) -> Result<BootstrapMap> {
    let parsed = Url::parse(url).context("parsing IANA bootstrap URL")?;
    let raw: RawBootstrap = get_json(client, &parsed).await?;
    let mut by_tld = HashMap::with_capacity(raw.services.len() * 2);
    for (tlds, servers) in raw.services {
        let Some(server) = servers.into_iter().next() else {
            continue;
        };
        for tld in tlds {
            by_tld.insert(tld.to_ascii_lowercase(), server.clone());
        }
    }
    Ok(BootstrapMap { by_tld })
}

#[derive(Debug, Deserialize)]
struct RdapDomainResponse {
    #[serde(default)]
    events: Vec<RdapEvent>,
    #[serde(default)]
    entities: Vec<RdapEntity>,
}

#[derive(Debug, Deserialize)]
struct RdapEvent {
    #[serde(rename = "eventAction")]
    event_action: String,
    #[serde(rename = "eventDate")]
    event_date: String,
}

#[derive(Debug, Deserialize)]
struct RdapEntity {
    #[serde(default)]
    roles: Vec<String>,
    #[serde(rename = "vcardArray", default)]
    vcard_array: Option<serde_json::Value>,
}

fn parse_answer(resp: &RdapDomainResponse) -> Option<RegistrationAnswer> {
    let expiration_raw = resp
        .events
        .iter()
        .find(|e| e.event_action.eq_ignore_ascii_case("expiration"))?;
    let expiration = DateTime::parse_from_rfc3339(&expiration_raw.event_date)
        .ok()?
        .with_timezone(&Utc);
    let registrar = resp
        .entities
        .iter()
        .find(|e| e.roles.iter().any(|r| r.eq_ignore_ascii_case("registrar")))
        .and_then(|e| registrar_name(e.vcard_array.as_ref()?));
    Some(RegistrationAnswer {
        expiration,
        registrar,
    })
}

/// Extracts the registrar's `fn` (full name) entry from the vCard array. The
/// RFC 7095 shape is `["vcard", [["entry-name", {...}, "type", "value"], ...]]`;
/// we hunt for the first entry whose name is `"fn"`.
fn registrar_name(vcard: &serde_json::Value) -> Option<String> {
    let entries = vcard.as_array()?.get(1)?.as_array()?;
    for entry in entries {
        let arr = entry.as_array()?;
        if arr.first().and_then(|v| v.as_str()) == Some("fn") {
            return arr.get(3).and_then(|v| v.as_str()).map(str::to_owned);
        }
    }
    None
}
