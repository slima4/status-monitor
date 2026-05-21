//! Per-org cache for the public status page payload.
//!
//! Two bounded `moka` layers per process:
//!  * `inner` — hot cache. TTL eviction + capacity-bounded LRU + single-flight
//!    (`try_get_with` collapses concurrent misses for one org into one
//!    compute).
//!  * `last_good` — last-known-good fallback. Survives `inner`'s TTL eviction
//!    so a transient ClickHouse/Postgres hiccup keeps the page up with stale
//!    data instead of 5xx-ing anonymous callers. Bounded by capacity *and*
//!    idle eviction so a churned/deleted tenant the purge worker has not yet
//!    reached cannot keep its snapshot resident forever.
//!
//! Cross-tenant isolation: each `OrgId` has its own slot in both layers, so an
//! org's compute failure can never serve another org's stale data, and a hot
//! org's recompute can't block another org's request.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache as FutureCache;
use moka::sync::Cache as SyncCache;

use crate::config::PublicStatusConfig;
use crate::domain::{OrgId, PublicStatusPage};

pub type PageData = PublicStatusPage;

#[derive(Debug, thiserror::Error)]
pub enum PageCacheError {
    /// Compute failed and no last-known-good snapshot exists yet.
    #[error("status page unavailable: no cached data and recompute failed")]
    Unavailable,
}

/// In-process per-org cache for `PageData`. Cheap to clone — both `moka`
/// handles are `Arc`-backed internally.
#[derive(Clone)]
pub struct PageCache {
    inner: FutureCache<OrgId, Arc<PageData>>,
    last_good: SyncCache<OrgId, Arc<PageData>>,
}

impl PageCache {
    /// Production constructor. Sizes both layers from `[public_status]`:
    /// `inner` holds `cache_max_orgs` entries for `cache_ttl_secs`;
    /// `last_good` holds 4× that (so a brief storm cycling many orgs through
    /// `inner` still finds a stale snapshot) and idle-evicts after
    /// `last_good_ttl_secs` to cap the heap under tenant churn.
    pub fn new(cfg: &PublicStatusConfig) -> Self {
        Self::build(
            u64::from(cfg.cache_max_orgs),
            Duration::from_secs(cfg.cache_ttl_secs),
            Duration::from_secs(cfg.last_good_ttl_secs),
        )
    }

    fn build(max_orgs: u64, ttl: Duration, last_good_idle: Duration) -> Self {
        Self {
            inner: FutureCache::builder()
                .max_capacity(max_orgs)
                .time_to_live(ttl)
                .build(),
            last_good: SyncCache::builder()
                .max_capacity(max_orgs.saturating_mul(4))
                .time_to_idle(last_good_idle)
                .build(),
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
        let last_good = self.last_good.clone();
        let res = self
            .inner
            .try_get_with(org, async move {
                match f().await {
                    Ok(page) => {
                        let arc = Arc::new(page);
                        last_good.insert(org, arc.clone());
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
            Err(e) => match self.last_good.get(&org) {
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

    /// Drop both layers for `org`. Called by the purge worker once an org's
    /// rows are gone (so `last_good` can't outlive the data behind it) and
    /// by the settings handler on a `public_status_enabled` flip in either
    /// direction (so a stale snapshot can't survive a disable→enable cycle).
    pub async fn invalidate(&self, org: OrgId) {
        self.inner.invalidate(&org).await;
        self.last_good.invalidate(&org);
    }

    /// Snapshot of the last successful compute for `org`, if any. Useful for
    /// tests and for surfacing "data is N seconds old" banners.
    pub fn last_good(&self, org: OrgId) -> Option<Arc<PageData>> {
        self.last_good.get(&org)
    }
}

#[cfg(test)]
impl PageCache {
    /// Test constructor with a caller-chosen hot TTL (sub-second precision the
    /// seconds-granularity config can't express) and a long `last_good` idle
    /// window so fallback assertions aren't racing eviction.
    pub fn for_test(ttl: Duration) -> Self {
        Self::build(1024, ttl, Duration::from_secs(3600))
    }

    /// Live entry count of the `last_good` layer. moka evicts lazily, so the
    /// pending-maintenance queue is drained first to make the count reflect
    /// post-eviction reality, which the memory-bound assertions rely on.
    pub fn last_good_len(&self) -> u64 {
        self.last_good.run_pending_tasks();
        self.last_good.entry_count()
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
        let cache = PageCache::for_test(Duration::from_secs(10));
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
        let cache = PageCache::for_test(Duration::from_secs(10));
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
        let cache = PageCache::for_test(Duration::from_secs(10));
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
        let cache = PageCache::for_test(Duration::from_secs(10));
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
        let cache = PageCache::for_test(Duration::from_millis(50));
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
        let cache = PageCache::for_test(Duration::from_secs(10));
        let o = org();
        let err = cache
            .get_or_compute(o, || async {
                Err::<PageData, _>(std::io::Error::other("ch down"))
            })
            .await
            .expect_err("no stale, propagates");
        assert!(matches!(err, PageCacheError::Unavailable));
    }

    #[tokio::test]
    async fn invalidate_drops_both_layers_for_one_org_only() {
        let cache = PageCache::for_test(Duration::from_secs(10));
        let a = org();
        let b = org();
        cache
            .get_or_compute(a, || async { Ok::<_, std::io::Error>(make_page("a")) })
            .await
            .expect("a ok");
        cache
            .get_or_compute(b, || async { Ok::<_, std::io::Error>(make_page("b")) })
            .await
            .expect("b ok");

        cache.invalidate(a).await;
        // moka's sync cache applies invalidation synchronously for the keyed
        // form, so A's snapshot is gone immediately.
        assert!(cache.last_good(a).is_none(), "A snapshot dropped");
        assert!(
            cache.last_good(b).is_some(),
            "B snapshot survives A's invalidation"
        );

        // A's next compute must run fresh (hot entry was dropped too).
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        cache
            .get_or_compute(a, || async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, std::io::Error>(make_page("a2"))
            })
            .await
            .expect("a recompute");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "hot entry was invalidated");
    }

    // `last_good` must stay bounded under tenant churn: a storm that cycles
    // far more orgs than capacity through a successful compute must not let
    // the stale layer grow without limit.
    #[tokio::test]
    async fn last_good_stays_bounded_under_org_churn() {
        // build() sizes last_good at 4× max_orgs, so cap here is 20.
        let max_orgs = 5;
        let cap = max_orgs * 4;
        let cache = PageCache::build(max_orgs, Duration::from_secs(60), Duration::from_secs(3600));

        // Well past the 20-entry cap is enough to prove the bound holds.
        let churn = 100u64;
        for _ in 0..churn {
            let o = org();
            cache
                .get_or_compute(o, || async { Ok::<_, std::io::Error>(make_page("ok")) })
                .await
                .expect("compute ok");
        }

        let len = cache.last_good_len();
        assert!(
            len <= cap,
            "last_good grew past its {cap}-entry bound: {len} after {churn} distinct orgs"
        );
    }

    // Idle orgs must be reclaimed so a one-time traffic spike doesn't pin
    // every org's snapshot in memory forever.
    #[tokio::test]
    async fn last_good_idle_evicts_after_ttl() {
        let cache = PageCache::build(64, Duration::from_secs(60), Duration::from_millis(50));
        let o = org();
        cache
            .get_or_compute(o, || async { Ok::<_, std::io::Error>(make_page("ok")) })
            .await
            .expect("compute ok");
        assert_eq!(
            cache.last_good_len(),
            1,
            "snapshot present right after seed"
        );

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(
            cache.last_good_len(),
            0,
            "idle snapshot must be evicted past last_good idle window"
        );
    }
}
