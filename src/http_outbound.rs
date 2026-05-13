use anyhow::Context;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Serialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{AppError, Result};

/// Shared HTTPS client used by outbound non-check traffic (notification
/// transports, RDAP lookups). Distinct from `HttpClients` — that one wraps a
/// custom connector that records DNS/connect/TLS/TTFB histograms and applies
/// the SSRF guard, both of which would poison metrics or block valid
/// destinations when called from non-check paths.
pub type OutboundHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

// Cap outbound response bodies at 1 MiB: IANA RDAP bootstrap is ~50 KiB, RDAP
// per-domain responses are <10 KiB, Slack/webhook ack bodies are tiny. Anything
// larger is a misconfigured or hostile endpoint, and we want a streamed limit
// so we never allocate a multi-MB buffer just to reject it.
const MAX_RESPONSE_BYTES: usize = 1 << 20;

pub fn build_outbound_client() -> OutboundHttpClient {
    crate::http_client::client::install_default_crypto_provider();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("loading native cert roots for outbound client")
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

pub async fn post_json<T: Serialize>(
    client: &OutboundHttpClient,
    url: &Url,
    body: &T,
) -> Result<()> {
    let payload = serde_json::to_vec(body).context("serializing request payload")?;
    let req = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(payload)))
        .context("building request")?;
    let resp = client.request(req).await.context("sending request")?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = resp
            .into_body()
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

pub async fn get_json<T: DeserializeOwned>(client: &OutboundHttpClient, url: &Url) -> Result<T> {
    let req = Request::get(url.as_str())
        .header(ACCEPT, "application/json")
        .body(Full::new(Bytes::new()))
        .context("building request")?;
    let resp = client.request(req).await.context("sending request")?;
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
