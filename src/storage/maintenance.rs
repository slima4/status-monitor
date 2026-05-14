//! Storage layer for `maintenance_windows` and `maintenance_window_components`.
//!
//! Operator-side CRUD: the public aggregator reads its own filtered slice from
//! the same tables, but never goes through this trait so its hot path stays
//! independent.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{
    MaintenanceFilter, MaintenanceWindow, MaintenanceWindowUpdate, NewMaintenanceWindow, OrgId,
};
use crate::error::Result;

#[derive(Debug, Clone, Copy)]
pub struct MaintenancePage {
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Default, Clone)]
pub struct MaintenanceListQuery {
    pub filter: MaintenanceFilter,
    pub limit: u32,
    pub offset: u32,
}

#[async_trait]
pub trait MaintenanceStore: Send + Sync {
    async fn create(&self, new: NewMaintenanceWindow) -> Result<MaintenanceWindow>;
    async fn list(&self, q: MaintenanceListQuery) -> Result<Vec<MaintenanceWindow>>;
    async fn count(&self, filter: MaintenanceFilter) -> Result<u64>;
    async fn get(&self, id: Uuid) -> Result<Option<MaintenanceWindow>>;
    async fn update(
        &self,
        id: Uuid,
        update: MaintenanceWindowUpdate,
    ) -> Result<Option<MaintenanceWindow>>;
    async fn delete(&self, id: Uuid) -> Result<bool>;
    /// Subset of `ids` that exist in `targets`. Used to validate
    /// `component_ids` on create/update without requiring callers to plumb in
    /// a `TargetStore`.
    async fn existing_target_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

pub struct PgMaintenanceStore {
    pool: PgPool,
    default_org_id: OrgId,
}

impl PgMaintenanceStore {
    pub fn new(pool: PgPool, default_org_id: OrgId) -> Self {
        Self { pool, default_org_id }
    }
}

#[derive(sqlx::FromRow)]
struct MaintenanceRow {
    id: Uuid,
    title: String,
    description: Option<String>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl MaintenanceRow {
    fn into_window(self, component_ids: Vec<Uuid>) -> MaintenanceWindow {
        MaintenanceWindow {
            id: self.id,
            title: self.title,
            description: self.description,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            component_ids,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

async fn load_components(pool: &PgPool, maintenance_id: Uuid) -> Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        r#"SELECT target_id FROM maintenance_window_components
           WHERE maintenance_id = $1 ORDER BY target_id"#,
    )
    .bind(maintenance_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow::anyhow!("load_components: {e}"))?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

#[async_trait]
impl MaintenanceStore for PgMaintenanceStore {
    async fn create(&self, new: NewMaintenanceWindow) -> Result<MaintenanceWindow> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        let row: MaintenanceRow = sqlx::query_as(
            r#"INSERT INTO maintenance_windows
                   (org_id, title, description, starts_at, ends_at)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING id, title, description, starts_at, ends_at, created_at, updated_at"#,
        )
        .bind(self.default_org_id.0)
        .bind(&new.title)
        .bind(&new.description)
        .bind(new.starts_at)
        .bind(new.ends_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("insert maintenance: {e}"))?;
        if !new.component_ids.is_empty() {
            // Pull org_id from the parent so it stays in lockstep even if a
            // future caller passes a wrong default_org_id; the trigger is the
            // belt, this is the suspenders.
            sqlx::query(
                r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
                   SELECT mw.org_id, mw.id, t.target_id
                   FROM maintenance_windows mw
                   CROSS JOIN UNNEST($2::uuid[]) AS t(target_id)
                   WHERE mw.id = $1"#,
            )
            .bind(row.id)
            .bind(&new.component_ids)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("insert components: {e}"))?;
        }
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(row.into_window(new.component_ids))
    }

    async fn list(&self, q: MaintenanceListQuery) -> Result<Vec<MaintenanceWindow>> {
        let now = Utc::now();
        let rows: Vec<MaintenanceRow> = sqlx::query_as(&list_sql(q.filter))
            .bind(now)
            .bind(q.limit as i64)
            .bind(q.offset as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("list maintenance: {e}"))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let components = load_components(&self.pool, row.id).await?;
            out.push(row.into_window(components));
        }
        Ok(out)
    }

    async fn count(&self, filter: MaintenanceFilter) -> Result<u64> {
        let now = Utc::now();
        let (total,): (i64,) = sqlx::query_as(&count_sql(filter))
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("count maintenance: {e}"))?;
        Ok(total.max(0) as u64)
    }

    async fn get(&self, id: Uuid) -> Result<Option<MaintenanceWindow>> {
        let row: Option<MaintenanceRow> = sqlx::query_as(
            r#"SELECT id, title, description, starts_at, ends_at, created_at, updated_at
               FROM maintenance_windows WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("get maintenance: {e}"))?;
        match row {
            Some(r) => {
                let components = load_components(&self.pool, r.id).await?;
                Ok(Some(r.into_window(components)))
            }
            None => Ok(None),
        }
    }

    async fn update(
        &self,
        id: Uuid,
        update: MaintenanceWindowUpdate,
    ) -> Result<Option<MaintenanceWindow>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        // `description` uses single-Option semantics (per the addendum wire type):
        // `Some("…")` overwrites, `None` leaves the stored value untouched.
        // To distinguish leave-alone from clear-to-null, the field would have
        // to be `Option<Option<String>>` like `IncidentNarrationUpdate`.
        let row: Option<MaintenanceRow> = sqlx::query_as(
            r#"UPDATE maintenance_windows
               SET title       = COALESCE($2, title),
                   description = COALESCE($3, description),
                   starts_at   = COALESCE($4, starts_at),
                   ends_at     = COALESCE($5, ends_at),
                   updated_at  = now()
               WHERE id = $1
               RETURNING id, title, description, starts_at, ends_at, created_at, updated_at"#,
        )
        .bind(id)
        .bind(update.title.as_ref())
        .bind(update.description.clone())
        .bind(update.starts_at)
        .bind(update.ends_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| anyhow::anyhow!("update maintenance: {e}"))?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        if let Some(ids) = update.component_ids.as_ref() {
            sqlx::query(r#"DELETE FROM maintenance_window_components WHERE maintenance_id = $1"#)
                .bind(row.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| anyhow::anyhow!("delete components: {e}"))?;
            if !ids.is_empty() {
                sqlx::query(
                    r#"INSERT INTO maintenance_window_components (org_id, maintenance_id, target_id)
                       SELECT mw.org_id, mw.id, t.target_id
                       FROM maintenance_windows mw
                       CROSS JOIN UNNEST($2::uuid[]) AS t(target_id)
                       WHERE mw.id = $1"#,
                )
                .bind(row.id)
                .bind(ids)
                .execute(&mut *tx)
                .await
                .map_err(|e| anyhow::anyhow!("insert components: {e}"))?;
            }
        }
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        let components = load_components(&self.pool, row.id).await?;
        Ok(Some(row.into_window(components)))
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let result = sqlx::query(r#"DELETE FROM maintenance_windows WHERE id = $1"#)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("delete maintenance: {e}"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn existing_target_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid,)> =
            sqlx::query_as(r#"SELECT id FROM targets WHERE id = ANY($1::uuid[])"#)
                .bind(ids)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("existing_target_ids: {e}"))?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }
}

fn list_sql(filter: MaintenanceFilter) -> String {
    format!(
        r#"SELECT id, title, description, starts_at, ends_at, created_at, updated_at
           FROM maintenance_windows
           WHERE {clause}
           ORDER BY starts_at DESC
           LIMIT $2 OFFSET $3"#,
        clause = filter_clause(filter),
    )
}

fn count_sql(filter: MaintenanceFilter) -> String {
    format!(
        r#"SELECT count(*) FROM maintenance_windows WHERE {clause}"#,
        clause = filter_clause(filter),
    )
}

fn filter_clause(filter: MaintenanceFilter) -> &'static str {
    match filter {
        MaintenanceFilter::Active => "starts_at <= $1 AND ends_at > $1",
        MaintenanceFilter::Upcoming => "starts_at > $1",
        MaintenanceFilter::Past => "ends_at <= $1",
        MaintenanceFilter::All => "$1 IS NOT NULL OR $1 IS NULL",
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryMaintenanceStore {
    inner: Mutex<InMemoryState>,
}

#[derive(Default)]
struct InMemoryState {
    windows: Vec<MaintenanceWindow>,
    known_targets: std::collections::HashSet<Uuid>,
}

impl InMemoryMaintenanceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate the set of target ids that `existing_target_ids` will
    /// consider valid. Tests use this to drive `INVALID_COMPONENT_ID` cases
    /// without spinning up a target store.
    pub fn with_targets(targets: impl IntoIterator<Item = Uuid>) -> Self {
        let mut state = InMemoryState::default();
        state.known_targets.extend(targets);
        Self {
            inner: Mutex::new(state),
        }
    }

    pub fn register_target(&self, id: Uuid) {
        self.inner.lock().known_targets.insert(id);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().windows.is_empty()
    }
}

#[async_trait]
impl MaintenanceStore for InMemoryMaintenanceStore {
    async fn create(&self, new: NewMaintenanceWindow) -> Result<MaintenanceWindow> {
        let now = Utc::now();
        let id = Uuid::now_v7();
        let mw = MaintenanceWindow {
            id,
            title: new.title,
            description: new.description,
            starts_at: new.starts_at,
            ends_at: new.ends_at,
            component_ids: new.component_ids,
            created_at: now,
            updated_at: now,
        };
        self.inner.lock().windows.push(mw.clone());
        Ok(mw)
    }

    async fn list(&self, q: MaintenanceListQuery) -> Result<Vec<MaintenanceWindow>> {
        let now = Utc::now();
        let g = self.inner.lock();
        let mut filtered: Vec<MaintenanceWindow> = g
            .windows
            .iter()
            .filter(|w| match_filter(w, q.filter, now))
            .cloned()
            .collect();
        filtered.sort_by_key(|w| std::cmp::Reverse(w.starts_at));
        let start = q.offset as usize;
        let end = (start + q.limit as usize).min(filtered.len());
        if start >= filtered.len() {
            return Ok(Vec::new());
        }
        Ok(filtered[start..end].to_vec())
    }

    async fn count(&self, filter: MaintenanceFilter) -> Result<u64> {
        let now = Utc::now();
        let g = self.inner.lock();
        Ok(g.windows.iter().filter(|w| match_filter(w, filter, now)).count() as u64)
    }

    async fn get(&self, id: Uuid) -> Result<Option<MaintenanceWindow>> {
        Ok(self.inner.lock().windows.iter().find(|w| w.id == id).cloned())
    }

    async fn update(
        &self,
        id: Uuid,
        update: MaintenanceWindowUpdate,
    ) -> Result<Option<MaintenanceWindow>> {
        let mut g = self.inner.lock();
        let Some(w) = g.windows.iter_mut().find(|w| w.id == id) else {
            return Ok(None);
        };
        if let Some(t) = update.title {
            w.title = t;
        }
        if let Some(d) = update.description {
            w.description = Some(d);
        }
        if let Some(s) = update.starts_at {
            w.starts_at = s;
        }
        if let Some(e) = update.ends_at {
            w.ends_at = e;
        }
        if let Some(c) = update.component_ids {
            w.component_ids = c;
        }
        w.updated_at = Utc::now();
        Ok(Some(w.clone()))
    }

    async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut g = self.inner.lock();
        let before = g.windows.len();
        g.windows.retain(|w| w.id != id);
        Ok(g.windows.len() < before)
    }

    async fn existing_target_ids(&self, ids: &[Uuid]) -> Result<Vec<Uuid>> {
        let g = self.inner.lock();
        Ok(ids
            .iter()
            .filter(|id| g.known_targets.contains(id))
            .copied()
            .collect())
    }
}

fn match_filter(w: &MaintenanceWindow, filter: MaintenanceFilter, now: DateTime<Utc>) -> bool {
    match filter {
        MaintenanceFilter::Active => w.starts_at <= now && w.ends_at > now,
        MaintenanceFilter::Upcoming => w.starts_at > now,
        MaintenanceFilter::Past => w.ends_at <= now,
        MaintenanceFilter::All => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn upcoming() -> NewMaintenanceWindow {
        NewMaintenanceWindow {
            title: "Upgrade".into(),
            description: None,
            starts_at: Utc::now() + ChronoDuration::hours(1),
            ends_at: Utc::now() + ChronoDuration::hours(2),
            component_ids: vec![],
        }
    }

    #[tokio::test]
    async fn create_and_list_roundtrip() {
        let store = InMemoryMaintenanceStore::new();
        let mw = store.create(upcoming()).await.unwrap();
        let list = store
            .list(MaintenanceListQuery {
                filter: MaintenanceFilter::All,
                limit: 10,
                offset: 0,
            })
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, mw.id);
    }

    #[tokio::test]
    async fn filter_active_excludes_upcoming() {
        let store = InMemoryMaintenanceStore::new();
        store.create(upcoming()).await.unwrap();
        let active = store
            .list(MaintenanceListQuery {
                filter: MaintenanceFilter::Active,
                limit: 10,
                offset: 0,
            })
            .await
            .unwrap();
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn update_replaces_components() {
        let store = InMemoryMaintenanceStore::new();
        let mw = store.create(upcoming()).await.unwrap();
        let new_id = Uuid::now_v7();
        let patched = store
            .update(
                mw.id,
                MaintenanceWindowUpdate {
                    component_ids: Some(vec![new_id]),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(patched.component_ids, vec![new_id]);
    }

    #[tokio::test]
    async fn existing_target_ids_filters_unknown() {
        let known = Uuid::now_v7();
        let store = InMemoryMaintenanceStore::with_targets([known]);
        let unknown = Uuid::now_v7();
        let got = store.existing_target_ids(&[known, unknown]).await.unwrap();
        assert_eq!(got, vec![known]);
    }

    #[tokio::test]
    async fn delete_returns_false_for_unknown() {
        let store = InMemoryMaintenanceStore::new();
        assert!(!store.delete(Uuid::now_v7()).await.unwrap());
    }
}
