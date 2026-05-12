use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::config::{CheckerConfig, DnsConfig, HttpClientConfig};
use crate::error::Result;
use crate::http_client::dns::HickoryDnsResolver;

pub fn build_client(
    http_cfg: &HttpClientConfig,
    checker_cfg: &CheckerConfig,
    dns_cfg: &DnsConfig,
) -> Result<reqwest::Client> {
    let resolver = Arc::new(HickoryDnsResolver::new(dns_cfg)?);

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
