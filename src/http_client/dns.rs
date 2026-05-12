use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::lookup_ip::LookupIp;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::{Resolver, TokioResolver};
use metrics::{Histogram, histogram};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::config::DnsConfig;
use crate::error::Result;
use crate::observability::metrics::names;

pub struct HickoryDnsResolver {
    inner: Arc<TokioResolver>,
    dns_ms: Histogram,
}

impl HickoryDnsResolver {
    pub fn new(cfg: &DnsConfig) -> Result<Self> {
        let mut opts = ResolverOpts::default();
        opts.cache_size = cfg.cache_size as u64;
        opts.positive_max_ttl = Some(Duration::from_secs(cfg.positive_ttl_secs));
        opts.negative_max_ttl = Some(Duration::from_secs(cfg.negative_ttl_secs));
        opts.attempts = 2;
        opts.timeout = Duration::from_secs(3);
        opts.try_tcp_on_error = true;

        let name_servers: Vec<NameServerConfig> = cfg
            .servers
            .iter()
            .map(|s| {
                let trimmed = s.split(':').next().unwrap_or(s);
                trimmed
                    .parse::<IpAddr>()
                    .map(NameServerConfig::udp)
                    .with_context(|| format!("invalid dns server ip: {s}"))
            })
            .collect::<anyhow::Result<_>>()?;

        let resolver_config = ResolverConfig::from_parts(None, vec![], name_servers);

        let resolver =
            Resolver::builder_with_config(resolver_config, TokioRuntimeProvider::default())
                .with_options(opts)
                .build()
                .context("failed to build hickory resolver")?;

        Ok(Self {
            inner: Arc::new(resolver),
            dns_ms: histogram!(names::CHECK_DNS_MS),
        })
    }
}

impl Resolve for HickoryDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.inner.clone();
        let dns_ms = self.dns_ms.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let start = Instant::now();
            let lookup: LookupIp = resolver.lookup_ip(host).await?;
            dns_ms.record(start.elapsed().as_millis() as f64);
            let ips: Vec<SocketAddr> = lookup.iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            let addrs: Addrs = Box::new(ips.into_iter());
            Ok(addrs)
        })
    }
}
