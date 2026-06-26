pub mod circuit_breaker;
pub mod dns;
pub mod domain_expiry;
pub mod host_throttle;
pub mod http_check;
pub mod interpolate;
pub mod pool;
pub mod rdap;
pub mod rdap_singleflight;
pub mod tcp_check;
pub mod tls_cert;

pub use http_check::execute_http_check;
pub(crate) use http_check::{HttpProbe, execute_http_check_probe};
pub use pool::{CheckTask, ResultFanout, WorkerPool, host_for_spec};

use std::hash::Hash;
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::domain::{CheckResult, CheckSpec};
use crate::http_client::HttpClients;
use crate::worker::domain_expiry::DomainExpiryRuntime;

/// Off-hot-path eviction over a `DashMap<K, Arc<T>>`. Drops entries whose
/// only strong reference is the map's own and that the caller-supplied
/// `idle` predicate accepts. Atomic per shard via `DashMap::retain` — never
/// drops an entry another task just cloned out of the map.
///
/// Used by `HostThrottle::sweep`, `WorkerPool::sweep_breakers`, and
/// `RdapSingleflight::sweep`. Three sites converged on this shape so an
/// invariant fix (e.g. tightening the strong-count check) only has to be
/// made in one place.
pub(crate) fn sweep_idle<K, T, F>(map: &DashMap<K, Arc<T>>, idle: F) -> usize
where
    K: Eq + Hash,
    F: Fn(&T) -> bool,
{
    let mut removed = 0usize;
    map.retain(|_, slot| {
        if Arc::strong_count(slot) != 1 {
            return true;
        }
        if idle(slot.as_ref()) {
            removed += 1;
            false
        } else {
            true
        }
    });
    removed
}

/// Per-dispatch dependencies handed to `execute`. Bundles everything an
/// executor sub-handler might need so adding a new dep (e.g. another
/// per-resource bulkhead, another store) doesn't ripple through every call
/// site's argument list.
pub struct WorkerDeps<'a> {
    pub http: &'a HttpClients,
    pub domain_expiry: &'a DomainExpiryRuntime,
}

/// Maps a `days_remaining` value to the canonical Up/Degraded/Down ladder used
/// by the TLS-cert and domain-expiry checks. Negative `days_remaining` always
/// falls below `critical_days` (which is `u32 >= 0`), so expiration is covered
/// by the same branch as the critical threshold.
pub(crate) fn classify_days(
    days_remaining: i64,
    warn_days: u32,
    critical_days: u32,
) -> crate::domain::CheckStatus {
    use crate::domain::CheckStatus;
    if days_remaining < i64::from(critical_days) {
        CheckStatus::Down
    } else if days_remaining < i64::from(warn_days) {
        CheckStatus::Degraded
    } else {
        CheckStatus::Up
    }
}

/// Resolves `host`, filters the addresses through the shared SSRF guard, and
/// tries to open a TCP connection to `(ip, port)`. Used by TCP and TLS-cert
/// checks — both want exactly the same resolve-and-connect dance.
pub(crate) async fn connect_via_guard(
    host: &str,
    port: u16,
    clients: &HttpClients,
) -> anyhow::Result<TcpStream> {
    let guard = clients.ssrf_guard();
    let mut last_err: Option<std::io::Error> = None;
    let mut tried = false;
    for ip in clients.resolver().resolve_addrs(host).await? {
        if !guard.allow(ip) {
            continue;
        }
        tried = true;
        match TcpStream::connect(SocketAddr::new(ip, port)).await {
            Ok(s) => return Ok(s),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e.into());
    }
    if tried {
        Err(anyhow::anyhow!("no addresses for {host}"))
    } else {
        Err(anyhow::anyhow!("no allowed addresses for {host}"))
    }
}

pub async fn execute(
    target_id: Uuid,
    org_id: Uuid,
    spec: &CheckSpec,
    deps: &WorkerDeps<'_>,
) -> CheckResult {
    match spec {
        CheckSpec::Http(http) => execute_http_check(target_id, org_id, http, deps.http).await,
        CheckSpec::Tcp(tcp) => {
            tcp_check::execute_tcp_check(target_id, org_id, tcp, deps.http).await
        }
        CheckSpec::TlsCert(cert) => {
            tls_cert::execute_tls_cert_check(target_id, org_id, cert, deps.http).await
        }
        CheckSpec::DomainExpiry(domain) => {
            domain_expiry::execute_domain_expiry_check(
                target_id,
                org_id,
                domain,
                deps.domain_expiry,
            )
            .await
        }
        CheckSpec::Dns(d) => dns::execute_dns_check(target_id, org_id, d, deps.http).await,
    }
}

/// Verbose variant of `execute` for the test-check UI: HTTP returns a
/// populated probe; other variants return `None`.
pub(crate) async fn execute_with_probe(
    target_id: Uuid,
    org_id: Uuid,
    spec: &CheckSpec,
    deps: &WorkerDeps<'_>,
) -> (CheckResult, Option<HttpProbe>) {
    if let CheckSpec::Http(http) = spec {
        let (r, p) = execute_http_check_probe(target_id, org_id, http, deps.http).await;
        (r, Some(p))
    } else {
        (execute(target_id, org_id, spec, deps).await, None)
    }
}
