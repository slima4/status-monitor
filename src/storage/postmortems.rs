//! Incident postmortem storage: one retrospective document per incident.
//!
//! Org-scoped like every other store — each method takes the caller's `org`
//! and the Postgres statements filter `org_id`, so a caller cannot reach
//! another tenant's postmortem. The upsert is guarded on the parent incident
//! existing in `org`, so a postmortem can never be attached to a foreign or
//! missing incident.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{IncidentPostmortem, OrgId, PostmortemUpsert, UserId};
use crate::error::Result;

#[async_trait]
pub trait PostmortemStore: Send + Sync {
    async fn get(&self, org: OrgId, incident_id: Uuid) -> Result<Option<IncidentPostmortem>>;
    /// Create or replace the postmortem for an incident. `None` ⇒ the incident
    /// does not exist in `org`. The author is recorded on first save and kept
    /// across later edits.
    async fn upsert(
        &self,
        org: OrgId,
        incident_id: Uuid,
        author: UserId,
        fields: PostmortemUpsert,
    ) -> Result<Option<IncidentPostmortem>>;
    /// Toggle the published flag. `None` ⇒ no postmortem for that incident.
    async fn set_published(
        &self,
        org: OrgId,
        incident_id: Uuid,
        publish: bool,
    ) -> Result<Option<IncidentPostmortem>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

pub struct PgPostmortemStore {
    pool: PgPool,
}

impl PgPostmortemStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const PM_COLS: &str = "incident_id, summary, root_cause, impact, action_items, author_id, \
     created_at, updated_at, published_at";

#[derive(sqlx::FromRow)]
struct PostmortemRow {
    incident_id: Uuid,
    summary: Option<String>,
    root_cause: Option<String>,
    impact: Option<String>,
    action_items: Value,
    author_id: Option<UserId>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    published_at: Option<DateTime<Utc>>,
}

fn row_to_postmortem(r: PostmortemRow) -> IncidentPostmortem {
    IncidentPostmortem {
        incident_id: r.incident_id,
        summary: r.summary,
        root_cause: r.root_cause,
        impact: r.impact,
        // A malformed stored blob degrades to an empty list rather than failing
        // the whole read; the column is only ever written from typed values.
        action_items: serde_json::from_value(r.action_items).unwrap_or_default(),
        author_id: r.author_id,
        created_at: r.created_at,
        updated_at: r.updated_at,
        published_at: r.published_at,
    }
}

#[async_trait]
impl PostmortemStore for PgPostmortemStore {
    async fn get(&self, org: OrgId, incident_id: Uuid) -> Result<Option<IncidentPostmortem>> {
        let sql = format!(
            "SELECT {PM_COLS} FROM incident_postmortems \
             WHERE incident_id = $1 AND org_id = $2"
        );
        let row: Option<PostmortemRow> = sqlx::query_as(&sql)
            .bind(incident_id)
            .bind(org.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("get postmortem: {e}"))?;
        Ok(row.map(row_to_postmortem))
    }

    async fn upsert(
        &self,
        org: OrgId,
        incident_id: Uuid,
        author: UserId,
        fields: PostmortemUpsert,
    ) -> Result<Option<IncidentPostmortem>> {
        let items = serde_json::to_value(&fields.action_items)
            .map_err(|e| anyhow::anyhow!("encode action_items: {e}"))?;
        // The SELECT-from-incidents guard means a postmortem can only be created
        // for an incident the caller owns; a foreign/missing id inserts nothing.
        let sql = format!(
            "INSERT INTO incident_postmortems \
                (org_id, incident_id, summary, root_cause, impact, action_items, author_id) \
             SELECT i.org_id, i.id, $3, $4, $5, $6, $7 \
             FROM incidents i WHERE i.id = $2 AND i.org_id = $1 \
             ON CONFLICT (incident_id) DO UPDATE \
             SET summary = EXCLUDED.summary, root_cause = EXCLUDED.root_cause, \
                 impact = EXCLUDED.impact, action_items = EXCLUDED.action_items, \
                 updated_at = now() \
             RETURNING {PM_COLS}"
        );
        let row: Option<PostmortemRow> = sqlx::query_as(&sql)
            .bind(org.0)
            .bind(incident_id)
            .bind(fields.summary)
            .bind(fields.root_cause)
            .bind(fields.impact)
            .bind(items)
            .bind(author)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("upsert postmortem: {e}"))?;
        Ok(row.map(row_to_postmortem))
    }

    async fn set_published(
        &self,
        org: OrgId,
        incident_id: Uuid,
        publish: bool,
    ) -> Result<Option<IncidentPostmortem>> {
        let sql = format!(
            "UPDATE incident_postmortems \
             SET published_at = CASE WHEN $3 THEN COALESCE(published_at, now()) ELSE NULL END, \
                 updated_at = now() \
             WHERE incident_id = $1 AND org_id = $2 RETURNING {PM_COLS}"
        );
        let row: Option<PostmortemRow> = sqlx::query_as(&sql)
            .bind(incident_id)
            .bind(org.0)
            .bind(publish)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("set postmortem published: {e}"))?;
        Ok(row.map(row_to_postmortem))
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryPostmortemStore {
    inner: Mutex<Vec<(OrgId, IncidentPostmortem)>>,
    // Incidents the caller is allowed to attach to; mirrors the PG org guard.
    known: Mutex<Vec<(OrgId, Uuid)>>,
}

impl InMemoryPostmortemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an incident so `upsert` accepts it (PG enforces this via the
    /// incidents FK + org filter).
    pub fn seed_incident(&self, org: OrgId, incident_id: Uuid) {
        self.known.lock().push((org, incident_id));
    }
}

#[async_trait]
impl PostmortemStore for InMemoryPostmortemStore {
    async fn get(&self, org: OrgId, incident_id: Uuid) -> Result<Option<IncidentPostmortem>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .find(|(o, p)| *o == org && p.incident_id == incident_id)
            .map(|(_, p)| p.clone()))
    }

    async fn upsert(
        &self,
        org: OrgId,
        incident_id: Uuid,
        author: UserId,
        fields: PostmortemUpsert,
    ) -> Result<Option<IncidentPostmortem>> {
        if !self
            .known
            .lock()
            .iter()
            .any(|(o, id)| *o == org && *id == incident_id)
        {
            return Ok(None);
        }
        let mut g = self.inner.lock();
        let now = Utc::now();
        if let Some((_, p)) = g
            .iter_mut()
            .find(|(o, p)| *o == org && p.incident_id == incident_id)
        {
            p.summary = fields.summary;
            p.root_cause = fields.root_cause;
            p.impact = fields.impact;
            p.action_items = fields.action_items;
            p.updated_at = now;
            return Ok(Some(p.clone()));
        }
        let pm = IncidentPostmortem {
            incident_id,
            summary: fields.summary,
            root_cause: fields.root_cause,
            impact: fields.impact,
            action_items: fields.action_items,
            author_id: Some(author),
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        g.push((org, pm.clone()));
        Ok(Some(pm))
    }

    async fn set_published(
        &self,
        org: OrgId,
        incident_id: Uuid,
        publish: bool,
    ) -> Result<Option<IncidentPostmortem>> {
        let mut g = self.inner.lock();
        let Some((_, p)) = g
            .iter_mut()
            .find(|(o, p)| *o == org && p.incident_id == incident_id)
        else {
            return Ok(None);
        };
        p.published_at = if publish {
            p.published_at.or_else(|| Some(Utc::now()))
        } else {
            None
        };
        p.updated_at = Utc::now();
        Ok(Some(p.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ActionItem;

    fn org() -> OrgId {
        OrgId(Uuid::nil())
    }

    #[tokio::test]
    async fn upsert_requires_known_incident() {
        let store = InMemoryPostmortemStore::new();
        let id = Uuid::now_v7();
        assert!(
            store
                .upsert(
                    org(),
                    id,
                    UserId(Uuid::now_v7()),
                    PostmortemUpsert::default()
                )
                .await
                .unwrap()
                .is_none(),
            "unknown incident yields None"
        );
        store.seed_incident(org(), id);
        let pm = store
            .upsert(
                org(),
                id,
                UserId(Uuid::now_v7()),
                PostmortemUpsert {
                    summary: Some("root caused".into()),
                    action_items: vec![ActionItem {
                        text: "add alert".into(),
                        owner_user_id: None,
                        done: false,
                    }],
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pm.summary.as_deref(), Some("root caused"));
        assert_eq!(pm.action_items.len(), 1);
        assert!(pm.published_at.is_none());
    }

    #[tokio::test]
    async fn publish_then_unpublish_toggles() {
        let store = InMemoryPostmortemStore::new();
        let id = Uuid::now_v7();
        store.seed_incident(org(), id);
        store
            .upsert(
                org(),
                id,
                UserId(Uuid::now_v7()),
                PostmortemUpsert::default(),
            )
            .await
            .unwrap();
        let p = store.set_published(org(), id, true).await.unwrap().unwrap();
        assert!(p.published_at.is_some());
        let p = store
            .set_published(org(), id, false)
            .await
            .unwrap()
            .unwrap();
        assert!(p.published_at.is_none());
    }

    #[tokio::test]
    async fn set_published_on_missing_is_none() {
        let store = InMemoryPostmortemStore::new();
        assert!(
            store
                .set_published(org(), Uuid::now_v7(), true)
                .await
                .unwrap()
                .is_none()
        );
    }
}
