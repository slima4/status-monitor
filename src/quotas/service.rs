//! Plan resolution + resource-quota checks.
//!
//! `limit_for_org` is the single read path for an org's effective limits:
//! the plan row, with a per-org `plan_overrides` row folded in (cached; an
//! expired override reverts to the plan default), then the self-host config
//! knob folded in last when, and only when, `tenancy.enabled = false`. The
//! `check_*` methods are the
//! *friendly-error* fast path called at handler entry; the race-safe
//! guarantee lives in the store INSERTs that take the same limit number
//! (one source of truth). Every block is recorded to `quota_events`
//! fire-and-forget.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use serde::Deserialize;
use sqlx::PgPool;

use crate::config::{AppConfig, SelfHostOverrides};
use crate::domain::quota::Plan;
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};

/// Cache-key tags for the usage cache. One vocabulary, equal to the
/// `plans` column names so the transparency endpoint, the UI, and any
/// future invalidation hook all name a quota the same way.
pub mod usage_keys {
    pub const TARGETS: &str = "max_targets";
    pub const MEMBERS: &str = "max_members";
    pub const PENDING_INVITATIONS: &str = "max_pending_invitations";
    pub const PUBLIC_COMPONENTS: &str = "max_public_components";
    pub const MAINTENANCE_WINDOWS: &str = "max_maintenance_windows";
}

/// The org-scoped count queries. Declared once and shared by the atomic
/// friendly-check path *and* the usage snapshot so the number a customer is
/// blocked at always equals the number the usage page shows (single source).
const SQL_COUNT_TARGETS: &str = "SELECT count(*) FROM targets WHERE org_id = $1";
const SQL_COUNT_PUBLIC_COMPONENTS: &str =
    "SELECT count(*) FROM targets WHERE org_id = $1 AND public_status = true";
const SQL_COUNT_MAINTENANCE_WINDOWS: &str =
    "SELECT count(*) FROM maintenance_windows WHERE org_id = $1";

/// Storage-layer row for `plans`. The domain `Plan` stays `sqlx`-free
/// (per the domain/storage split); this is the only place that maps the
/// table. Field order matches the SELECT.
#[derive(sqlx::FromRow)]
struct PlanRow {
    id: String,
    name: String,
    description: String,
    max_targets: i32,
    min_check_interval_secs: i32,
    retention_days: i32,
    max_members: i32,
    max_pending_invitations: i32,
    max_api_tokens_per_user: i32,
    max_public_components: i32,
    max_maintenance_windows: i32,
    max_logo_size_bytes: i32,
    api_writes_per_minute: i32,
    api_reads_per_minute: i32,
    bulk_ops_per_minute: i32,
    test_now_per_minute: i32,
    check_now_per_minute: i32,
    custom_domain_enabled: bool,
    white_label_enabled: bool,
    incident_narration_enabled: bool,
    is_listed: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<PlanRow> for Plan {
    fn from(r: PlanRow) -> Self {
        Plan {
            id: r.id,
            name: r.name,
            description: r.description,
            max_targets: r.max_targets,
            min_check_interval_secs: r.min_check_interval_secs,
            retention_days: r.retention_days,
            max_members: r.max_members,
            max_pending_invitations: r.max_pending_invitations,
            max_api_tokens_per_user: r.max_api_tokens_per_user,
            max_public_components: r.max_public_components,
            max_maintenance_windows: r.max_maintenance_windows,
            max_logo_size_bytes: r.max_logo_size_bytes,
            api_writes_per_minute: r.api_writes_per_minute,
            api_reads_per_minute: r.api_reads_per_minute,
            bulk_ops_per_minute: r.bulk_ops_per_minute,
            test_now_per_minute: r.test_now_per_minute,
            check_now_per_minute: r.check_now_per_minute,
            custom_domain_enabled: r.custom_domain_enabled,
            white_label_enabled: r.white_label_enabled,
            incident_narration_enabled: r.incident_narration_enabled,
            is_listed: r.is_listed,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Clone)]
pub struct QuotaService {
    db: Option<PgPool>,
    tenancy_enabled: bool,
    self_host: SelfHostOverrides,
    /// Org → effective plan. Keyed by org (not plan id) and populated by a
    /// single `organizations ⋈ plans` query, so a steady-state cache hit is
    /// **zero** DB round-trips on the per-request hot path.
    plan_cache: Cache<OrgId, Arc<Plan>>,
    /// `(org, quota_name)` → current count for the usage snapshot. TTL-only
    /// and recompute-from-DB on miss; never incremented, so a write path that
    /// forgets to adjust a counter cannot drift it (the cache contract).
    usage_cache: Cache<(OrgId, &'static str), u32>,
}

/// One org's plan plus its current resource counts. The handler shapes this
/// into the public usage JSON; keeping the service free of the API DTO keeps
/// the count logic in one place and the wire shape in the API layer.
pub struct OrgUsage {
    pub plan: Arc<Plan>,
    pub targets: i64,
    pub members: i64,
    pub pending_invitations: i64,
    pub public_components: i64,
    pub maintenance_windows: i64,
}

impl QuotaService {
    pub fn new(cfg: &AppConfig, db: Option<PgPool>) -> Self {
        let plan_ttl = Duration::from_secs(cfg.quotas.plan_cache_ttl_secs.max(1));
        let usage_ttl = Duration::from_secs(cfg.quotas.usage_cache_ttl_secs.max(1));
        Self {
            db,
            tenancy_enabled: cfg.tenancy.enabled,
            self_host: cfg.quotas.self_host_overrides.clone(),
            plan_cache: Cache::builder()
                .time_to_live(plan_ttl)
                .max_capacity(64)
                .build(),
            usage_cache: Cache::builder()
                .time_to_live(usage_ttl)
                .max_capacity(4096)
                .build(),
        }
    }

    /// Effective plan for an org. Without a DB (in-memory dev/test fixtures
    /// that always run single-tenant) there is no `plans` table, so quotas
    /// are not enforced — return an unlimited synthetic plan.
    pub async fn limit_for_org(&self, org: OrgId) -> Result<Arc<Plan>> {
        let Some(db) = &self.db else {
            return Ok(Arc::new(unlimited_plan()));
        };

        let db2 = db.clone();
        let plan = self
            .plan_cache
            .try_get_with(org, async move {
                // One join: org row → its plan. Cached by org id, so a hit
                // costs zero queries (the cache TTL bounds staleness).
                let p: PlanRow = sqlx::query_as(
                    "SELECT p.id, p.name, p.description, p.max_targets, \
                     p.min_check_interval_secs, p.retention_days, p.max_members, \
                     p.max_pending_invitations, p.max_api_tokens_per_user, \
                     p.max_public_components, p.max_maintenance_windows, \
                     p.max_logo_size_bytes, p.api_writes_per_minute, \
                     p.api_reads_per_minute, p.bulk_ops_per_minute, \
                     p.test_now_per_minute, p.check_now_per_minute, \
                     p.custom_domain_enabled, p.white_label_enabled, \
                     p.incident_narration_enabled, p.is_listed, p.created_at, \
                     p.updated_at \
                     FROM organizations o JOIN plans p ON p.id = o.plan_id \
                     WHERE o.id = $1",
                )
                .bind(org.0)
                .fetch_one(&db2)
                .await?;
                let mut plan: Plan = p.into();
                // Per-org exception (beta customers, friends-of-the-
                // project): a present, unexpired plan_overrides row
                // replaces the named caps. Folded into the cached value, so
                // the TTL bounds an override edit/expiry exactly as it
                // bounds a plans-table edit (same staleness contract).
                if let Some(ov) = plan_override(&db2, org).await? {
                    plan = apply_overrides(&plan, &ov);
                }
                Ok::<Arc<Plan>, sqlx::Error>(Arc::new(plan))
            })
            .await
            .map_err(|e| match e.as_ref() {
                sqlx::Error::RowNotFound => {
                    AppError::not_found(crate::api::error::codes::ORG_NOT_FOUND, "org not found")
                }
                _ => AppError::Other(anyhow::anyhow!("limit_for_org: {e}")),
            })?;

        // Self-host single-tenant only: overrides replace the plan defaults.
        // Ignored entirely in SaaS so a future reader can't be tempted to
        // honour them unconditionally.
        if !self.tenancy_enabled && self.self_host.enabled {
            return Ok(Arc::new(apply_overrides(
                &plan,
                &PlanOverrides::from(&self.self_host),
            )));
        }
        Ok(plan)
    }

    async fn count(&self, sql: &str, org: OrgId) -> Result<i64> {
        let Some(db) = &self.db else { return Ok(0) };
        let n: i64 = sqlx::query_scalar(sql)
            .bind(org.0)
            .fetch_one(db)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("quota count: {e}")))?;
        Ok(n)
    }

    /// Friendly pre-check for `n` new targets. The atomic guarantee is the
    /// `WHERE (count) + n <= limit` inside `TargetStore::create`/`bulk_create`
    /// which is handed the same `max_targets` — this only produces the nice
    /// 422 on the common (uncontended) path.
    pub async fn check_can_create_targets(
        &self,
        org: OrgId,
        user: Option<UserId>,
        n: i64,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_targets);
        let current = self.count(SQL_COUNT_TARGETS, org).await?;
        if current + n > limit {
            self.record_block(org, user, "max_targets", current, limit);
            return Err(AppError::quota_exceeded(
                "max_targets",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    pub async fn check_can_create_maintenance_window(
        &self,
        org: OrgId,
        user: Option<UserId>,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_maintenance_windows);
        let current = self.count(SQL_COUNT_MAINTENANCE_WINDOWS, org).await?;
        if current + 1 > limit {
            self.record_block(org, user, "max_maintenance_windows", current, limit);
            return Err(AppError::quota_exceeded(
                "max_maintenance_windows",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Friendly pre-check for adding one member, so an over-cap invitation
    /// accept fails with a clean 422 before the token is consumed. The
    /// race-safe guarantee is the advisory-locked count inside
    /// `orgs::add_member`, handed the same `max_members`.
    pub async fn check_can_add_member(&self, org: OrgId, user: Option<UserId>) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let Some(db) = &self.db else { return Ok(()) };
        let limit = i64::from(plan.max_members);
        let current = i64::from(crate::storage::orgs::active_member_count(db, org).await?);
        if current + 1 > limit {
            self.record_block(org, user, "max_members", current, limit);
            return Err(AppError::quota_exceeded(
                "max_members",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// `additional` is how many targets this request would newly flip public.
    pub async fn check_public_components(
        &self,
        org: OrgId,
        user: Option<UserId>,
        additional: i64,
    ) -> Result<()> {
        if additional <= 0 {
            return Ok(());
        }
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_public_components);
        let current = self.count(SQL_COUNT_PUBLIC_COMPONENTS, org).await?;
        if current + additional > limit {
            self.record_block(org, user, "max_public_components", current, limit);
            return Err(AppError::quota_exceeded(
                "max_public_components",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Read-through the usage cache for one `(org, quota_name)` count.
    /// TTL-only and recompute-from-DB on miss — never an increment, so a path
    /// that forgets to adjust a counter cannot drift it (the cache contract).
    /// Two racing misses recompute the same idempotent `COUNT(*)`; harmless.
    async fn cached_count<F>(&self, org: OrgId, key: &'static str, compute: F) -> Result<i64>
    where
        F: Future<Output = Result<i64>>,
    {
        if let Some(v) = self.usage_cache.get(&(org, key)).await {
            return Ok(i64::from(v));
        }
        let n = compute.await?.max(0);
        let stored = u32::try_from(n).unwrap_or(u32::MAX);
        self.usage_cache.insert((org, key), stored).await;
        Ok(i64::from(stored))
    }

    /// Plan + current counts for the usage endpoint / UI. Counts go through
    /// the 10 s usage cache. Without a DB (in-memory fixtures) the counts are
    /// zero against the synthetic unlimited plan.
    pub async fn org_usage(&self, org: OrgId) -> Result<OrgUsage> {
        let plan = self.limit_for_org(org).await?;
        let Some(db) = &self.db else {
            return Ok(OrgUsage {
                plan,
                targets: 0,
                members: 0,
                pending_invitations: 0,
                public_components: 0,
                maintenance_windows: 0,
            });
        };
        let targets = self
            .cached_count(org, usage_keys::TARGETS, self.count(SQL_COUNT_TARGETS, org))
            .await?;
        let members = self
            .cached_count(org, usage_keys::MEMBERS, async {
                crate::storage::orgs::active_member_count(db, org)
                    .await
                    .map(i64::from)
            })
            .await?;
        // Reuse the invitation module's "pending" predicate so the usage view
        // and the atomic invite-cap enforcer agree on what counts as pending.
        let pending_invitations = self
            .cached_count(org, usage_keys::PENDING_INVITATIONS, async {
                crate::auth::invitations::count_pending_for_org(db, org)
                    .await
                    .map(i64::from)
            })
            .await?;
        let public_components = self
            .cached_count(
                org,
                usage_keys::PUBLIC_COMPONENTS,
                self.count(SQL_COUNT_PUBLIC_COMPONENTS, org),
            )
            .await?;
        let maintenance_windows = self
            .cached_count(
                org,
                usage_keys::MAINTENANCE_WINDOWS,
                self.count(SQL_COUNT_MAINTENANCE_WINDOWS, org),
            )
            .await?;
        Ok(OrgUsage {
            plan,
            targets,
            members,
            pending_invitations,
            public_components,
            maintenance_windows,
        })
    }

    /// Append a `quota_exceeded` audit row. Fire-and-forget: a failed insert
    /// must never turn a clean 422 into a 500.
    pub fn record_block(
        &self,
        org: OrgId,
        user: Option<UserId>,
        quota_name: &'static str,
        current: i64,
        limit: i64,
    ) {
        record_quota_event(
            self.db.clone(),
            Some(org),
            user,
            "quota_exceeded",
            Some(quota_name),
            serde_json::json!({ "current": current, "limit": limit }),
            None,
        );
    }
}

/// Shared fire-and-forget writer used by quota checks and the rate-limit
/// middleware. No-op without a DB.
pub fn record_quota_event(
    db: Option<PgPool>,
    org: Option<OrgId>,
    user: Option<UserId>,
    event: &'static str,
    quota_name: Option<&'static str>,
    details: serde_json::Value,
    ip_hash: Option<String>,
) {
    let Some(db) = db else { return };
    tokio::spawn(async move {
        let res = sqlx::query(
            "INSERT INTO quota_events (org_id, user_id, event, quota_name, details, ip_hash) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(org.map(|o| o.0))
        .bind(user.map(|u| u.0))
        .bind(event)
        .bind(quota_name)
        .bind(details)
        .bind(ip_hash)
        .execute(&db)
        .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "quota_events insert failed (non-fatal)");
        }
    });
}

/// The cap fields a limit override may set. Deserialized from a
/// `plan_overrides.override_json` row; also the merge input for the
/// self-host config knob (via `From`), so `apply_overrides` is the one
/// merge for both. `deny_unknown_fields` so a typo'd admin key (`max_target`)
/// is a loud parse error the caller logs and ignores, not a silent no-op.
/// Deliberately has no `enabled` flag — whether an override applies is the
/// caller's decision (a present unexpired row; or the self-host gate), never
/// a property of the cap bag itself.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PlanOverrides {
    max_targets: Option<u32>,
    min_check_interval_secs: Option<u32>,
    retention_days: Option<u32>,
    max_members: Option<u32>,
    max_pending_invitations: Option<u32>,
    max_api_tokens_per_user: Option<u32>,
    max_public_components: Option<u32>,
    max_maintenance_windows: Option<u32>,
    max_logo_size_bytes: Option<u32>,
}

impl From<&SelfHostOverrides> for PlanOverrides {
    fn from(o: &SelfHostOverrides) -> Self {
        Self {
            max_targets: o.max_targets,
            min_check_interval_secs: o.min_check_interval_secs,
            retention_days: o.retention_days,
            max_members: o.max_members,
            max_pending_invitations: o.max_pending_invitations,
            max_api_tokens_per_user: o.max_api_tokens_per_user,
            max_public_components: o.max_public_components,
            max_maintenance_windows: o.max_maintenance_windows,
            max_logo_size_bytes: o.max_logo_size_bytes,
        }
    }
}

fn apply_overrides(base: &Plan, ov: &PlanOverrides) -> Plan {
    let mut p = base.clone();
    let take = |v: Option<u32>, cur: i32| {
        v.map(|x| i32::try_from(x).unwrap_or(i32::MAX))
            .unwrap_or(cur)
    };
    p.max_targets = take(ov.max_targets, p.max_targets);
    p.min_check_interval_secs = take(ov.min_check_interval_secs, p.min_check_interval_secs);
    p.retention_days = take(ov.retention_days, p.retention_days);
    p.max_members = take(ov.max_members, p.max_members);
    p.max_pending_invitations = take(ov.max_pending_invitations, p.max_pending_invitations);
    p.max_api_tokens_per_user = take(ov.max_api_tokens_per_user, p.max_api_tokens_per_user);
    p.max_public_components = take(ov.max_public_components, p.max_public_components);
    p.max_maintenance_windows = take(ov.max_maintenance_windows, p.max_maintenance_windows);
    p.max_logo_size_bytes = take(ov.max_logo_size_bytes, p.max_logo_size_bytes);
    p
}

/// Active per-org limit override, if any. Expired rows are filtered in SQL,
/// so an override past its `expires_at` reverts to the plan default. A
/// *query* error propagates (so the caller's cache does not memoize a
/// degraded plan for the whole TTL on a transient DB blip — a healthy
/// override must not be dropped by unrelated infra trouble). Only the
/// benign cases are `Ok(None)`: no row, or a malformed `override_json`
/// (logged) — a bad admin row must never take an org's limits down with it.
async fn plan_override(db: &PgPool, org: OrgId) -> Result<Option<PlanOverrides>, sqlx::Error> {
    let json: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT override_json FROM plan_overrides \
         WHERE org_id = $1 AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(org.0)
    .fetch_optional(db)
    .await?;
    let Some(json) = json else { return Ok(None) };
    match serde_json::from_value::<PlanOverrides>(json) {
        Ok(ov) => Ok(Some(ov)),
        Err(e) => {
            tracing::warn!(error = %e, "plan_overrides override_json invalid; ignoring");
            Ok(None)
        }
    }
}

/// Unlimited plan used when there is no `plans` table to consult (DB-less
/// in-memory test/dev fixtures). Every cap is `i32::MAX`.
fn unlimited_plan() -> Plan {
    let now = chrono::Utc::now();
    Plan {
        id: "self-host".into(),
        name: "Self-host".into(),
        description: "Unlimited (no plans table)".into(),
        max_targets: i32::MAX,
        min_check_interval_secs: 1,
        retention_days: i32::MAX,
        max_members: i32::MAX,
        max_pending_invitations: i32::MAX,
        max_api_tokens_per_user: i32::MAX,
        max_public_components: i32::MAX,
        max_maintenance_windows: i32::MAX,
        max_logo_size_bytes: i32::MAX,
        api_writes_per_minute: i32::MAX,
        api_reads_per_minute: i32::MAX,
        bulk_ops_per_minute: i32::MAX,
        test_now_per_minute: i32::MAX,
        check_now_per_minute: i32::MAX,
        custom_domain_enabled: false,
        white_label_enabled: false,
        incident_narration_enabled: true,
        is_listed: false,
        created_at: now,
        updated_at: now,
    }
}
