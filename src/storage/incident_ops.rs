//! Operational incident lifecycle storage.
//!
//! Separate from [`crate::storage::incidents`] (public narration) and from
//! `public_status::incident_writer::IncidentStore` (the auto open/close
//! materialiser): this trait owns the *internal* operational surface — the
//! state machine (acknowledge / assign / resolve / reopen), the internal
//! activity log, and the read model that backs the operator console.
//!
//! Every method takes the caller's `org`. The Postgres store filters `org_id`
//! in every statement, so a caller cannot reach another tenant's rows; the
//! in-memory store is a single-tenant test double and matches on id alone.
//! Postgres state transitions run under a per-incident advisory lock so
//! concurrent ack/resolve/escalate cannot race the machine.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{
    ActorType, IncidentEvent, IncidentEventKind, IncidentOrigin, IncidentSeverity, IncidentState,
    IncidentTransition, IncidentUrgency, IncidentVisibility, NewManualIncident, OpsIncident, OrgId,
    TransitionError, UserId, next_state,
};
use crate::error::Result;
use crate::storage::locks::{advisory_xact_lock, incident_lock_key};

/// Who is performing an action. Maps onto `incident_events.actor_type` +
/// `actor_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    System,
    User(UserId),
    Mcp(UserId),
}

impl Actor {
    pub fn actor_type(self) -> ActorType {
        match self {
            Self::System => ActorType::System,
            Self::User(_) => ActorType::User,
            Self::Mcp(_) => ActorType::Mcp,
        }
    }
    pub fn user_id(self) -> Option<UserId> {
        match self {
            Self::System => None,
            Self::User(u) | Self::Mcp(u) => Some(u),
        }
    }
}

/// Result of a lifecycle mutation: distinguishes a missing incident from an
/// illegal transition (which the API layer maps to 409, not 404).
#[derive(Debug, Clone)]
pub enum LifecycleOutcome {
    Updated(Box<OpsIncident>),
    NotFound,
    IllegalTransition(TransitionError),
}

/// Filter for the operator incident console.
#[derive(Debug, Clone, Default)]
pub struct IncidentOpsFilter {
    pub state: Option<IncidentState>,
    pub limit: Option<usize>,
}

#[async_trait]
pub trait IncidentOpsStore: Send + Sync {
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<OpsIncident>>;
    async fn list(&self, org: OrgId, filter: IncidentOpsFilter) -> Result<Vec<OpsIncident>>;
    async fn declare(
        &self,
        org: OrgId,
        new: NewManualIncident,
        actor: Actor,
    ) -> Result<OpsIncident>;
    async fn acknowledge(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome>;
    /// Manual resolve by a human (`resolved_by` = the actor's user).
    async fn resolve(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome>;
    /// Recovery detected by the writer (`resolved_by` = NULL, actor = system).
    async fn auto_resolve(&self, org: OrgId, id: Uuid) -> Result<LifecycleOutcome>;
    async fn reopen(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome>;
    async fn assign(
        &self,
        org: OrgId,
        id: Uuid,
        assignee: Option<UserId>,
        actor: Actor,
    ) -> Result<Option<OpsIncident>>;
    async fn add_note(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        message: String,
    ) -> Result<Option<IncidentEvent>>;
    async fn timeline(&self, org: OrgId, id: Uuid) -> Result<Vec<IncidentEvent>>;
}

// ── Postgres impl ────────────────────────────────────────────────────────

pub struct PgIncidentOpsStore {
    pool: PgPool,
}

impl PgIncidentOpsStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Columns selected for an [`OpsIncident`]. Kept in one place so every
/// `RETURNING` / `SELECT` stays in lockstep with [`OpsIncidentRow`].
const OPS_COLS: &str = "id, target_id, title, state, severity, urgency, origin, visibility, \
     started_at, ended_at, acknowledged_at, acknowledged_by, assigned_to, resolved_by, \
     escalation_policy_id, escalation_level, escalation_round, next_escalation_at, \
     check_count, error_sample, created_at, updated_at";

#[derive(sqlx::FromRow)]
struct OpsIncidentRow {
    id: Uuid,
    target_id: Option<Uuid>,
    title: Option<String>,
    state: String,
    severity: String,
    urgency: String,
    origin: String,
    visibility: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    acknowledged_at: Option<DateTime<Utc>>,
    acknowledged_by: Option<UserId>,
    assigned_to: Option<UserId>,
    resolved_by: Option<UserId>,
    escalation_policy_id: Option<Uuid>,
    escalation_level: i32,
    escalation_round: i32,
    next_escalation_at: Option<DateTime<Utc>>,
    check_count: i32,
    error_sample: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn row_to_ops(r: OpsIncidentRow) -> OpsIncident {
    OpsIncident {
        id: r.id,
        target_id: r.target_id,
        title: r.title,
        state: IncidentState::from_db_str(&r.state),
        severity: IncidentSeverity::from_db_str(&r.severity),
        urgency: IncidentUrgency::from_db_str(&r.urgency),
        origin: IncidentOrigin::from_db_str(&r.origin),
        visibility: IncidentVisibility::from_db_str(&r.visibility),
        started_at: r.started_at,
        ended_at: r.ended_at,
        acknowledged_at: r.acknowledged_at,
        acknowledged_by: r.acknowledged_by,
        assigned_to: r.assigned_to,
        resolved_by: r.resolved_by,
        escalation_policy_id: r.escalation_policy_id,
        escalation_level: r.escalation_level,
        escalation_round: r.escalation_round,
        next_escalation_at: r.next_escalation_at,
        check_count: r.check_count.max(0) as u64,
        error_sample: r.error_sample,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: Uuid,
    incident_id: Uuid,
    occurred_at: DateTime<Utc>,
    kind: String,
    actor_type: String,
    actor_id: Option<UserId>,
    detail: Value,
    message: Option<String>,
}

fn row_to_event(r: EventRow) -> IncidentEvent {
    IncidentEvent {
        id: r.id,
        incident_id: r.incident_id,
        occurred_at: r.occurred_at,
        kind: IncidentEventKind::from_db_str(&r.kind),
        actor_type: ActorType::from_db_str(&r.actor_type),
        actor_id: r.actor_id,
        detail: r.detail,
        message: r.message,
    }
}

/// Insert one internal timeline entry inside an open transaction. Assumes the
/// caller already proved the incident exists in `org` (e.g. via the locking
/// `SELECT ... FOR UPDATE`), so it binds `org.0` directly.
async fn insert_event_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: OrgId,
    incident_id: Uuid,
    kind: IncidentEventKind,
    actor: Actor,
    message: Option<&str>,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO incident_events (org_id, incident_id, kind, actor_type, actor_id, message)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
    )
    .bind(org.0)
    .bind(incident_id)
    .bind(kind.as_db_str())
    .bind(actor.actor_type().as_db_str())
    .bind(actor.user_id())
    .bind(message)
    .execute(&mut **tx)
    .await
    .map_err(|e| anyhow::anyhow!("insert incident_event: {e}"))?;
    Ok(())
}

impl PgIncidentOpsStore {
    /// Shared transition core: lock, read current state, apply the pure state
    /// machine, run `update` to mutate the row, then log `kind`.
    #[allow(clippy::too_many_arguments)]
    async fn transition(
        &self,
        org: OrgId,
        id: Uuid,
        transition: IncidentTransition,
        event_kind: IncidentEventKind,
        actor: Actor,
        note: Option<String>,
        update_sql: &str,
    ) -> Result<LifecycleOutcome> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        advisory_xact_lock(&mut *tx, &incident_lock_key(id))
            .await
            .map_err(|e| anyhow::anyhow!("incident lock: {e}"))?;

        let current: Option<(String,)> =
            sqlx::query_as("SELECT state FROM incidents WHERE id = $1 AND org_id = $2 FOR UPDATE")
                .bind(id)
                .bind(org.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| anyhow::anyhow!("load incident state: {e}"))?;
        let Some((state_str,)) = current else {
            return Ok(LifecycleOutcome::NotFound);
        };
        let from = IncidentState::from_db_str(&state_str);
        if let Err(err) = next_state(from, transition) {
            return Ok(LifecycleOutcome::IllegalTransition(err));
        }

        let row: OpsIncidentRow = sqlx::query_as(update_sql)
            .bind(id)
            .bind(org.0)
            .bind(actor.user_id())
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("apply transition: {e}"))?;

        insert_event_tx(&mut tx, org, id, event_kind, actor, note.as_deref()).await?;
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(LifecycleOutcome::Updated(Box::new(row_to_ops(row))))
    }
}

#[async_trait]
impl IncidentOpsStore for PgIncidentOpsStore {
    async fn get(&self, org: OrgId, id: Uuid) -> Result<Option<OpsIncident>> {
        let sql = format!("SELECT {OPS_COLS} FROM incidents WHERE id = $1 AND org_id = $2");
        let row: Option<OpsIncidentRow> = sqlx::query_as(&sql)
            .bind(id)
            .bind(org.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("get ops incident: {e}"))?;
        Ok(row.map(row_to_ops))
    }

    async fn list(&self, org: OrgId, filter: IncidentOpsFilter) -> Result<Vec<OpsIncident>> {
        let cap = filter.limit.unwrap_or(100).clamp(1, 1000) as i64;
        let state = filter.state.map(|s| s.as_db_str());
        let sql = format!(
            "SELECT {OPS_COLS} FROM incidents \
             WHERE org_id = $1 AND ($2::text IS NULL OR state = $2) \
             ORDER BY started_at DESC LIMIT $3"
        );
        let rows: Vec<OpsIncidentRow> = sqlx::query_as(&sql)
            .bind(org.0)
            .bind(state)
            .bind(cap)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("list ops incidents: {e}"))?;
        Ok(rows.into_iter().map(row_to_ops).collect())
    }

    async fn declare(
        &self,
        org: OrgId,
        new: NewManualIncident,
        actor: Actor,
    ) -> Result<OpsIncident> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        // status_at_start is a non-null column shared with monitor-opened
        // incidents; a declared incident has no check status, so it records the
        // declared problem as 'down'.
        let sql = format!(
            "INSERT INTO incidents \
                (org_id, target_id, started_at, status_at_start, origin, state, \
                 severity, urgency, title, visibility) \
             VALUES ($1, $2, now(), 'down', 'manual', 'triggered', $3, $4, $5, 'internal') \
             RETURNING {OPS_COLS}"
        );
        let row: OpsIncidentRow = sqlx::query_as(&sql)
            .bind(org.0)
            .bind(new.target_id)
            .bind(new.severity.as_db_str())
            .bind(new.urgency.as_db_str())
            .bind(new.title)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("declare incident: {e}"))?;
        let id = row.id;
        insert_event_tx(&mut tx, org, id, IncidentEventKind::Triggered, actor, None).await?;
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(row_to_ops(row))
    }

    async fn acknowledge(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome> {
        // COALESCE preserves the first acker + ack time on a re-ack (the state
        // machine treats Acknowledged→Acknowledged as idempotent), so MTTA and
        // on-call attribution reflect who actually took the page first.
        let sql = format!(
            "UPDATE incidents \
             SET state = 'acknowledged', \
                 acknowledged_at = COALESCE(acknowledged_at, now()), \
                 acknowledged_by = COALESCE(acknowledged_by, $3), \
                 next_escalation_at = NULL, updated_at = now() \
             WHERE id = $1 AND org_id = $2 RETURNING {OPS_COLS}"
        );
        self.transition(
            org,
            id,
            IncidentTransition::Acknowledge,
            IncidentEventKind::Acknowledged,
            actor,
            note,
            &sql,
        )
        .await
    }

    async fn resolve(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome> {
        // Manual resolve: resolved_by = actor's user (bound as $3).
        let sql = format!(
            "UPDATE incidents \
             SET state = 'resolved', ended_at = COALESCE(ended_at, now()), \
                 duration_secs = COALESCE(duration_secs, \
                     GREATEST(0, EXTRACT(EPOCH FROM (now() - started_at))::int)), \
                 resolved_by = $3, next_escalation_at = NULL, updated_at = now() \
             WHERE id = $1 AND org_id = $2 RETURNING {OPS_COLS}"
        );
        self.transition(
            org,
            id,
            IncidentTransition::Resolve,
            IncidentEventKind::Resolved,
            actor,
            note,
            &sql,
        )
        .await
    }

    async fn auto_resolve(&self, org: OrgId, id: Uuid) -> Result<LifecycleOutcome> {
        // System recovery: resolved_by stays NULL ($3 = system actor's NULL user).
        let sql = format!(
            "UPDATE incidents \
             SET state = 'resolved', ended_at = COALESCE(ended_at, now()), \
                 duration_secs = COALESCE(duration_secs, \
                     GREATEST(0, EXTRACT(EPOCH FROM (now() - started_at))::int)), \
                 resolved_by = $3, next_escalation_at = NULL, updated_at = now() \
             WHERE id = $1 AND org_id = $2 RETURNING {OPS_COLS}"
        );
        self.transition(
            org,
            id,
            IncidentTransition::AutoResolve,
            IncidentEventKind::Resolved,
            Actor::System,
            None,
            &sql,
        )
        .await
    }

    async fn reopen(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        note: Option<String>,
    ) -> Result<LifecycleOutcome> {
        // Reopen does not use the actor's user id in the row, but the shared
        // `transition` helper always binds it as $3 (ack/resolve do use it), so
        // the predicate references $3 as a deliberate no-op to keep the bound
        // parameter count in step with the statement.
        let sql = format!(
            "UPDATE incidents \
             SET state = 'triggered', ended_at = NULL, duration_secs = NULL, resolved_by = NULL, \
                 acknowledged_at = NULL, acknowledged_by = NULL, \
                 escalation_level = 0, escalation_round = 0, \
                 updated_at = now() \
             WHERE id = $1 AND org_id = $2 AND ($3::uuid IS NULL OR true) RETURNING {OPS_COLS}"
        );
        self.transition(
            org,
            id,
            IncidentTransition::Reopen,
            IncidentEventKind::Reopened,
            actor,
            note,
            &sql,
        )
        .await
    }

    async fn assign(
        &self,
        org: OrgId,
        id: Uuid,
        assignee: Option<UserId>,
        actor: Actor,
    ) -> Result<Option<OpsIncident>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin: {e}"))?;
        let sql = format!(
            "UPDATE incidents SET assigned_to = $3, updated_at = now() \
             WHERE id = $1 AND org_id = $2 RETURNING {OPS_COLS}"
        );
        let row: Option<OpsIncidentRow> = sqlx::query_as(&sql)
            .bind(id)
            .bind(org.0)
            .bind(assignee)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("assign incident: {e}"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind = if assignee.is_some() {
            IncidentEventKind::Assigned
        } else {
            IncidentEventKind::Unassigned
        };
        insert_event_tx(&mut tx, org, id, kind, actor, None).await?;
        tx.commit().await.map_err(|e| anyhow::anyhow!("commit: {e}"))?;
        Ok(Some(row_to_ops(row)))
    }

    async fn add_note(
        &self,
        org: OrgId,
        id: Uuid,
        actor: Actor,
        message: String,
    ) -> Result<Option<IncidentEvent>> {
        // SELECT-guarded INSERT: appending to another tenant's incident (or a
        // missing one) yields a clean no-op rather than an orphan event.
        let row: Option<EventRow> = sqlx::query_as(
            r#"INSERT INTO incident_events (org_id, incident_id, kind, actor_type, actor_id, message)
               SELECT i.org_id, $1, 'note', $2, $3, $4
               FROM incidents i WHERE i.id = $1 AND i.org_id = $5
               RETURNING id, incident_id, occurred_at, kind, actor_type, actor_id, detail, message"#,
        )
        .bind(id)
        .bind(actor.actor_type().as_db_str())
        .bind(actor.user_id())
        .bind(message)
        .bind(org.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("add_note: {e}"))?;
        Ok(row.map(row_to_event))
    }

    async fn timeline(&self, org: OrgId, id: Uuid) -> Result<Vec<IncidentEvent>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            r#"SELECT id, incident_id, occurred_at, kind, actor_type, actor_id, detail, message
               FROM incident_events
               WHERE incident_id = $1 AND org_id = $2
               ORDER BY occurred_at ASC"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("incident timeline: {e}"))?;
        Ok(rows.into_iter().map(row_to_event).collect())
    }
}

// ── In-memory impl (tests) ──────────────────────────────────────────────

#[derive(Default)]
pub struct InMemoryIncidentOpsStore {
    inner: Mutex<MemState>,
}

#[derive(Default)]
struct MemState {
    incidents: Vec<OpsIncident>,
    events: Vec<IncidentEvent>,
}

impl InMemoryIncidentOpsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn seed(&self, incident: OpsIncident) {
        self.inner.lock().incidents.push(incident);
    }

    fn push_event(state: &mut MemState, incident_id: Uuid, kind: IncidentEventKind, actor: Actor, message: Option<String>) {
        state.events.push(IncidentEvent {
            id: Uuid::now_v7(),
            incident_id,
            occurred_at: Utc::now(),
            kind,
            actor_type: actor.actor_type(),
            actor_id: actor.user_id(),
            detail: Value::Object(Default::default()),
            message,
        });
    }

    fn apply(
        &self,
        id: Uuid,
        transition: IncidentTransition,
        kind: IncidentEventKind,
        actor: Actor,
        note: Option<String>,
        mutate: impl FnOnce(&mut OpsIncident),
    ) -> LifecycleOutcome {
        let mut g = self.inner.lock();
        let Some(idx) = g.incidents.iter().position(|i| i.id == id) else {
            return LifecycleOutcome::NotFound;
        };
        let from = g.incidents[idx].state;
        if let Err(err) = next_state(from, transition) {
            return LifecycleOutcome::IllegalTransition(err);
        }
        mutate(&mut g.incidents[idx]);
        g.incidents[idx].updated_at = Utc::now();
        let updated = g.incidents[idx].clone();
        Self::push_event(&mut g, id, kind, actor, note);
        LifecycleOutcome::Updated(Box::new(updated))
    }
}

#[async_trait]
impl IncidentOpsStore for InMemoryIncidentOpsStore {
    async fn get(&self, _org: OrgId, id: Uuid) -> Result<Option<OpsIncident>> {
        Ok(self.inner.lock().incidents.iter().find(|i| i.id == id).cloned())
    }

    async fn list(&self, _org: OrgId, filter: IncidentOpsFilter) -> Result<Vec<OpsIncident>> {
        let mut out: Vec<OpsIncident> = self
            .inner
            .lock()
            .incidents
            .iter()
            .filter(|i| filter.state.is_none_or(|s| i.state == s))
            .cloned()
            .collect();
        out.sort_by_key(|i| std::cmp::Reverse(i.started_at));
        out.truncate(filter.limit.unwrap_or(100));
        Ok(out)
    }

    async fn declare(&self, _org: OrgId, new: NewManualIncident, actor: Actor) -> Result<OpsIncident> {
        let now = Utc::now();
        let inc = OpsIncident {
            id: Uuid::now_v7(),
            target_id: new.target_id,
            title: new.title,
            state: IncidentState::Triggered,
            severity: new.severity,
            urgency: new.urgency,
            origin: IncidentOrigin::Manual,
            visibility: IncidentVisibility::Internal,
            started_at: now,
            ended_at: None,
            acknowledged_at: None,
            acknowledged_by: None,
            assigned_to: None,
            resolved_by: None,
            escalation_policy_id: None,
            escalation_level: 0,
            escalation_round: 0,
            next_escalation_at: None,
            check_count: 0,
            error_sample: None,
            created_at: now,
            updated_at: now,
        };
        let mut g = self.inner.lock();
        g.incidents.push(inc.clone());
        Self::push_event(&mut g, inc.id, IncidentEventKind::Triggered, actor, None);
        Ok(inc)
    }

    async fn acknowledge(&self, _org: OrgId, id: Uuid, actor: Actor, note: Option<String>) -> Result<LifecycleOutcome> {
        Ok(self.apply(id, IncidentTransition::Acknowledge, IncidentEventKind::Acknowledged, actor, note, |i| {
            i.state = IncidentState::Acknowledged;
            i.acknowledged_at.get_or_insert_with(Utc::now);
            if i.acknowledged_by.is_none() {
                i.acknowledged_by = actor.user_id();
            }
            i.next_escalation_at = None;
        }))
    }

    async fn resolve(&self, _org: OrgId, id: Uuid, actor: Actor, note: Option<String>) -> Result<LifecycleOutcome> {
        Ok(self.apply(id, IncidentTransition::Resolve, IncidentEventKind::Resolved, actor, note, |i| {
            i.state = IncidentState::Resolved;
            i.ended_at.get_or_insert_with(Utc::now);
            i.resolved_by = actor.user_id();
            i.next_escalation_at = None;
        }))
    }

    async fn auto_resolve(&self, _org: OrgId, id: Uuid) -> Result<LifecycleOutcome> {
        Ok(self.apply(id, IncidentTransition::AutoResolve, IncidentEventKind::Resolved, Actor::System, None, |i| {
            i.state = IncidentState::Resolved;
            i.ended_at.get_or_insert_with(Utc::now);
            i.resolved_by = None;
            i.next_escalation_at = None;
        }))
    }

    async fn reopen(&self, _org: OrgId, id: Uuid, actor: Actor, note: Option<String>) -> Result<LifecycleOutcome> {
        Ok(self.apply(id, IncidentTransition::Reopen, IncidentEventKind::Reopened, actor, note, |i| {
            i.state = IncidentState::Triggered;
            i.ended_at = None;
            i.resolved_by = None;
            i.acknowledged_at = None;
            i.acknowledged_by = None;
            i.escalation_level = 0;
            i.escalation_round = 0;
        }))
    }

    async fn assign(&self, _org: OrgId, id: Uuid, assignee: Option<UserId>, actor: Actor) -> Result<Option<OpsIncident>> {
        let mut g = self.inner.lock();
        let Some(idx) = g.incidents.iter().position(|i| i.id == id) else {
            return Ok(None);
        };
        g.incidents[idx].assigned_to = assignee;
        g.incidents[idx].updated_at = Utc::now();
        let updated = g.incidents[idx].clone();
        let kind = if assignee.is_some() {
            IncidentEventKind::Assigned
        } else {
            IncidentEventKind::Unassigned
        };
        Self::push_event(&mut g, id, kind, actor, None);
        Ok(Some(updated))
    }

    async fn add_note(&self, _org: OrgId, id: Uuid, actor: Actor, message: String) -> Result<Option<IncidentEvent>> {
        let mut g = self.inner.lock();
        if !g.incidents.iter().any(|i| i.id == id) {
            return Ok(None);
        }
        Self::push_event(&mut g, id, IncidentEventKind::Note, actor, Some(message));
        Ok(g.events.last().cloned())
    }

    async fn timeline(&self, _org: OrgId, id: Uuid) -> Result<Vec<IncidentEvent>> {
        let mut out: Vec<IncidentEvent> = self
            .inner
            .lock()
            .events
            .iter()
            .filter(|e| e.incident_id == id)
            .cloned()
            .collect();
        out.sort_by_key(|e| e.occurred_at);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> OrgId {
        OrgId(Uuid::nil())
    }

    fn user() -> UserId {
        UserId(Uuid::now_v7())
    }

    fn seed_triggered(store: &InMemoryIncidentOpsStore) -> Uuid {
        let id = Uuid::now_v7();
        let now = Utc::now();
        store.seed(OpsIncident {
            id,
            target_id: Some(Uuid::now_v7()),
            title: None,
            state: IncidentState::Triggered,
            severity: IncidentSeverity::Major,
            urgency: IncidentUrgency::High,
            origin: IncidentOrigin::Monitor,
            visibility: IncidentVisibility::Internal,
            started_at: now,
            ended_at: None,
            acknowledged_at: None,
            acknowledged_by: None,
            assigned_to: None,
            resolved_by: None,
            escalation_policy_id: None,
            escalation_level: 0,
            escalation_round: 0,
            next_escalation_at: Some(now),
            check_count: 2,
            error_sample: Some("boom".into()),
            created_at: now,
            updated_at: now,
        });
        id
    }

    fn unwrap_updated(o: LifecycleOutcome) -> OpsIncident {
        match o {
            LifecycleOutcome::Updated(i) => *i,
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acknowledge_sets_owner_and_stops_escalation() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        let u = user();
        let inc = unwrap_updated(
            store
                .acknowledge(org(), id, Actor::User(u), Some("on it".into()))
                .await
                .unwrap(),
        );
        assert_eq!(inc.state, IncidentState::Acknowledged);
        assert_eq!(inc.acknowledged_by, Some(u));
        assert!(inc.next_escalation_at.is_none());
        let tl = store.timeline(org(), id).await.unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].kind, IncidentEventKind::Acknowledged);
    }

    #[tokio::test]
    async fn re_acknowledge_keeps_first_acker() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        let first = user();
        let acked = unwrap_updated(store.acknowledge(org(), id, Actor::User(first), None).await.unwrap());
        let first_at = acked.acknowledged_at;
        // A second responder re-acks; ownership + time must not be overwritten.
        let again = unwrap_updated(store.acknowledge(org(), id, Actor::User(user()), None).await.unwrap());
        assert_eq!(again.acknowledged_by, Some(first));
        assert_eq!(again.acknowledged_at, first_at);
    }

    #[tokio::test]
    async fn cannot_acknowledge_resolved() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        store.resolve(org(), id, Actor::User(user()), None).await.unwrap();
        let out = store.acknowledge(org(), id, Actor::User(user()), None).await.unwrap();
        assert!(matches!(out, LifecycleOutcome::IllegalTransition(_)));
    }

    #[tokio::test]
    async fn manual_resolve_records_user_auto_resolve_does_not() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        let u = user();
        let inc = unwrap_updated(store.resolve(org(), id, Actor::User(u), None).await.unwrap());
        assert_eq!(inc.state, IncidentState::Resolved);
        assert_eq!(inc.resolved_by, Some(u));
        assert!(inc.ended_at.is_some());

        let id2 = seed_triggered(&store);
        let inc2 = unwrap_updated(store.auto_resolve(org(), id2).await.unwrap());
        assert_eq!(inc2.resolved_by, None);
    }

    #[tokio::test]
    async fn reopen_resets_resolution_and_ack() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        store.acknowledge(org(), id, Actor::User(user()), None).await.unwrap();
        store.resolve(org(), id, Actor::User(user()), None).await.unwrap();
        let inc = unwrap_updated(store.reopen(org(), id, Actor::User(user()), None).await.unwrap());
        assert_eq!(inc.state, IncidentState::Triggered);
        assert!(inc.ended_at.is_none());
        assert!(inc.acknowledged_by.is_none());
        assert!(inc.resolved_by.is_none());
    }

    #[tokio::test]
    async fn assign_and_unassign_log_events() {
        let store = InMemoryIncidentOpsStore::new();
        let id = seed_triggered(&store);
        let u = user();
        let inc = store
            .assign(org(), id, Some(u), Actor::User(u))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(inc.assigned_to, Some(u));
        store.assign(org(), id, None, Actor::User(u)).await.unwrap();
        let tl = store.timeline(org(), id).await.unwrap();
        assert_eq!(tl.len(), 2);
        assert_eq!(tl[0].kind, IncidentEventKind::Assigned);
        assert_eq!(tl[1].kind, IncidentEventKind::Unassigned);
    }

    #[tokio::test]
    async fn add_note_on_missing_incident_is_none() {
        let store = InMemoryIncidentOpsStore::new();
        let res = store
            .add_note(org(), Uuid::now_v7(), Actor::System, "x".into())
            .await
            .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn list_filters_by_state() {
        let store = InMemoryIncidentOpsStore::new();
        let a = seed_triggered(&store);
        let _b = seed_triggered(&store);
        store.resolve(org(), a, Actor::System, None).await.unwrap();
        let triggered = store
            .list(org(), IncidentOpsFilter { state: Some(IncidentState::Triggered), limit: None })
            .await
            .unwrap();
        assert_eq!(triggered.len(), 1);
        let resolved = store
            .list(org(), IncidentOpsFilter { state: Some(IncidentState::Resolved), limit: None })
            .await
            .unwrap();
        assert_eq!(resolved.len(), 1);
    }
}
