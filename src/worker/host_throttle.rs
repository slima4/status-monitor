use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

use crate::domain::{CheckSpec, OrgId};

/// Per-(org, host, port) in-flight cap. Tenant-scoped — one customer's burst
/// can't starve another customer's monitor of the same host.
pub struct HostThrottle {
    caps: DashMap<HostKey, Arc<Semaphore>>,
    /// RDAP slots keyed by TLD. Process-wide (not per-tenant) — never burst
    /// the registry. Per-TLD instead of single global so a slow `.com` query
    /// doesn't drag `.org` checks down with it.
    rdap: DashMap<String, Arc<Semaphore>>,
    per_host_max: usize,
    rdap_max: usize,
    acquire_timeout: Duration,
}

pub type HostKey = (OrgId, String, u16);

#[must_use = "drop HostPermit only after the check completes — early drop cancels the throttle"]
pub struct HostPermit {
    _inner: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub struct Throttled;

impl HostThrottle {
    pub fn new(per_host_max: usize, rdap_max: usize, acquire_timeout: Duration) -> Self {
        Self {
            caps: DashMap::new(),
            rdap: DashMap::new(),
            per_host_max: clamp_permits(per_host_max),
            rdap_max: clamp_permits(rdap_max),
            acquire_timeout,
        }
    }

    /// `None` for DNS (shared resolver pool) and DomainExpiry (uses
    /// `acquire_rdap` instead).
    pub fn key_for(org: OrgId, spec: &CheckSpec) -> Option<HostKey> {
        match spec {
            CheckSpec::Http(http) => {
                let host = normalize_host(http.url.host_str()?);
                let port = http.url.port_or_known_default()?;
                Some((org, host, port))
            }
            CheckSpec::Tcp(tcp) => Some((org, normalize_host(&tcp.host), tcp.port)),
            CheckSpec::TlsCert(cert) => Some((org, normalize_host(&cert.host), cert.port)),
            CheckSpec::Dns(_) | CheckSpec::DomainExpiry(_) => None,
        }
    }

    /// Lowercase TLD (last label) of a domain. `None` when the input has no
    /// label. Trailing dot tolerated.
    pub fn rdap_tld(domain: &str) -> Option<String> {
        let trimmed = domain.trim_end_matches('.');
        trimmed
            .rsplit('.')
            .next()
            .filter(|t| !t.is_empty())
            .map(str::to_ascii_lowercase)
    }

    /// Wide-open cap for tests + benches.
    pub fn permissive() -> Arc<Self> {
        Arc::new(Self::new(
            Semaphore::MAX_PERMITS,
            Semaphore::MAX_PERMITS,
            Duration::from_secs(60),
        ))
    }

    pub async fn acquire(&self, key: HostKey) -> Result<HostPermit, Throttled> {
        let sem = self.semaphore_for(key);
        acquire_with_timeout(sem, self.acquire_timeout).await
    }

    /// Per-TLD RDAP cap. Caller derives the TLD from the domain via
    /// `rdap_tld`.
    pub async fn acquire_rdap(&self, tld: &str) -> Result<HostPermit, Throttled> {
        let sem = self.rdap_semaphore_for(tld);
        acquire_with_timeout(sem, self.acquire_timeout).await
    }

    fn semaphore_for(&self, key: HostKey) -> Arc<Semaphore> {
        if let Some(s) = self.caps.get(&key) {
            return s.clone();
        }
        self.caps
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_host_max)))
            .clone()
    }

    fn rdap_semaphore_for(&self, tld: &str) -> Arc<Semaphore> {
        if let Some(s) = self.rdap.get(tld) {
            return s.clone();
        }
        self.rdap
            .entry(tld.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(self.rdap_max)))
            .clone()
    }

    /// Off-hot-path eviction. Atomic via `remove_if` — never drops a
    /// semaphore another task just cloned out.
    pub fn sweep(&self) -> usize {
        let mut removed = 0usize;
        let host_keys: Vec<HostKey> = self
            .caps
            .iter()
            .filter(|e| Arc::strong_count(e.value()) == 1)
            .map(|e| e.key().clone())
            .collect();
        let per_host_max = self.per_host_max;
        for k in host_keys {
            if self
                .caps
                .remove_if(&k, |_, v| {
                    Arc::strong_count(v) == 1 && v.available_permits() == per_host_max
                })
                .is_some()
            {
                removed += 1;
            }
        }
        let rdap_keys: Vec<String> = self
            .rdap
            .iter()
            .filter(|e| Arc::strong_count(e.value()) == 1)
            .map(|e| e.key().clone())
            .collect();
        let rdap_max = self.rdap_max;
        for k in rdap_keys {
            if self
                .rdap
                .remove_if(&k, |_, v| {
                    Arc::strong_count(v) == 1 && v.available_permits() == rdap_max
                })
                .is_some()
            {
                removed += 1;
            }
        }
        removed
    }

    #[cfg(test)]
    pub fn map_len(&self) -> usize {
        self.caps.len()
    }

    #[cfg(test)]
    pub fn rdap_map_len(&self) -> usize {
        self.rdap.len()
    }
}

async fn acquire_with_timeout(sem: Arc<Semaphore>, dur: Duration) -> Result<HostPermit, Throttled> {
    if let Ok(p) = sem.clone().try_acquire_owned() {
        return Ok(HostPermit { _inner: p });
    }
    match timeout(dur, sem.acquire_owned()).await {
        Ok(Ok(p)) => Ok(HostPermit { _inner: p }),
        _ => Err(Throttled),
    }
}

fn clamp_permits(n: usize) -> usize {
    n.clamp(1, Semaphore::MAX_PERMITS)
}

/// Canonical host key: ASCII-lowercased + trailing dot stripped. The `url`
/// crate already returns punycode for IDN, so the public-status FQDN
/// trailing-dot Host bypass is what we still have to fix at this layer.
fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    fn org() -> OrgId {
        OrgId(Uuid::new_v4())
    }

    fn key(org: OrgId, host: &str) -> HostKey {
        (org, host.to_owned(), 443)
    }

    #[tokio::test]
    async fn twenty_concurrent_cap_two_all_complete() {
        let throttle = Arc::new(HostThrottle::new(2, 1, Duration::from_secs(5)));
        let org = org();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..20 {
            let t = throttle.clone();
            let k = key(org, "example.com");
            let in_flight = in_flight.clone();
            let peak = peak.clone();
            tasks.push(tokio::spawn(async move {
                let permit = t.acquire(k).await.expect("acquire");
                let n = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                drop(permit);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn throttled_when_timeout_expires() {
        let throttle = Arc::new(HostThrottle::new(1, 1, Duration::from_millis(10)));
        let org = org();
        let k = key(org, "example.com");

        let hold = throttle.acquire(k.clone()).await.expect("first");
        let denied = throttle.acquire(k).await;
        assert!(denied.is_err(), "second acquire must time out");
        drop(hold);
    }

    #[tokio::test]
    async fn tenant_isolation_a_does_not_starve_b() {
        let throttle = Arc::new(HostThrottle::new(1, 1, Duration::from_millis(10)));
        let a = org();
        let b = org();
        let _hold_a = throttle.acquire(key(a, "example.com")).await.expect("a");

        let b_permit = throttle.acquire(key(b, "example.com")).await;
        assert!(b_permit.is_ok(), "tenant B must not be starved by tenant A");
    }

    #[tokio::test]
    async fn rdap_global_cap_one_per_tld() {
        let throttle = Arc::new(HostThrottle::new(2, 1, Duration::from_millis(10)));
        let _hold = throttle.acquire_rdap("com").await.expect("first");
        let denied_same_tld = throttle.acquire_rdap("com").await;
        assert!(
            denied_same_tld.is_err(),
            "second .com acquire must time out under cap=1"
        );
        // Different TLD is independent.
        let other = throttle.acquire_rdap("org").await;
        assert!(other.is_ok(), ".org must not be blocked by a stuck .com");
    }

    #[tokio::test]
    async fn sweep_drops_unused_entries() {
        let throttle = HostThrottle::new(2, 1, Duration::from_millis(10));
        let org = org();
        {
            let _p = throttle.acquire(key(org, "h1")).await.unwrap();
            let _q = throttle.acquire(key(org, "h2")).await.unwrap();
        }
        assert_eq!(throttle.map_len(), 2);
        let evicted = throttle.sweep();
        assert_eq!(evicted, 2);
        assert_eq!(throttle.map_len(), 0);
    }

    #[test]
    fn host_normalization_collapses_trailing_dot_and_case() {
        assert_eq!(normalize_host("Example.COM."), "example.com");
        assert_eq!(normalize_host("example.com"), "example.com");
    }

    #[test]
    fn rdap_tld_extracts_lowercase_last_label() {
        assert_eq!(
            HostThrottle::rdap_tld("example.com").as_deref(),
            Some("com")
        );
        assert_eq!(
            HostThrottle::rdap_tld("EXAMPLE.COM.").as_deref(),
            Some("com")
        );
        assert_eq!(HostThrottle::rdap_tld("uk").as_deref(), Some("uk"));
        assert_eq!(HostThrottle::rdap_tld(""), None);
    }

    #[test]
    fn permissive_does_not_panic_on_construction() {
        let _ = HostThrottle::permissive();
    }
}
