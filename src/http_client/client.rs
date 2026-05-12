use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use metrics::{Histogram, histogram};

use crate::config::{CheckerConfig, DnsConfig, HttpClientConfig};
use crate::error::Result;
use crate::http_client::dns::HickoryDnsResolver;
use crate::observability::metrics::names;

#[derive(Clone)]
pub struct HttpClients {
    verifying: reqwest::Client,
    insecure: reqwest::Client,
    pub(crate) ttfb_ms: Histogram,
}

impl HttpClients {
    pub fn pick(&self, verify_tls: bool) -> &reqwest::Client {
        if verify_tls {
            &self.verifying
        } else {
            &self.insecure
        }
    }
}

pub fn build_clients(
    http_cfg: &HttpClientConfig,
    checker_cfg: &CheckerConfig,
    dns_cfg: &DnsConfig,
) -> Result<HttpClients> {
    let resolver = Arc::new(HickoryDnsResolver::new(dns_cfg)?);
    let verifying = build_one(http_cfg, checker_cfg, resolver.clone(), true)?;
    let insecure = build_one(http_cfg, checker_cfg, resolver, false)?;
    Ok(HttpClients {
        verifying,
        insecure,
        ttfb_ms: histogram!(names::CHECK_TTFB_MS),
    })
}

fn build_one(
    http_cfg: &HttpClientConfig,
    checker_cfg: &CheckerConfig,
    resolver: Arc<HickoryDnsResolver>,
    verify_tls: bool,
) -> Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(http_cfg.pool_max_idle_per_host)
        .pool_idle_timeout(Duration::from_secs(http_cfg.pool_idle_timeout_secs))
        .tcp_keepalive(Duration::from_secs(http_cfg.tcp_keepalive_secs))
        .tcp_nodelay(true)
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(Duration::from_secs(http_cfg.http2_keep_alive_interval_secs))
        .http2_keep_alive_timeout(Duration::from_secs(http_cfg.http2_keep_alive_timeout_secs))
        .http2_keep_alive_while_idle(http_cfg.http2_keep_alive_while_idle)
        .use_rustls_tls()
        .https_only(false)
        .danger_accept_invalid_certs(!verify_tls)
        .dns_resolver(resolver)
        .connect_timeout(Duration::from_millis(checker_cfg.connect_timeout_ms))
        .timeout(Duration::from_millis(checker_cfg.default_timeout_ms))
        .gzip(true)
        .brotli(true)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(&http_cfg.user_agent)
        .build()
        .context("failed to build HTTP client")?;
    Ok(client)
}
