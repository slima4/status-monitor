use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::domain::{CheckSpec, OrgId};

/// Per-(org, host, port) in-flight cap. Tenant-scoped — one customer's burst
/// can't starve another customer's monitor of the same host. Fail-fast
/// bulkhead pattern: contention returns `Throttled` immediately with no
/// wait, so a pool worker permit is never held while queued.
pub struct HostThrottle {
    caps: DashMap<HostKey, Arc<Semaphore>>,
    /// RDAP slots keyed by TLD. Process-wide (not per-tenant) — never burst
    /// the registry. Per-TLD instead of single global so a slow `.com` query
    /// doesn't drag `.org` checks down with it.
    rdap: DashMap<Arc<str>, Arc<Semaphore>>,
    per_host_max: usize,
    rdap_max: usize,
}

pub type HostKey = (OrgId, Arc<str>, u16);

#[must_use = "drop HostPermit only after the check completes — early drop cancels the throttle"]
pub struct HostPermit {
    _inner: OwnedSemaphorePermit,
}

#[derive(Debug)]
pub struct Throttled;

impl HostThrottle {
    pub fn new(per_host_max: usize, rdap_max: usize) -> Self {
        Self {
            caps: DashMap::new(),
            rdap: DashMap::new(),
            per_host_max: clamp_permits(per_host_max),
            rdap_max: clamp_permits(rdap_max),
        }
    }

    /// `None` for DNS (shared resolver pool) and DomainExpiry (uses
    /// `acquire_rdap` instead). Called once at registry refresh — the
    /// returned `Arc<str>` is cached on `ScheduledTarget` and the dispatch
    /// hot path only ever clones the Arc.
    pub fn key_for(org: OrgId, spec: &CheckSpec) -> Option<HostKey> {
        match spec {
            CheckSpec::Http(http) => {
                let host: Arc<str> = Arc::from(normalize_host(http.url.host_str()?));
                let port = http.url.port_or_known_default()?;
                Some((org, host, port))
            }
            CheckSpec::Tcp(tcp) => Some((org, Arc::from(normalize_host(&tcp.host)), tcp.port)),
            CheckSpec::TlsCert(cert) => {
                Some((org, Arc::from(normalize_host(&cert.host)), cert.port))
            }
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
        Arc::new(Self::new(Semaphore::MAX_PERMITS, Semaphore::MAX_PERMITS))
    }

    pub fn acquire(&self, key: &HostKey) -> Result<HostPermit, Throttled> {
        try_acquire(self.semaphore_for(key))
    }

    /// Per-TLD RDAP cap. Caller passes the pre-computed TLD (cached on
    /// `ScheduledTarget.rdap_tld`).
    pub fn acquire_rdap(&self, tld: &Arc<str>) -> Result<HostPermit, Throttled> {
        try_acquire(self.rdap_semaphore_for(tld))
    }

    fn semaphore_for(&self, key: &HostKey) -> Arc<Semaphore> {
        if let Some(s) = self.caps.get(key) {
            return s.clone();
        }
        self.caps
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_host_max)))
            .clone()
    }

    fn rdap_semaphore_for(&self, tld: &Arc<str>) -> Arc<Semaphore> {
        if let Some(s) = self.rdap.get(tld) {
            return s.clone();
        }
        self.rdap
            .entry(tld.clone())
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
        let rdap_keys: Vec<Arc<str>> = self
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

fn try_acquire(sem: Arc<Semaphore>) -> Result<HostPermit, Throttled> {
    sem.try_acquire_owned()
        .map(|p| HostPermit { _inner: p })
        .map_err(|_| Throttled)
}

fn clamp_permits(n: usize) -> usize {
    n.clamp(1, Semaphore::MAX_PERMITS)
}

/// Canonical host key: ASCII-lowercased + trailing dot stripped. The `url`
/// crate already returns punycode for IDN, so the public-status FQDN
/// trailing-dot Host bypass is what we still have to fix at this layer.
pub fn canonical_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn normalize_host(host: &str) -> String {
    canonical_host(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn org() -> OrgId {
        OrgId(Uuid::new_v4())
    }

    fn key(org: OrgId, host: &str) -> HostKey {
        (org, Arc::from(host), 443)
    }

    fn tld(s: &str) -> Arc<str> {
        Arc::from(s)
    }

    #[test]
    fn cap_two_allows_two_concurrent_then_rejects() {
        let throttle = HostThrottle::new(2, 1);
        let org = org();
        let k = key(org, "example.com");
        let _a = throttle.acquire(&k).expect("first");
        let _b = throttle.acquire(&k).expect("second");
        let third = throttle.acquire(&k);
        assert!(third.is_err(), "third acquire above cap must fail-fast");
    }

    #[test]
    fn permit_release_re_opens_slot() {
        let throttle = HostThrottle::new(1, 1);
        let org = org();
        let k = key(org, "example.com");
        {
            let _hold = throttle.acquire(&k).expect("hold");
            assert!(throttle.acquire(&k).is_err());
        }
        assert!(
            throttle.acquire(&k).is_ok(),
            "permit drop must replenish the slot"
        );
    }

    #[test]
    fn tenant_isolation_a_does_not_starve_b() {
        let throttle = HostThrottle::new(1, 1);
        let a = org();
        let b = org();
        let _hold_a = throttle.acquire(&key(a, "example.com")).expect("a");
        let b_permit = throttle.acquire(&key(b, "example.com"));
        assert!(b_permit.is_ok(), "tenant B must not be starved by tenant A");
    }

    #[test]
    fn rdap_cap_one_per_tld_independent() {
        let throttle = HostThrottle::new(2, 1);
        let _hold = throttle.acquire_rdap(&tld("com")).expect("first");
        assert!(
            throttle.acquire_rdap(&tld("com")).is_err(),
            ".com cap=1 must reject second concurrent"
        );
        assert!(
            throttle.acquire_rdap(&tld("org")).is_ok(),
            ".org must not be blocked by a stuck .com"
        );
    }

    #[test]
    fn sweep_drops_unused_entries() {
        let throttle = HostThrottle::new(2, 1);
        let org = org();
        {
            let _p = throttle.acquire(&key(org, "h1")).unwrap();
            let _q = throttle.acquire(&key(org, "h2")).unwrap();
        }
        assert_eq!(throttle.map_len(), 2);
        assert_eq!(throttle.sweep(), 2);
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
