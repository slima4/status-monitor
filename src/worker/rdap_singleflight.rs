//! Singleflight + short-TTL cache for RDAP probes, keyed by canonical
//! domain. RDAP responses are public registry data — coalescing across
//! tenants is safe and drops outbound traffic by ~100× when many customers
//! monitor the same popular domains.
//!
//! Semantics:
//!  - First caller for `domain` acquires the per-slot mutex and runs the
//!    fetcher closure.
//!  - Concurrent callers for the same `domain` wait on the same mutex; when
//!    the first caller fills the slot, the rest read a `Ready` value
//!    without invoking the closure.
//!  - A successful response is reused for `cache_ttl` (default 60s). The
//!    window is short by design: durable last-good lives in Postgres
//!    `domain_expiry_state`; the in-process cache only absorbs
//!    scheduler-jitter waves and concurrent target dispatch.
//!  - Errors are *not* cached. The next caller after a failure tries again,
//!    so a transient blip doesn't poison the slot.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::Mutex;

use crate::error::Result;
use crate::worker::rdap::RdapAnswer;

/// Default in-process cache window for a successful RDAP answer.
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60);

pub struct RdapSingleflight {
    slots: DashMap<Arc<str>, Arc<Slot>>,
    cache_ttl: Duration,
}

struct Slot {
    state: Mutex<SlotState>,
}

enum SlotState {
    Empty,
    Ready {
        value: Arc<RdapAnswer>,
        fetched_at: Instant,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    Hit,
    Miss,
}

impl RdapSingleflight {
    pub fn new(cache_ttl: Duration) -> Self {
        Self {
            slots: DashMap::new(),
            cache_ttl,
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(DEFAULT_CACHE_TTL)
    }

    /// Calls `fetcher` only when the slot for `domain` is missing or stale;
    /// concurrent callers for the same key collapse to one invocation.
    pub async fn lookup<F, Fut>(
        &self,
        domain: Arc<str>,
        fetcher: F,
    ) -> Result<(Arc<RdapAnswer>, FetchOutcome)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<RdapAnswer>>,
    {
        let slot = self
            .slots
            .entry(domain.clone())
            .or_insert_with(|| {
                Arc::new(Slot {
                    state: Mutex::new(SlotState::Empty),
                })
            })
            .clone();

        // MUST stay `tokio::sync::Mutex`. The guard is held across
        // `fetcher().await` to serialise concurrent callers onto one
        // outbound RDAP request — a sync mutex (parking_lot) would block the
        // worker thread for the full RDAP round-trip and saturate the
        // runtime under contention.
        let mut guard = slot.state.lock().await;
        if let SlotState::Ready { value, fetched_at } = &*guard
            && fetched_at.elapsed() < self.cache_ttl
        {
            return Ok((value.clone(), FetchOutcome::Hit));
        }

        let answer = fetcher().await?;
        let value = Arc::new(answer);
        *guard = SlotState::Ready {
            value: value.clone(),
            fetched_at: Instant::now(),
        };
        Ok((value, FetchOutcome::Miss))
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn answer() -> RdapAnswer {
        RdapAnswer {
            expiration: Utc::now() + chrono::Duration::days(90),
            registrar: Some("Test Registrar".into()),
        }
    }

    #[tokio::test]
    async fn concurrent_callers_collapse_to_one_fetch() {
        let sf = Arc::new(RdapSingleflight::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicUsize::new(0));
        let domain: Arc<str> = Arc::from("example.com");

        let mut handles = Vec::new();
        for _ in 0..10 {
            let sf = sf.clone();
            let calls = calls.clone();
            let domain = domain.clone();
            handles.push(tokio::spawn(async move {
                sf.lookup(domain, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    Ok(answer())
                })
                .await
                .unwrap()
            }));
        }
        let mut hits = 0;
        let mut misses = 0;
        for h in handles {
            let (_, outcome) = h.await.unwrap();
            match outcome {
                FetchOutcome::Hit => hits += 1,
                FetchOutcome::Miss => misses += 1,
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(misses, 1);
        assert_eq!(hits, 9);
    }

    #[tokio::test]
    async fn ttl_expiry_triggers_refetch() {
        let sf = RdapSingleflight::new(Duration::from_millis(20));
        let calls = Arc::new(AtomicUsize::new(0));
        let domain: Arc<str> = Arc::from("example.com");

        let _ = sf
            .lookup(domain.clone(), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(answer())
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let (_, outcome) = sf
            .lookup(domain, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(answer())
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(outcome, FetchOutcome::Miss);
    }

    #[tokio::test]
    async fn failure_is_not_cached() {
        let sf = RdapSingleflight::new(Duration::from_secs(60));
        let domain: Arc<str> = Arc::from("example.com");
        let err = sf
            .lookup(domain.clone(), || async {
                Err::<RdapAnswer, _>(crate::error::AppError::Other(anyhow::anyhow!("boom")))
            })
            .await;
        assert!(err.is_err());
        // Second call must invoke the fetcher again because the previous
        // failure is not cached.
        let calls = Arc::new(AtomicUsize::new(0));
        let (_, outcome) = sf
            .lookup(domain, || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(answer())
                }
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome, FetchOutcome::Miss);
    }

    /// Singleflight key normalisation lives in `domain_expiry::fresh_probe`
    /// — by the time a domain hits `lookup` it is the canonical
    /// (lowercased + IDN-encoded + trailing-dot stripped) form. This test
    /// asserts the cache contract on the canonical key: identical keys
    /// share a slot regardless of how many `Arc<str>` clones the caller
    /// makes.
    #[tokio::test]
    async fn canonical_keys_share_one_slot() {
        let sf = Arc::new(RdapSingleflight::new(Duration::from_secs(60)));
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let _ = sf
                .lookup(Arc::<str>::from("xn--bhn-qla.de"), || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(answer())
                    }
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sf.len(), 1);
    }

    #[tokio::test]
    async fn distinct_domains_each_fetch_once() {
        let sf = RdapSingleflight::new(Duration::from_secs(60));
        let calls = Arc::new(AtomicUsize::new(0));
        for name in ["a.com", "b.com", "c.com"] {
            let _ = sf
                .lookup(Arc::from(name), || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(answer())
                    }
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(sf.len(), 3);
    }
}
