use anyhow::Context;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::CONTENT_TYPE;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Serialize;
use url::Url;

use crate::error::{AppError, Result};

pub type NotifyHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

pub fn build_notify_client() -> NotifyHttpClient {
    crate::http_client::client::install_default_crypto_provider();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("loading native cert roots for notifier")
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

pub async fn post_json<T: Serialize>(client: &NotifyHttpClient, url: &Url, body: &T) -> Result<()> {
    let payload = serde_json::to_vec(body).context("serializing notifier payload")?;
    let req = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(payload)))
        .context("building notifier request")?;
    let resp = client
        .request(req)
        .await
        .context("sending notifier request")?;
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
            "notifier endpoint returned {status}: {body}"
        )));
    }
    Ok(())
}
