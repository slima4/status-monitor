use std::time::Duration;

use anyhow::Context;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{AppError, Result};
use crate::security::{SsrfGuard, SsrfHttpConnector};

/// Shared HTTPS client used by outbound non-check traffic (notification
/// transports, RDAP lookups). Uses [`SsrfHttpConnector`] so a webhook URL
/// pointing at a private IP (loopback, RFC1918, link-local, cloud metadata,
/// 6to4 / NAT64-embedded private v4) is dropped at DNS-filter time before
/// any TCP open — DNS-rebinding safe by construction.
///
/// Distinct from `HttpClients` (the check-path client): that one wraps a
/// custom connector that records DNS/connect/TLS/TTFB histograms and uses
/// the shared Hickory resolver, both of which would poison check metrics
/// when called from non-check paths.
pub type OutboundHttpClient = Client<HttpsConnector<SsrfHttpConnector>, Full<Bytes>>;

// Cap outbound response bodies at 1 MiB: IANA RDAP bootstrap is ~50 KiB, RDAP
// per-domain responses are <10 KiB, Slack/webhook ack bodies are tiny. Anything
// larger is a misconfigured or hostile endpoint, and we want a streamed limit
// so we never allocate a multi-MB buffer just to reject it.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

/// Total budget for a single outbound request (connect + headers + body). The
/// `SsrfHttpConnector` already caps the connect phase at 10 s, but `hyper`'s
/// client has no built-in read timeout — a hostile endpoint that ACKs the
/// connect then drips one byte per second would pin a notifier task forever.
/// 30 s is generous enough for slow Slack/Telegram acks under load, tight
/// enough that a stuck request frees the worker before the next notification
/// cycle.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub fn build_outbound_client(guard: SsrfGuard) -> OutboundHttpClient {
    crate::http_client::client::install_default_crypto_provider();
    // Native trust store first; fall back to the bundled webpki roots when
    // it can't be read (an empty/broken store, or a macOS keychain I/O
    // hiccup under load) rather than panicking the process. Mirrors the
    // check-client TLS builder; outbound talks to public CAs either way.
    let https = match hyper_rustls::HttpsConnectorBuilder::new().with_native_roots() {
        Ok(b) => b,
        Err(_) => hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots(),
    }
    .https_or_http()
    .enable_http1()
    .enable_http2()
    .wrap_connector(SsrfHttpConnector::new(guard));
    Client::builder(TokioExecutor::new()).build(https)
}

pub async fn post_json<T: Serialize>(
    client: &OutboundHttpClient,
    url: &Url,
    body: &T,
) -> Result<()> {
    post_json_with_headers(client, url, body, &std::collections::BTreeMap::new()).await
}

/// Like [`post_json`] but adds caller-supplied request headers (generic
/// webhook channels let users attach e.g. an `Authorization` header). A
/// header whose name or value is not valid HTTP is skipped rather than
/// failing the whole delivery — the receiver still gets the payload.
pub async fn post_json_with_headers<T: Serialize>(
    client: &OutboundHttpClient,
    url: &Url,
    body: &T,
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    use hyper::header::{HeaderName, HeaderValue};
    let payload = serde_json::to_vec(body).context("serializing request payload")?;
    let mut builder = Request::post(url.as_str()).header(CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        match (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            (Ok(name), Ok(value)) => builder = builder.header(name, value),
            _ => tracing::warn!(header = %k, "skipping invalid webhook header"),
        }
    }
    let req = builder
        .body(Full::new(Bytes::from(payload)))
        .context("building request")?;
    let resp = with_request_timeout(url, client.request(req)).await?;
    let status = resp.status();
    if !status.is_success() {
        // Even on the error path we bound the diagnostic body: a hostile
        // endpoint that returns 500 with a 10 GiB body would OOM the
        // notifier otherwise. Limited rejects mid-stream, so we don't
        // allocate the full payload just to truncate it.
        let limited = Limited::new(resp.into_body(), MAX_RESPONSE_BYTES);
        let bytes = limited
            .collect()
            .await
            .map(|c| c.to_bytes())
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&bytes);
        return Err(AppError::Other(anyhow::anyhow!(
            "endpoint returned {status}: {body}"
        )));
    }
    Ok(())
}

/// Wrap an outbound request future in [`REQUEST_TIMEOUT`]. Returns the same
/// `Result<Response, AppError>` shape callers already match on; on timeout we
/// surface a context that identifies the URL so the notifier audit log shows
/// *which* webhook stalled.
async fn with_request_timeout<F, T, E>(url: &Url, fut: F) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(REQUEST_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(AppError::Other(anyhow::anyhow!(
            "sending request to {url}: {e}"
        ))),
        Err(_) => Err(AppError::Other(anyhow::anyhow!(
            "request to {url} exceeded {REQUEST_TIMEOUT:?}"
        ))),
    }
}

pub async fn get_json<T: DeserializeOwned>(client: &OutboundHttpClient, url: &Url) -> Result<T> {
    let req = Request::get(url.as_str())
        .header(ACCEPT, "application/json")
        .body(Full::new(Bytes::new()))
        .context("building request")?;
    let resp = with_request_timeout(url, client.request(req)).await?;
    let status = resp.status();
    let limited = Limited::new(resp.into_body(), MAX_RESPONSE_BYTES);
    let bytes = limited.collect().await.map_err(|e| {
        AppError::Other(anyhow::anyhow!(
            "{url} body exceeded {MAX_RESPONSE_BYTES} bytes or read failed: {e}"
        ))
    })?;
    let bytes = bytes.to_bytes();
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        return Err(AppError::Other(anyhow::anyhow!(
            "{url} returned {status}: {body}"
        )));
    }
    serde_json::from_slice(&bytes).map_err(|e| AppError::Other(anyhow::anyhow!("{url}: {e}")))
}
