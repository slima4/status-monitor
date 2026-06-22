//! Storage layer for operator narration of incidents.
//!
//! Separate from `public_status::incident_writer::IncidentStore` (which only
//! exposes open/insert/close to the background materialiser) so the operator
//! surface can read full incident rows and append public update entries
//! without widening the writer trait.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{
    CheckStatus, Incident, IncidentNarrationUpdate, IncidentSeverity, IncidentStatusPhase,
    NewIncidentUpdate, OrgId, PublicIncidentUpdate,
};
use std::collections::HashMap;

use crate::error::Result;
use crate::storage::TimeRange;

/// One open incident joined with its target name + latest update.
/// Powers the operator dashboard banner.
#[derive(Debug, Clone)]
pub struct ActiveIncident {
    pub id: Uuid,
    pub target_id: Uuid,
    pub target_name: String,
    pub severity: IncidentSeverity,
    pub started_at: DateTime<Utc>,
    pub public_title: Option<String>,
    pub latest_update: Option<PublicIncidentUpdate>,
}

/// Operator-facing incident narration repository. Every method takes the
/// caller's `org` (resolved from `CurrentOrg`) and refuses to touch any other
/// tenant's rows. Compile-time scoping means "forgot to pass the org" is a
/// type error, not a cross-tenant leak.
#[async_trait]
pub trait IncidentNarrationStore: Send + Sync {
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<Incident>>;
    async fn patch_narration(
        &self,
        org: OrgId,
        id: Uuid,
        update: IncidentNarrationUpdate,
    ) -> Result<Option<Incident>>;
    async fn append_update(
        &self,
        org: OrgId,
        incident_id: Uuid,
        new: NewIncidentUpdate,
        author: Option<String>,
    ) -> Result<Option<PublicIncidentUpdate>>;
    /// Currently-open incidents across every target in `org`, oldest-first
    /// (so the banner shows "first opened …" without re-sorting). Capped at
    /// `limit` for the dashboard banner.
    async fn list_active(&self, org: OrgId, limit: usize) -> Result<Vec<ActiveIncident>>;
    /// Confirmed incidents for one target overlapping `range`, newest first. An
    /// incident still open at `range.to` is included; `ongoing_only` keeps only those.
    async fn list_for_target(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
        offset: usize,
        ongoing_only: bool,
    ) -> Result<Vec<Incident>>;
    /// Confirmed downtime per target over `range`; targets with no overlap absent.
    async fn confirmed_downtime_by_target(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<HashMap<Uuid, i64>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

pub struct PgIncidentNarrationStore {
    pool: PgPool,
}

impl PgIncidentNarrationStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct IncidentRow {
    id: Uuid,
    target_id: Uuid,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    severity: String,
    status_at_start: String,
    check_count: i32,
    error_sample: Option<String>,
    public_title: Option<String>,
    public_description: Option<String>,
    duration_secs: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    regions_down: Option<Vec<String>>,
    regions_up: Option<Vec<String>>,
}

#[derive(sqlx::FromRow)]
struct UpdateRow {
    posted_at: DateTime<Utc>,
    phase: String,
    message: String,
}

async fn load_with_updates(pool: &PgPool, id: Uuid, org_id: Uuid) -> Result<Option<Incident>> {
    let Some(row): Option<IncidentRow> = sqlx::query_as(
        r#"SELECT id, target_id, started_at, ended_at, severity, status_at_start,
                  check_count, error_sample, public_title, public_description,
                  duration_secs, created_at, updated_at, regions_down, regions_up
           FROM incidents WHERE id = $1 AND org_id = $2"#,
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| anyhow::anyhow!("get incident: {e}"))?
    else {
        return Ok(None);
    };
    let updates: Vec<UpdateRow> = sqlx::query_as(
        r#"SELECT posted_at, phase, message
           FROM incident_updates
           WHERE incident_id = $1 AND org_id = $2
           ORDER BY posted_at ASC"#,
    )
    .bind(id)
    .bind(org_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow::anyhow!("get incident updates: {e}"))?;
    Ok(Some(row_to_incident(row, updates)))
}

fn row_to_incident(row: IncidentRow, updates: Vec<UpdateRow>) -> Incident {
    Incident {
        id: row.id,
        target_id: row.target_id,
        started_at: row.started_at,
        ended_at: row.ended_at,
        status: parse_status(&row.status_at_start),
        duration_secs: row.duration_secs.map(|d| d.max(0) as u64),
        check_count: row.check_count.max(0) as u64,
        error_sample: row.error_sample,
        severity: IncidentSeverity::from_db_str(&row.severity),
        public_title: row.public_title,
        public_description: row.public_description,
        created_at: Some(row.created_at),
        updated_at: Some(row.updated_at),
        updates: updates
            .into_iter()
            .map(|u| PublicIncidentUpdate {
                posted_at: u.posted_at,
                phase: IncidentStatusPhase::from_db_str(&u.phase),
                message: u.message,
            })
            .collect(),
        regions_down: row.regions_down.unwrap_or_default(),
        regions_up: row.regions_up.unwrap_or_default(),
    }
}

fn parse_status(s: &str) -> CheckStatus {
    match s {
        "down" => CheckStatus::Down,
        "degraded" => CheckStatus::Degraded,
        "error" => CheckStatus::Error,
        _ => CheckStatus::Down,
    }
}

#[async_trait]
impl IncidentNarrationStore for PgIncidentNarrationStore {
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<Incident>> {
        load_with_updates(&self.pool, id, org.0).await
    }

    async fn patch_narration(
        &self,
        org: OrgId,
        id: Uuid,
        update: IncidentNarrationUpdate,
    ) -> Result<Option<Incident>> {
        let severity = update.severity.map(|s| s.as_db_str());
        let row: Option<IncidentRow> = sqlx::query_as(
            r#"UPDATE incidents
               SET public_title       = CASE WHEN $2::bool THEN $3 ELSE public_title END,
                   public_description = CASE WHEN $4::bool THEN $5 ELSE public_description END,
                   severity           = COALESCE($6, severity),
                   updated_at         = now()
               WHERE id = $1 AND org_id = $7
               RETURNING id, target_id, started_at, ended_at, severity, status_at_start,
                         check_count, error_sample, public_title, public_description,
                         duration_secs, created_at, updated_at, regions_down, regions_up"#,
        )
        .bind(id)
        .bind(update.public_title.is_some())
        .bind(update.public_title.clone().flatten())
        .bind(update.public_description.is_some())
        .bind(update.public_description.clone().flatten())
        .bind(severity)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("patch_narration: {e}"))?;
        let Some(row) = row else { return Ok(None) };
        let updates: Vec<UpdateRow> = sqlx::query_as(
            r#"SELECT posted_at, phase, message
               FROM incident_updates
               WHERE incident_id = $1 AND org_id = $2
               ORDER BY posted_at ASC"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("get incident updates: {e}"))?;
        Ok(Some(row_to_incident(row, updates)))
    }

    async fn append_update(
        &self,
        org: OrgId,
        incident_id: Uuid,
        new: NewIncidentUpdate,
        author: Option<String>,
    ) -> Result<Option<PublicIncidentUpdate>> {
        let phase_db = new.phase.as_db_str();
        // Wrap the INSERT + parent UPDATE in a single transaction so the
        // displayed `updated_at` cannot fall out of sync with the appended
        // entry. CTE alternative would be one round-trip but obscures intent.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        // org_id is denormalised onto incident_updates and enforced by the
        // trg_incident_updates_org_match trigger. Filtering by the caller's
        // own org_id here means an attempt to append to another tenant's
        // incident yields a clean no-op instead of touching the parent row.
        let row: Option<(DateTime<Utc>, String, String)> = sqlx::query_as(
            r#"INSERT INTO incident_updates (org_id, incident_id, phase, message, author)
               SELECT i.org_id, $1, $2, $3, $4
               FROM incidents i
               WHERE i.id = $1 AND i.org_id = $5
               RETURNING posted_at, phase, message"#,
        )
        .bind(incident_id)
        .bind(phase_db)
        .bind(&new.message)
        .bind(author)
        .bind(org.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("append_update insert: {e}"))?;
        if row.is_some() {
            sqlx::query(r#"UPDATE incidents SET updated_at = now() WHERE id = $1 AND org_id = $2"#)
                .bind(incident_id)
                .bind(org.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| anyhow::anyhow!("append_update bump parent: {e}"))?;
        }
        tx.commit()
            .await
            .map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(row.map(|(posted_at, phase, message)| PublicIncidentUpdate {
            posted_at,
            phase: IncidentStatusPhase::from_db_str(&phase),
            message,
        }))
    }

    async fn list_active(&self, org: OrgId, limit: usize) -> Result<Vec<ActiveIncident>> {
        // LATERAL picks the latest update per incident in one round-trip. The
        // ceiling bounds a runaway query; callers that paginate (MCP) pass a
        // limit at least one page past what they'll surface so the page after
        // the cap is still reachable.
        let cap = limit.clamp(1, 1000) as i64;
        let rows: Vec<ActiveIncidentRow> = sqlx::query_as(
            r#"SELECT i.id, i.target_id,
                      t.name AS target_name,
                      i.severity, i.started_at, i.public_title,
                      u.posted_at AS update_posted_at,
                      u.phase     AS update_phase,
                      u.message   AS update_message
               FROM incidents i
               JOIN targets t ON t.id = i.target_id AND t.org_id = i.org_id
               LEFT JOIN LATERAL (
                   SELECT posted_at, phase, message
                   FROM incident_updates iu
                   WHERE iu.incident_id = i.id AND iu.org_id = i.org_id
                   ORDER BY iu.posted_at DESC
                   LIMIT 1
               ) u ON true
               WHERE i.org_id = $1 AND i.ended_at IS NULL
               ORDER BY i.started_at ASC
               LIMIT $2"#,
        )
        .bind(org.0)
        .bind(cap)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("list_active incidents: {e}"))?;
        Ok(rows.into_iter().map(row_to_active).collect())
    }

    async fn list_for_target(
        &self,
        org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
        offset: usize,
        ongoing_only: bool,
    ) -> Result<Vec<Incident>> {
        // Backstop only; callers pass their own page size (the API peeks one past
        // its limit, so the ceiling must clear the largest caller limit + 1).
        let cap = limit.clamp(1, 5000) as i64;
        let skip = offset as i64;
        let rows: Vec<IncidentRow> = sqlx::query_as(
            r#"SELECT id, target_id, started_at, ended_at, severity, status_at_start,
                      check_count, error_sample, public_title, public_description,
                      duration_secs, created_at, updated_at, regions_down, regions_up
               FROM incidents
               WHERE org_id = $1 AND target_id = $2
                 AND started_at < $4
                 AND (ended_at IS NULL OR ended_at >= $3)
                 AND ($5 = false OR ended_at IS NULL)
               ORDER BY started_at DESC, id DESC
               LIMIT $6 OFFSET $7"#,
        )
        .bind(org.0)
        .bind(target_id)
        .bind(range.from)
        .bind(range.to)
        .bind(ongoing_only)
        .bind(cap)
        .bind(skip)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("list_for_target incidents: {e}"))?;
        // History rows don't render the update timeline; skip the per-row join.
        Ok(rows
            .into_iter()
            .map(|r| row_to_incident(r, Vec::new()))
            .collect())
    }

    async fn confirmed_downtime_by_target(
        &self,
        org: OrgId,
        range: TimeRange,
    ) -> Result<HashMap<Uuid, i64>> {
        // FLOOR per incident to match the detail page's per-incident truncation.
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            r#"SELECT target_id,
                      SUM(GREATEST(0, FLOOR(EXTRACT(EPOCH FROM
                          (LEAST(COALESCE(ended_at, $2), $2) - GREATEST(started_at, $1))))))::bigint
               FROM incidents
               WHERE org_id = $3 AND target_id IS NOT NULL AND started_at < $2
                 AND (ended_at IS NULL OR ended_at >= $1)
               GROUP BY target_id"#,
        )
        .bind(range.from)
        .bind(range.to)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("confirmed_downtime_by_target: {e}"))?;
        Ok(rows.into_iter().collect())
    }
}

#[derive(sqlx::FromRow)]
struct ActiveIncidentRow {
    id: Uuid,
    target_id: Uuid,
    target_name: String,
    severity: String,
    started_at: DateTime<Utc>,
    public_title: Option<String>,
    update_posted_at: Option<DateTime<Utc>>,
    update_phase: Option<String>,
    update_message: Option<String>,
}

fn row_to_active(r: ActiveIncidentRow) -> ActiveIncident {
    let latest_update = match (r.update_posted_at, r.update_phase, r.update_message) {
        (Some(posted_at), Some(phase), Some(message)) => Some(PublicIncidentUpdate {
            posted_at,
            phase: IncidentStatusPhase::from_db_str(&phase),
            message,
        }),
        _ => None,
    };
    ActiveIncident {
        id: r.id,
        target_id: r.target_id,
        target_name: r.target_name,
        severity: IncidentSeverity::from_db_str(&r.severity),
        started_at: r.started_at,
        public_title: r.public_title,
        latest_update,
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryIncidentNarrationStore {
    inner: Mutex<InMemoryState>,
}

#[derive(Default)]
struct InMemoryState {
    incidents: Vec<Incident>,
}

impl InMemoryIncidentNarrationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&self, incident: Incident) {
        self.inner.lock().incidents.push(incident);
    }
}

#[async_trait]
impl IncidentNarrationStore for InMemoryIncidentNarrationStore {
    async fn get(&self, _org: OrgId, id: Uuid) -> Result<Option<Incident>> {
        Ok(self
            .inner
            .lock()
            .incidents
            .iter()
            .find(|i| i.id == id)
            .cloned())
    }

    async fn patch_narration(
        &self,
        _org: OrgId,
        id: Uuid,
        update: IncidentNarrationUpdate,
    ) -> Result<Option<Incident>> {
        let mut g = self.inner.lock();
        let Some(inc) = g.incidents.iter_mut().find(|i| i.id == id) else {
            return Ok(None);
        };
        if let Some(t) = update.public_title {
            inc.public_title = t;
        }
        if let Some(d) = update.public_description {
            inc.public_description = d;
        }
        if let Some(s) = update.severity {
            inc.severity = s;
        }
        inc.updated_at = Some(Utc::now());
        Ok(Some(inc.clone()))
    }

    async fn append_update(
        &self,
        _org: OrgId,
        incident_id: Uuid,
        new: NewIncidentUpdate,
        _author: Option<String>,
    ) -> Result<Option<PublicIncidentUpdate>> {
        let mut g = self.inner.lock();
        let Some(inc) = g.incidents.iter_mut().find(|i| i.id == incident_id) else {
            return Ok(None);
        };
        let entry = PublicIncidentUpdate {
            posted_at: Utc::now(),
            phase: new.phase,
            message: new.message,
        };
        inc.updates.push(entry.clone());
        inc.updated_at = Some(entry.posted_at);
        Ok(Some(entry))
    }

    async fn list_active(&self, _org: OrgId, limit: usize) -> Result<Vec<ActiveIncident>> {
        // In-memory store doesn't track targets; tests that need a real
        // `target_name` use the Postgres-backed store via the harness.
        let mut out: Vec<ActiveIncident> = self
            .inner
            .lock()
            .incidents
            .iter()
            .filter(|i| i.ended_at.is_none())
            .map(|i| ActiveIncident {
                id: i.id,
                target_id: i.target_id,
                target_name: String::new(),
                severity: i.severity,
                started_at: i.started_at,
                public_title: i.public_title.clone(),
                latest_update: i.updates.last().cloned(),
            })
            .collect();
        out.sort_by_key(|a| a.started_at);
        out.truncate(limit);
        Ok(out)
    }

    async fn list_for_target(
        &self,
        _org: OrgId,
        target_id: Uuid,
        range: TimeRange,
        limit: usize,
        offset: usize,
        ongoing_only: bool,
    ) -> Result<Vec<Incident>> {
        let mut out: Vec<Incident> = self
            .inner
            .lock()
            .incidents
            .iter()
            .filter(|i| i.target_id == target_id)
            .filter(|i| i.started_at < range.to && i.ended_at.is_none_or(|e| e >= range.from))
            .filter(|i| !ongoing_only || i.ended_at.is_none())
            .cloned()
            .collect();
        out.sort_by_key(|i| (std::cmp::Reverse(i.started_at), std::cmp::Reverse(i.id)));
        Ok(out.into_iter().skip(offset).take(limit).collect())
    }

    async fn confirmed_downtime_by_target(
        &self,
        _org: OrgId,
        range: TimeRange,
    ) -> Result<HashMap<Uuid, i64>> {
        let mut out: HashMap<Uuid, i64> = HashMap::new();
        for i in self.inner.lock().incidents.iter() {
            if i.started_at < range.to && i.ended_at.is_none_or(|e| e >= range.from) {
                let end = i.ended_at.unwrap_or(range.to).min(range.to);
                let start = i.started_at.max(range.from);
                *out.entry(i.target_id).or_default() += (end - start).num_seconds().max(0);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn sample() -> Incident {
        Incident {
            id: Uuid::now_v7(),
            target_id: Uuid::now_v7(),
            started_at: Utc::now() - ChronoDuration::minutes(15),
            ended_at: None,
            status: CheckStatus::Down,
            duration_secs: None,
            check_count: 3,
            error_sample: Some("timeout".into()),
            severity: IncidentSeverity::Major,
            public_title: None,
            public_description: None,
            created_at: Some(Utc::now() - ChronoDuration::minutes(15)),
            updated_at: Some(Utc::now() - ChronoDuration::minutes(15)),
            updates: vec![],
            regions_down: Vec::new(),
            regions_up: Vec::new(),
        }
    }

    fn org() -> OrgId {
        OrgId(Uuid::nil())
    }

    #[tokio::test]
    async fn patch_narration_overwrites_fields() {
        let store = InMemoryIncidentNarrationStore::new();
        let inc = sample();
        let id = inc.id;
        store.seed(inc);
        let patched = store
            .patch_narration(
                org(),
                id,
                IncidentNarrationUpdate {
                    public_title: Some(Some("Latency spike".into())),
                    public_description: Some(Some("EU".into())),
                    severity: Some(IncidentSeverity::Critical),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(patched.public_title.as_deref(), Some("Latency spike"));
        assert_eq!(patched.public_description.as_deref(), Some("EU"));
        assert_eq!(patched.severity, IncidentSeverity::Critical);
    }

    #[tokio::test]
    async fn patch_narration_with_none_clears_fields() {
        let store = InMemoryIncidentNarrationStore::new();
        let mut inc = sample();
        inc.public_title = Some("old".into());
        let id = inc.id;
        store.seed(inc);
        let patched = store
            .patch_narration(
                org(),
                id,
                IncidentNarrationUpdate {
                    public_title: Some(None),
                    public_description: None,
                    severity: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(patched.public_title, None);
    }

    #[tokio::test]
    async fn append_update_records_entry() {
        let store = InMemoryIncidentNarrationStore::new();
        let inc = sample();
        let id = inc.id;
        store.seed(inc);
        let entry = store
            .append_update(
                org(),
                id,
                NewIncidentUpdate {
                    phase: IncidentStatusPhase::Identified,
                    message: "rolling back".into(),
                },
                Some("op@example.com".into()),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.phase, IncidentStatusPhase::Identified);
        let again = store.get(org(), id).await.unwrap().unwrap();
        assert_eq!(again.updates.len(), 1);
    }

    #[tokio::test]
    async fn append_update_for_missing_incident_returns_none() {
        let store = InMemoryIncidentNarrationStore::new();
        let res = store
            .append_update(
                org(),
                Uuid::now_v7(),
                NewIncidentUpdate {
                    phase: IncidentStatusPhase::Investigating,
                    message: "x".into(),
                },
                None,
            )
            .await
            .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn list_for_target_filters_by_window_ongoing_and_offset() {
        let store = InMemoryIncidentNarrationStore::new();
        let target = Uuid::now_v7();
        let other = Uuid::now_v7();
        let now = Utc::now();
        let mk = |tid: Uuid, started_min_ago: i64, ended_min_ago: Option<i64>| {
            let mut inc = sample();
            inc.id = Uuid::now_v7();
            inc.target_id = tid;
            inc.started_at = now - ChronoDuration::minutes(started_min_ago);
            inc.ended_at = ended_min_ago.map(|m| now - ChronoDuration::minutes(m));
            inc
        };
        store.seed(mk(target, 30, Some(20))); // closed, inside window
        store.seed(mk(target, 10, None)); // ongoing
        store.seed(mk(target, 60 * 48, Some(60 * 47))); // fully before window
        store.seed(mk(other, 5, None)); // different target

        let window = TimeRange {
            from: now - ChronoDuration::hours(24),
            to: now,
        };
        let all = store
            .list_for_target(org(), target, window, 50, 0, false)
            .await
            .unwrap();
        assert_eq!(all.len(), 2); // the out-of-window and other-target rows excluded
        assert!(all[0].started_at >= all[1].started_at); // newest first

        let ongoing = store
            .list_for_target(org(), target, window, 50, 0, true)
            .await
            .unwrap();
        assert_eq!(ongoing.len(), 1);
        assert!(ongoing[0].ended_at.is_none());

        let page2 = store
            .list_for_target(org(), target, window, 1, 1, false)
            .await
            .unwrap();
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].started_at, all[1].started_at); // offset skipped the newest
    }

    #[tokio::test]
    async fn confirmed_downtime_by_target_sums_clamped_overlap() {
        let store = InMemoryIncidentNarrationStore::new();
        let (t1, t2, t3) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let now = Utc::now();
        let mk = |tid: Uuid, start_min: i64, end_min: Option<i64>| {
            let mut i = sample();
            i.id = Uuid::now_v7();
            i.target_id = tid;
            i.started_at = now - ChronoDuration::minutes(start_min);
            i.ended_at = end_min.map(|m| now - ChronoDuration::minutes(m));
            i
        };
        store.seed(mk(t1, 50, Some(40))); // 600s
        store.seed(mk(t1, 20, None)); // ongoing -> 1200s
        store.seed(mk(t2, 90, Some(50))); // clamped to window -> 600s
        store.seed(mk(t3, 120, Some(110))); // before window -> excluded

        let window = TimeRange {
            from: now - ChronoDuration::hours(1),
            to: now,
        };
        let map = store
            .confirmed_downtime_by_target(org(), window)
            .await
            .unwrap();
        assert_eq!(map.get(&t1).copied(), Some(1800));
        assert_eq!(map.get(&t2).copied(), Some(600));
        assert_eq!(map.get(&t3), None);
    }
}
