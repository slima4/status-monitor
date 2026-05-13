//! Single-entry cache for the public status page payload.
//!
//! Backed by [`moka::future::Cache`] for TTL + single-flight (only one task
//! runs the recompute when the entry expires; all other callers await its
//! result). Falls back to a last-known-good copy in [`ArcSwapOption`] so a
//! transient ClickHouse/Postgres failure does not surface a 5xx to anonymous
//! callers — the page stays up with stale data.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use moka::future::Cache;

use crate::domain::PublicStatusPage;

pub type PageData = PublicStatusPage;

#[derive(Debug, thiserror::Error)]
pub enum PageCacheError {
    /// Compute failed and no last-known-good snapshot exists yet.
    #[error("status page unavailable: no cached data and recompute failed")]
    Unavailable,
}

/// In-process cache for `PageData`. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct PageCache {
    inner: Cache<(), Arc<PageData>>,
    last_good: Arc<ArcSwapOption<PageData>>,
}

impl PageCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(1)
                .time_to_live(ttl)
                .build(),
            last_good: Arc::new(ArcSwapOption::empty()),
        }
    }

    /// Returns the cached `PageData` if fresh, otherwise invokes `f` exactly
    /// once across concurrent callers (single-flight) and caches its `Ok`
    /// result. On `Err`, returns the last-known-good snapshot if one exists;
    /// otherwise returns [`PageCacheError::Unavailable`].
    pub async fn get_or_compute<F, Fut, E>(&self, f: F) -> Result<Arc<PageData>, PageCacheError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<PageData, E>>,
        E: std::fmt::Display + std::fmt::Debug,
    {
        // Wrap the user closure so try_get_with sees `Result<Arc<PageData>, String>`.
        // try_get_with does NOT cache the error — successive callers re-run `f`.
        let last_good = self.last_good.clone();
        let res = self
            .inner
            .try_get_with((), async move {
                match f().await {
                    Ok(page) => {
                        let arc = Arc::new(page);
                        last_good.store(Some(arc.clone()));
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
            Err(e) => match self.last_good.load_full() {
                Some(stale) => {
                    tracing::warn!(error = %e, "public_status compute failed; serving stale");
                    Ok(stale)
                }
                None => {
                    tracing::error!(
                        error = %e,
                        "public_status compute failed and no last-good snapshot; returning Unavailable"
                    );
                    Err(PageCacheError::Unavailable)
                }
            },
        }
    }

    /// Snapshot of the last successful compute, if any. Useful for tests and
    /// for surfacing "data is N seconds old" banners.
    pub fn last_good(&self) -> Option<Arc<PageData>> {
        self.last_good.load_full()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use chrono::Utc;

    use super::*;
    use crate::domain::{OverallState, OverallStatus};

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
        let page = cache
            .get_or_compute(|| async { Ok::<_, std::io::Error>(make_page("ok")) })
            .await
            .expect("first compute ok");
        // Verify Arc identity by cheaply cloning and confirming pointer equality
        // with the cache's last_good snapshot.
        let snap = cache.last_good().expect("snapshot present after success");
        assert!(Arc::ptr_eq(&page, &snap));
        assert_eq!(page.site_name, "ok");
    }

    #[tokio::test]
    async fn second_call_within_ttl_does_not_recompute() {
        let cache = PageCache::new(Duration::from_secs(10));
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let calls = calls.clone();
            cache
                .get_or_compute(|| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, std::io::Error>(make_page("ok"))
                })
                .await
                .expect("ok");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "compute deduplicated by TTL");
    }

    #[tokio::test]
    async fn single_flight_under_concurrency() {
        // Many concurrent callers should collapse to exactly one compute.
        let cache = PageCache::new(Duration::from_secs(10));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..50 {
            let cache = cache.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_compute(|| async move {
                        // Sleep so concurrent waiters pile up on the in-flight future.
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
        assert_eq!(calls.load(Ordering::SeqCst), 1, "single-flight collapsed concurrent calls");
    }

    #[tokio::test]
    async fn serves_stale_when_compute_fails_after_initial_success() {
        let cache = PageCache::new(Duration::from_millis(50));
        // 1) Prime the cache.
        let _good = cache
            .get_or_compute(|| async { Ok::<_, std::io::Error>(make_page("good")) })
            .await
            .expect("prime ok");

        // 2) Wait past TTL so the entry expires and the next call triggers recompute.
        tokio::time::sleep(Duration::from_millis(80)).await;

        // 3) Fail the recompute — must serve stale, not error.
        let stale = cache
            .get_or_compute(|| async {
                Err::<PageData, _>(std::io::Error::other("ch down"))
            })
            .await
            .expect("served stale");
        assert_eq!(stale.site_name, "good");
    }

    #[tokio::test]
    async fn unavailable_when_first_compute_fails_with_no_stale() {
        let cache = PageCache::new(Duration::from_secs(10));
        let err = cache
            .get_or_compute(|| async {
                Err::<PageData, _>(std::io::Error::other("ch down"))
            })
            .await
            .expect_err("no stale, propagates");
        matches!(err, PageCacheError::Unavailable);
    }
}
