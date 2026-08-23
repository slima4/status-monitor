//! Plan resolution + resource-quota checks.
//!
//! `limit_for_org` is the single read path for an org's effective limits:
//! the plan row, with a per-org `plan_overrides` row folded in (cached; an
//! expired override reverts to the plan default). The `check_*` methods are
//! the *friendly-error* fast path called at handler entry; the race-safe
//! guarantee lives in the store INSERTs that take the same limit number
//! (one source of truth). Every block is recorded to `quota_events`
//! fire-and-forget.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use serde::Deserialize;
use sqlx::PgPool;

use std::collections::HashMap;

use anyhow::Context;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::domain::quota::{Plan, evidence_ttl_days, raw_ttl_days};
use crate::domain::{OrgId, UserId};
use crate::error::{AppError, Result};
use crate::storage::{ClampedRange, TimeRange};

/// The two physical windows a written row is stamped with: how long the row
/// lives, and how long a failed flow run's page snapshot lives inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionDays {
    pub row: u16,
    pub evidence: u16,
}

/// Bulk `org_id → physical retention days`, one query, the same ceilings as
/// [`Plan::raw_window_days`] via [`raw_ttl_days`] / [`evidence_ttl_days`]. Feeds
/// the write-path TTL snapshot ([`crate::storage::org_ttl`]), which must read
/// every active org without thrashing the per-org plan cache. Plan-level:
/// neither column carries a per-org override or add-on today, so no override
/// folding is applied.
pub async fn retention_days_by_org(pool: &PgPool) -> Result<HashMap<Uuid, RetentionDays>> {
    let rows: Vec<(Uuid, i32, i32)> = sqlx::query_as(
        "SELECT o.id, p.raw_days, p.evidence_days \
         FROM organizations o JOIN plans p ON p.id = o.plan_id",
    )
    .fetch_all(pool)
    .await
    .context("load retention days by org")?;
    Ok(rows
        .into_iter()
        .map(|(id, raw, evidence)| {
            (
                id,
                RetentionDays {
                    row: raw_ttl_days(raw),
                    evidence: evidence_ttl_days(evidence, raw),
                },
            )
        })
        .collect())
}

/// Cache-key tags for the usage cache. One vocabulary, equal to the
/// `plans` column names so the transparency endpoint, the UI, and any
/// future invalidation hook all name a quota the same way.
pub mod usage_keys {
    pub const TARGETS: &str = "max_targets";
    pub const MEMBERS: &str = "max_members";
    pub const PENDING_INVITATIONS: &str = "max_pending_invitations";
    /// Not a `plans` column: an abuse ceiling, equal for every plan.
    pub const INVITATION_SENDS: &str = "invitation_sends_per_window";
    pub const PUBLIC_COMPONENTS: &str = "max_public_components";
    pub const STATUS_PAGES: &str = "max_status_pages";
    pub const MAINTENANCE_WINDOWS: &str = "max_maintenance_windows";
    pub const NOTIFICATION_CHANNELS: &str = "max_notification_channels";
    pub const ESCALATION_POLICIES: &str = "max_escalation_policies";
    pub const ON_CALL_SCHEDULES: &str = "max_on_call_schedules";
    pub const FLOW_CHECKS: &str = "max_flow_checks";
}

/// The org-scoped count queries. Declared once and shared by the atomic
/// friendly-check path *and* the usage snapshot so the number a customer is
/// blocked at always equals the number the usage page shows (single source).
const SQL_COUNT_TARGETS: &str = "SELECT count(*) FROM targets WHERE org_id = $1";
const SQL_COUNT_FLOW: &str = "SELECT count(*) FROM targets WHERE org_id = $1 AND kind = 'flow'";
// Public components are now distinct monitors curated onto any page — the
// per-org cap counts a monitor once no matter how many pages it sits on.
const SQL_COUNT_PUBLIC_COMPONENTS: &str =
    "SELECT count(DISTINCT target_id) FROM status_page_components WHERE org_id = $1";
const SQL_COUNT_STATUS_PAGES: &str = "SELECT count(*) FROM status_pages WHERE org_id = $1";
const SQL_COUNT_MAINTENANCE_WINDOWS: &str =
    "SELECT count(*) FROM maintenance_windows WHERE org_id = $1";
const SQL_COUNT_NOTIFICATION_CHANNELS: &str =
    "SELECT count(*) FROM notification_channels WHERE org_id = $1";
const SQL_COUNT_ESCALATION_POLICIES: &str =
    "SELECT count(*) FROM escalation_policies WHERE org_id = $1 AND deleted_at IS NULL";
const SQL_COUNT_ON_CALL_SCHEDULES: &str =
    "SELECT count(*) FROM on_call_schedules WHERE org_id = $1 AND deleted_at IS NULL";

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
    raw_days: i32,
    evidence_days: i32,
    max_members: i32,
    max_pending_invitations: i32,
    max_api_tokens_per_user: i32,
    max_public_components: i32,
    max_status_pages: i32,
    max_share_links_per_monitor: i32,
    max_shared_monitors: i32,
    max_maintenance_windows: i32,
    max_notification_channels: i32,
    max_escalation_policies: i32,
    max_on_call_schedules: i32,
    max_logo_size_bytes: i32,
    max_regions: i32,
    api_writes_per_minute: i32,
    api_reads_per_minute: i32,
    bulk_ops_per_minute: i32,
    test_now_per_minute: i32,
    check_now_per_minute: i32,
    custom_domain_enabled: bool,
    white_label_enabled: bool,
    sms_alerts_enabled: bool,
    incident_narration_enabled: bool,
    on_call_enabled: bool,
    max_flow_checks: i32,
    max_flow_steps: i32,
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
            raw_days: r.raw_days,
            evidence_days: r.evidence_days,
            max_members: r.max_members,
            max_pending_invitations: r.max_pending_invitations,
            max_api_tokens_per_user: r.max_api_tokens_per_user,
            max_public_components: r.max_public_components,
            max_status_pages: r.max_status_pages,
            max_share_links_per_monitor: r.max_share_links_per_monitor,
            max_shared_monitors: r.max_shared_monitors,
            max_maintenance_windows: r.max_maintenance_windows,
            max_notification_channels: r.max_notification_channels,
            max_escalation_policies: r.max_escalation_policies,
            max_on_call_schedules: r.max_on_call_schedules,
            max_logo_size_bytes: r.max_logo_size_bytes,
            max_regions: r.max_regions,
            api_writes_per_minute: r.api_writes_per_minute,
            api_reads_per_minute: r.api_reads_per_minute,
            bulk_ops_per_minute: r.bulk_ops_per_minute,
            test_now_per_minute: r.test_now_per_minute,
            check_now_per_minute: r.check_now_per_minute,
            custom_domain_enabled: r.custom_domain_enabled,
            white_label_enabled: r.white_label_enabled,
            sms_alerts_enabled: r.sms_alerts_enabled,
            incident_narration_enabled: r.incident_narration_enabled,
            on_call_enabled: r.on_call_enabled,
            max_flow_checks: r.max_flow_checks,
            max_flow_steps: r.max_flow_steps,
            is_listed: r.is_listed,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Clone)]
pub struct QuotaService {
    db: Option<PgPool>,
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
    pub status_pages: i64,
    pub maintenance_windows: i64,
    pub notification_channels: i64,
}

impl QuotaService {
    pub fn new(cfg: &AppConfig, db: Option<PgPool>) -> Self {
        let plan_ttl = Duration::from_secs(cfg.quotas.plan_cache_ttl_secs.max(1));
        let usage_ttl = Duration::from_secs(cfg.quotas.usage_cache_ttl_secs.max(1));
        Self {
            db,
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
                     p.min_check_interval_secs, p.retention_days, p.raw_days, p.evidence_days, \
                     p.max_members, \
                     p.max_pending_invitations, p.max_api_tokens_per_user, \
                     p.max_public_components, p.max_status_pages, \
                     p.max_share_links_per_monitor, p.max_shared_monitors, \
                     p.max_maintenance_windows, \
                     p.max_notification_channels, p.max_escalation_policies, \
                     p.max_on_call_schedules, \
                     p.max_logo_size_bytes, p.max_regions, \
                     p.api_writes_per_minute, \
                     p.api_reads_per_minute, p.bulk_ops_per_minute, \
                     p.test_now_per_minute, p.check_now_per_minute, \
                     p.custom_domain_enabled, p.white_label_enabled, \
                     p.sms_alerts_enabled, p.incident_narration_enabled, \
                     p.on_call_enabled, p.max_flow_checks, p.max_flow_steps, \
                     p.is_listed, p.created_at, p.updated_at \
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
                // Billed add-ons stack on the resolved plan/override base.
                plan = apply_addons(&plan, &org_addons(&db2, org).await?);
                Ok::<Arc<Plan>, sqlx::Error>(Arc::new(plan))
            })
            .await
            .map_err(|e| match e.as_ref() {
                sqlx::Error::RowNotFound => {
                    AppError::not_found(crate::api::error::codes::ORG_NOT_FOUND, "org not found")
                }
                _ => AppError::Other(anyhow::anyhow!("limit_for_org: {e}")),
            })?;

        Ok(plan)
    }

    /// Clamp a read range to the org's raw forensics window (raw-table reads).
    pub async fn clamp_raw(&self, org: OrgId, range: TimeRange) -> Result<ClampedRange> {
        let plan = self.limit_for_org(org).await?;
        Ok(ClampedRange::for_window(
            range,
            plan.raw_window_days(),
            chrono::Utc::now(),
        ))
    }

    /// Clamp a read range to the org's history window (rollup/chart reads).
    pub async fn clamp_history(&self, org: OrgId, range: TimeRange) -> Result<ClampedRange> {
        let plan = self.limit_for_org(org).await?;
        Ok(ClampedRange::for_window(
            range,
            plan.history_window_days(),
            chrono::Utc::now(),
        ))
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

    /// Friendly pre-check for `n` new flow monitors against the plan's
    /// `max_flow_checks`. Flow is heavy (a browser per check), so it carries a
    /// tighter sub-cap than the overall monitor limit. `0` is also the gate,
    /// caught earlier by `gate_flow` with a clearer message.
    pub async fn check_can_create_flow(
        &self,
        org: OrgId,
        user: Option<UserId>,
        n: i64,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_flow_checks);
        let current = self.count(SQL_COUNT_FLOW, org).await?;
        if current + n > limit {
            self.record_block(org, user, usage_keys::FLOW_CHECKS, current, limit);
            return Err(AppError::quota_exceeded(
                usage_keys::FLOW_CHECKS,
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Region-assignment cap. `requested` is the size of the region set the
    /// caller wants for one monitor (assignment replaces the set, so this is the
    /// whole count, not an increment). Audits a block like every other cap.
    pub async fn check_region_assignment(
        &self,
        org: OrgId,
        user: Option<UserId>,
        requested: i64,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_regions);
        if requested > limit {
            self.record_block(org, user, "max_regions", requested, limit);
            return Err(AppError::quota_exceeded(
                "max_regions",
                requested,
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

    /// Friendly pre-check for one new status page. The race-safe guarantee is
    /// the count-subquery + advisory lock inside `StatusPageStore::create`,
    /// handed the same `max_status_pages` cap.
    pub async fn check_can_create_status_page(
        &self,
        org: OrgId,
        user: Option<UserId>,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_status_pages);
        let current = self.count(SQL_COUNT_STATUS_PAGES, org).await?;
        if current + 1 > limit {
            self.record_block(org, user, "max_status_pages", current, limit);
            return Err(AppError::quota_exceeded(
                "max_status_pages",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Friendly pre-check for one new notification channel. The race-safe
    /// guarantee is the count-subquery + advisory lock inside
    /// `NotificationChannelStore::create`, handed the same
    /// `max_notification_channels`; this only produces the nice 422 on the
    /// common (uncontended) path.
    pub async fn check_can_create_notification_channel(
        &self,
        org: OrgId,
        user: Option<UserId>,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_notification_channels);
        let current = self.count(SQL_COUNT_NOTIFICATION_CHANNELS, org).await?;
        if current + 1 > limit {
            self.record_block(org, user, "max_notification_channels", current, limit);
            return Err(AppError::quota_exceeded(
                "max_notification_channels",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Friendly pre-check for one new escalation policy. The race-safe
    /// guarantee is the count-subquery + advisory lock inside
    /// `EscalationPolicyStore::create`, handed the same `max_escalation_policies`.
    pub async fn check_can_create_escalation_policy(
        &self,
        org: OrgId,
        user: Option<UserId>,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_escalation_policies);
        let current = self.count(SQL_COUNT_ESCALATION_POLICIES, org).await?;
        if current + 1 > limit {
            self.record_block(org, user, "max_escalation_policies", current, limit);
            return Err(AppError::quota_exceeded(
                "max_escalation_policies",
                current,
                limit,
                plan.id.clone(),
            ));
        }
        Ok(())
    }

    /// Friendly pre-check for one new on-call schedule. The race-safe guarantee
    /// is the count-subquery + advisory lock inside `OnCallStore::create`,
    /// handed the same `max_on_call_schedules`.
    pub async fn check_can_create_on_call_schedule(
        &self,
        org: OrgId,
        user: Option<UserId>,
    ) -> Result<()> {
        let plan = self.limit_for_org(org).await?;
        let limit = i64::from(plan.max_on_call_schedules);
        let current = self.count(SQL_COUNT_ON_CALL_SCHEDULES, org).await?;
        if current + 1 > limit {
            self.record_block(org, user, "max_on_call_schedules", current, limit);
            return Err(AppError::quota_exceeded(
                "max_on_call_schedules",
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
                status_pages: 0,
                maintenance_windows: 0,
                notification_channels: 0,
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
        let status_pages = self
            .cached_count(
                org,
                usage_keys::STATUS_PAGES,
                self.count(SQL_COUNT_STATUS_PAGES, org),
            )
            .await?;
        let maintenance_windows = self
            .cached_count(
                org,
                usage_keys::MAINTENANCE_WINDOWS,
                self.count(SQL_COUNT_MAINTENANCE_WINDOWS, org),
            )
            .await?;
        let notification_channels = self
            .cached_count(
                org,
                usage_keys::NOTIFICATION_CHANNELS,
                self.count(SQL_COUNT_NOTIFICATION_CHANNELS, org),
            )
            .await?;
        Ok(OrgUsage {
            plan,
            targets,
            members,
            pending_invitations,
            public_components,
            status_pages,
            maintenance_windows,
            notification_channels,
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

/// Append a best-effort row to `quota_events`.
///
/// Fire-and-forget by design: the rate-limit reject path is already-hot
/// and already-degraded; a failing INSERT here must never escalate into
/// the user's response. Callers therefore get no return value and the
/// only failure handling is a `warn!` log line.
///
/// **Durability contract:** `quota_events` is the high-volume
/// observability stream — rate-limit hits, quota blocks, abuse rejects.
/// Under DB pressure (the exact condition rate-limit blocks happen
/// most often) some rows can be lost. Readers must treat the table as a
/// best-effort sample, never as an authoritative audit trail.
///
/// Compliance-grade audit (GDPR DSR, SOC2) goes to `org_audit_log` via
/// [`crate::storage::orgs::record_audit_tx`] — a different table with a
/// different durability contract, by design.
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
/// merge for both. Unknown keys are ignored (not `deny_unknown_fields`): a
/// future cap added to `Plan` but not yet here, or a typo, must only fail to
/// apply that one key — never reject the whole row and silently revert the org
/// to plan defaults. Deliberately has no `enabled` flag — whether an override
/// applies is the caller's decision (a present unexpired row; or the self-host
/// gate), never a property of the cap bag itself.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PlanOverrides {
    max_targets: Option<u32>,
    min_check_interval_secs: Option<u32>,
    retention_days: Option<u32>,
    max_members: Option<u32>,
    max_pending_invitations: Option<u32>,
    max_api_tokens_per_user: Option<u32>,
    max_public_components: Option<u32>,
    max_status_pages: Option<u32>,
    max_share_links_per_monitor: Option<u32>,
    max_shared_monitors: Option<u32>,
    max_maintenance_windows: Option<u32>,
    max_notification_channels: Option<u32>,
    max_escalation_policies: Option<u32>,
    max_on_call_schedules: Option<u32>,
    max_regions: Option<u32>,
    max_logo_size_bytes: Option<u32>,
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
    p.max_status_pages = take(ov.max_status_pages, p.max_status_pages);
    p.max_share_links_per_monitor = take(
        ov.max_share_links_per_monitor,
        p.max_share_links_per_monitor,
    );
    p.max_shared_monitors = take(ov.max_shared_monitors, p.max_shared_monitors);
    p.max_maintenance_windows = take(ov.max_maintenance_windows, p.max_maintenance_windows);
    p.max_notification_channels = take(ov.max_notification_channels, p.max_notification_channels);
    p.max_escalation_policies = take(ov.max_escalation_policies, p.max_escalation_policies);
    p.max_on_call_schedules = take(ov.max_on_call_schedules, p.max_on_call_schedules);
    p.max_regions = take(ov.max_regions, p.max_regions);
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

/// Additive, billed capacity on top of the base plan (Stripe quantity items).
/// Unlike `PlanOverrides` (which replaces a cap), add-ons stack on the resolved
/// plan/override. Count caps only. Summed per type from `org_addons`.
#[derive(Debug, Default)]
struct Addons {
    extra_targets: i64,
    extra_status_pages: i64,
    extra_members: i64,
    extra_shared_monitors: i64,
    extra_notification_channels: i64,
}

fn apply_addons(base: &Plan, a: &Addons) -> Plan {
    let mut p = base.clone();
    // Saturate at i32::MAX; clamp negatives to 0 — a bad row must never shrink a cap.
    let add = |cur: i32, extra: i64| {
        i32::try_from(i64::from(cur).saturating_add(extra.max(0))).unwrap_or(i32::MAX)
    };
    p.max_targets = add(p.max_targets, a.extra_targets);
    p.max_status_pages = add(p.max_status_pages, a.extra_status_pages);
    p.max_members = add(p.max_members, a.extra_members);
    p.max_shared_monitors = add(p.max_shared_monitors, a.extra_shared_monitors);
    p.max_notification_channels = add(p.max_notification_channels, a.extra_notification_channels);
    p
}

/// Add-on quantities for an org, summed by type. Like `plan_override`: a query
/// error propagates (never memoize a degraded plan on a transient blip); empty
/// is `Ok(default)`. Unknown type (CHECK makes it impossible) is logged + ignored.
async fn org_addons(db: &PgPool, org: OrgId) -> Result<Addons, sqlx::Error> {
    // PK (org_id, addon_type) → at most one row per type, no aggregation needed.
    let rows: Vec<(String, i32)> =
        sqlx::query_as("SELECT addon_type, quantity FROM org_addons WHERE org_id = $1")
            .bind(org.0)
            .fetch_all(db)
            .await?;
    let mut a = Addons::default();
    for (kind, qty) in rows {
        let qty = i64::from(qty);
        match kind.as_str() {
            "extra_targets" => a.extra_targets = qty,
            "extra_status_pages" => a.extra_status_pages = qty,
            "extra_members" => a.extra_members = qty,
            "extra_shared_monitors" => a.extra_shared_monitors = qty,
            "extra_notification_channels" => a.extra_notification_channels = qty,
            other => tracing::warn!(addon_type = other, "unknown org_addons type; ignoring"),
        }
    }
    Ok(a)
}

/// Unlimited plan used when there is no `plans` table to consult (DB-less
/// in-memory test/dev fixtures). Every cap is `i32::MAX`.
pub(crate) fn unlimited_plan() -> Plan {
    let now = chrono::Utc::now();
    Plan {
        id: "self-host".into(),
        name: "Self-host".into(),
        description: "Unlimited (no plans table)".into(),
        max_targets: i32::MAX,
        min_check_interval_secs: 1,
        retention_days: i32::MAX,
        raw_days: i32::MAX,
        evidence_days: i32::MAX,
        max_members: i32::MAX,
        max_pending_invitations: i32::MAX,
        max_api_tokens_per_user: i32::MAX,
        max_public_components: i32::MAX,
        max_status_pages: i32::MAX,
        max_share_links_per_monitor: i32::MAX,
        max_shared_monitors: i32::MAX,
        max_maintenance_windows: i32::MAX,
        max_notification_channels: i32::MAX,
        max_escalation_policies: i32::MAX,
        max_on_call_schedules: i32::MAX,
        max_logo_size_bytes: i32::MAX,
        max_regions: i32::MAX,
        api_writes_per_minute: i32::MAX,
        api_reads_per_minute: i32::MAX,
        bulk_ops_per_minute: i32::MAX,
        test_now_per_minute: i32::MAX,
        check_now_per_minute: i32::MAX,
        custom_domain_enabled: false,
        white_label_enabled: false,
        sms_alerts_enabled: false,
        incident_narration_enabled: true,
        on_call_enabled: true,
        // Matches the seed: flow stays off until the plan row raises the cap.
        max_flow_checks: 0,
        max_flow_steps: 30,
        is_listed: false,
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;

    #[test]
    fn unknown_keys_are_ignored_and_new_caps_apply() {
        // Regression: deny_unknown_fields made any override naming a not-yet-
        // listed cap (max_regions/...) fail to parse, dropping the WHOLE
        // override and silently reverting the org to plan defaults.
        let json = serde_json::json!({
            "max_regions": 7,
            "max_escalation_policies": 3,
            "totally_unknown_future_key": 99,
        });
        let ov: PlanOverrides =
            serde_json::from_value(json).expect("unknown keys must be ignored, not rejected");
        assert_eq!(ov.max_regions, Some(7));
        assert_eq!(ov.max_escalation_policies, Some(3));

        let base = unlimited_plan();
        let merged = apply_overrides(&base, &ov);
        assert_eq!(merged.max_regions, 7);
        assert_eq!(merged.max_escalation_policies, 3);
        assert_eq!(
            merged.max_targets, base.max_targets,
            "untouched cap unchanged"
        );
    }
}
