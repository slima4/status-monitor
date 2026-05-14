//! Per-org cache for the public status page payload.
//!
//! Each `OrgId` gets its own TTL + single-flight slot in the moka cache, plus
//! its own `ArcSwap<PageData>` last-known-good fallback. Cross-tenant
//! isolation: an org's compute failure cannot serve another org's stale data,
//! and a hot org's recompute can't block another org's request.
//!
//! Failure handling matches the pre-Phase-6 single-org shape: a transient
//! ClickHouse/Postgres failure never surfaces a 5xx to anonymous callers —
//! the page stays up with stale data via the per-org `last_good`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use moka::future::Cache;

use crate::domain::{OrgId, PublicStatusPage};

pub type PageData = PublicStatusPage;

#[derive(Debug, thiserror::Error)]
pub enum PageCacheError {
    /// Compute failed and no last-known-good snapshot exists yet.
    #[error("status page unavailable: no cached data and recompute failed")]
    Unavailable,
}

/// In-process per-org cache for `PageData`. Cheap to clone (everything inside
/// is `Arc`-shaped).
#[derive(Clone)]
pub struct PageCache {
    inner: Cache<OrgId, Arc<PageData>>,
    last_good: Arc<DashMap<OrgId, Arc<ArcSwap<PageData>>>>,
}

impl PageCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            // 1024 active orgs is well above any realistic working set. moka
            // evicts LRU when full; an evicted entry forces the next caller
            // to recompute (acceptable — the request that triggered eviction
            // was for a different org anyway).
            inner: Cache::builder()
                .max_capacity(1024)
                .time_to_live(ttl)
                .build(),
            last_good: Arc::new(DashMap::new()),
        }
    }

    /// Returns the cached `PageData` for `org` if fresh, otherwise invokes
    /// `f` exactly once across concurrent callers for that org (single-
    /// flight) and caches its `Ok` result. On `Err`, returns the org's
    /// last-known-good snapshot if one exists; otherwise returns
    /// [`PageCacheError::Unavailable`]. A failure for org A never affects
    /// org B's cached or stale data.
    pub async fn get_or_compute<F, Fut, E>(
        &self,
        org: OrgId,
        f: F,
    ) -> Result<Arc<PageData>, PageCacheError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<PageData, E>>,
        E: std::fmt::Display + std::fmt::Debug,
    {
        // Clone the per-org last_good slot up front so the moka closure can
        // capture it. DashMap entries are inserted lazily on first success.
        let last_good = self.last_good.clone();
        let res = self
            .inner
            .try_get_with(org, async move {
                match f().await {
                    Ok(page) => {
                        let arc = Arc::new(page);
                        last_good
                            .entry(org)
                            .or_insert_with(|| Arc::new(ArcSwap::from(arc.clone())))
                            .store(arc.clone());
                        Ok::<_, String>(arc)
                    }
                    // {:#} prints the anyhow chain via each link's Display.
                    // Some upstream errors (clickhouse-rs) have terse Display
                    // impls, so append Debug so we never log an empty cause.
                    Err(e) => Err(format!("{e:#} | dbg={e:?}")),
                }
            })
            .await;
        match res {
            Ok(page) => Ok(page),
            Err(e) => match self.last_good.get(&org).map(|s| s.load_full()) {
                Some(stale) => {
                    tracing::warn!(%org, error = %e, "public_status compute failed; serving stale");
                    Ok(stale)
                }
                None => {
                    tracing::error!(
                        %org,
                        error = %e,
                        "public_status compute failed and no last-good snapshot; returning Unavailable"
                    );
                    Err(PageCacheError::Unavailable)
                }
            },
        }
    }

    /// Snapshot of the last successful compute for `org`, if any. Useful for
    /// tests and for surfacing "data is N seconds old" banners.
    pub fn last_good(&self, org: OrgId) -> Option<Arc<PageData>> {
        self.last_good.get(&org).map(|s| s.load_full())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::domain::{OverallState, OverallStatus};

    fn org() -> OrgId {
        OrgId(Uuid::new_v4())
    }

    fn make_page(site: &str) -> PageData {
        PageData {
            overall: OverallStatus {
                state: OverallState::Operational,
                label: "All Systems Operational".into(),
            },
            generated_at: Utc::now(),
            site_name: site.into(),
            groups: Vec::new(),
            active_incidents: Vec::new(),
            recent_incidents: Vec::new(),
            active_maintenance: Vec::new(),
            upcoming_maintenance: Vec::new(),
        }
    }

    #[tokio::test]
    async fn returns_arc_pagedata_on_first_compute() {
        let cache = PageCache::new(Duration::from_secs(10));
        let o = org();
        let page = cache
            .get_or_compute(o, || async { Ok::<_, std::io::Error>(make_page("ok")) })
            .await
            .expect("first compute ok");
        let snap = cache.last_good(o).expect("snapshot present after success");
        assert!(Arc::ptr_eq(&page, &snap));
        assert_eq!(page.site_name, "ok");
    }

    #[tokio::test]
    async fn second_call_within_ttl_does_not_recompute() {
        let cache = PageCache::new(Duration::from_secs(10));
        let o = org();
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let calls = calls.clone();
            cache
                .get_or_compute(o, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, std::io::Error>(make_page("ok"))
                })
                .await
                .expect("ok");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "compute deduplicated by TTL"
        );
    }

    #[tokio::test]
    async fn single_flight_under_concurrency_same_org() {
        let cache = PageCache::new(Duration::from_secs(10));
        let o = org();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..50 {
            let cache = cache.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_compute(o, || async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, std::io::Error>(make_page("flight"))
                    })
                    .await
                    .expect("ok")
            }));
        }
        let mut last: Option<Arc<PageData>> = None;
        for h in handles {
            let got = h.await.expect("join");
            if let Some(prev) = &last {
                assert!(Arc::ptr_eq(prev, &got), "all callers receive same Arc");
            }
            last = Some(got);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "single-flight collapsed concurrent calls"
        );
    }

    #[tokio::test]
    async fn distinct_orgs_have_independent_caches_and_stale_fallbacks() {
        // Org A succeeds and seeds its last_good. Org B fails on first compute
        // — must NOT serve A's stale data, must return Unavailable.
        let cache = PageCache::new(Duration::from_secs(10));
        let a = org();
        let b = org();
        let _ = cache
            .get_or_compute(a, || async { Ok::<_, std::io::Error>(make_page("a")) })
            .await
            .expect("a ok");
        let err = cache
            .get_or_compute(b, || async {
                Err::<PageData, _>(std::io::Error::other("b down"))
            })
            .await
            .expect_err("b has no stale of its own");
        assert!(matches!(err, PageCacheError::Unavailable));
        // A's last_good is untouched.
        let snap_a = cache.last_good(a).expect("a still cached");
        assert_eq!(snap_a.site_name, "a");
        assert!(cache.last_good(b).is_none(), "b has no snapshot");
    }

    #[tokio::test]
    async fn serves_stale_when_compute_fails_after_initial_success() {
        let cache = PageCache::new(Duration::from_millis(50));
        let o = org();
        let _good = cache
            .get_or_compute(o, || async { Ok::<_, std::io::Error>(make_page("good")) })
            .await
            .expect("prime ok");
        tokio::time::sleep(Duration::from_millis(80)).await;
        let stale = cache
            .get_or_compute(o, || async {
                Err::<PageData, _>(std::io::Error::other("ch down"))
            })
            .await
            .expect("served stale");
        assert_eq!(stale.site_name, "good");
    }

    #[tokio::test]
    async fn unavailable_when_first_compute_fails_with_no_stale() {
        let cache = PageCache::new(Duration::from_secs(10));
        let o = org();
        let err = cache
            .get_or_compute(o, || async {
                Err::<PageData, _>(std::io::Error::other("ch down"))
            })
            .await
            .expect_err("no stale, propagates");
        matches!(err, PageCacheError::Unavailable);
    }
}
