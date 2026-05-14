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
use crate::error::Result;

#[async_trait]
pub trait IncidentNarrationStore: Send + Sync {
    async fn get(&self, id: Uuid) -> Result<Option<Incident>>;
    async fn patch_narration(
        &self,
        id: Uuid,
        update: IncidentNarrationUpdate,
    ) -> Result<Option<Incident>>;
    async fn append_update(
        &self,
        incident_id: Uuid,
        new: NewIncidentUpdate,
        author: Option<String>,
    ) -> Result<Option<PublicIncidentUpdate>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

/// Org-scoped operator-side incident store. Every query binds
/// `self.default_org_id` so an operator on one tenant cannot read or mutate
/// incidents owned by another.
pub struct PgIncidentNarrationStore {
    pool: PgPool,
    default_org_id: OrgId,
}

impl PgIncidentNarrationStore {
    pub fn new(pool: PgPool, default_org_id: OrgId) -> Self {
        Self {
            pool,
            default_org_id,
        }
    }

    fn org_id(&self) -> Uuid {
        self.default_org_id.0
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
                  duration_secs, created_at, updated_at
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
    async fn get(&self, id: Uuid) -> Result<Option<Incident>> {
        load_with_updates(&self.pool, id, self.org_id()).await
    }

    async fn patch_narration(
        &self,
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
                         duration_secs, created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.public_title.is_some())
        .bind(update.public_title.clone().flatten())
        .bind(update.public_description.is_some())
        .bind(update.public_description.clone().flatten())
        .bind(severity)
        .bind(self.org_id())
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
        .bind(self.org_id())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("get incident updates: {e}"))?;
        Ok(Some(row_to_incident(row, updates)))
    }

    async fn append_update(
        &self,
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
        // trg_incident_updates_org_match trigger. Filtering by the operator's
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
        .bind(self.org_id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("append_update insert: {e}"))?;
        if row.is_some() {
            sqlx::query(r#"UPDATE incidents SET updated_at = now() WHERE id = $1 AND org_id = $2"#)
                .bind(incident_id)
                .bind(self.org_id())
                .execute(&mut *tx)
                .await
                .map_err(|e| anyhow::anyhow!("append_update bump parent: {e}"))?;
        }
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(row.map(|(posted_at, phase, message)| PublicIncidentUpdate {
            posted_at,
            phase: IncidentStatusPhase::from_db_str(&phase),
            message,
        }))
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
    async fn get(&self, id: Uuid) -> Result<Option<Incident>> {
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
        }
    }

    #[tokio::test]
    async fn patch_narration_overwrites_fields() {
        let store = InMemoryIncidentNarrationStore::new();
        let inc = sample();
        let id = inc.id;
        store.seed(inc);
        let patched = store
            .patch_narration(
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
        let again = store.get(id).await.unwrap().unwrap();
        assert_eq!(again.updates.len(), 1);
    }

    #[tokio::test]
    async fn append_update_for_missing_incident_returns_none() {
        let store = InMemoryIncidentNarrationStore::new();
        let res = store
            .append_update(
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
}
