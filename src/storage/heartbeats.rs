//! Storage for `heartbeat_monitors`: inbound ping state per heartbeat-kind
//! target. Token discipline mirrors `monitor_shares`: SHA-256 hex is the
//! lookup key, an encrypted copy re-displays the ping URL. The
//! disabled→enabled re-arm lives in `PostgresTargetStore`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::{OrgId, PingSignal};
use crate::error::{AppError, Result};
use crate::security::Cipher;
use crate::storage::capability_token;
use crate::worker::heartbeat::{Failure, PingState};

#[derive(Debug, Clone)]
pub struct HeartbeatMonitor {
    /// `None` when the KEK rotated out and the encrypted copy can't be opened.
    pub token: Option<String>,
    /// Last *success*. A `/start` must not hold a monitor up while the job it
    /// announced hangs, so it lives in `last_start_at` instead.
    pub last_ping_at: Option<DateTime<Utc>>,
    /// Silence-window start: creation, the wiring ping, every disabled→enabled
    /// flip. A failing wiring ping does not move it, or the failure it carries
    /// would be cleared by the statement that recorded it.
    pub armed_at: DateTime<Utc>,
    /// Wired-up point: the first ping of any signal, never cleared. `None`
    /// means the job has never spoken, which is not the same as silent.
    pub first_ping_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_start_at: Option<DateTime<Utc>>,
    pub last_fail_at: Option<DateTime<Utc>>,
    pub last_exit_code: Option<u8>,
}

impl HeartbeatMonitor {
    /// No ping of any signal yet: not evaluated, not alerting, no data. Single
    /// owner of the rule, since the API projection and the badge both ask.
    pub fn is_pending(&self) -> bool {
        self.first_ping_at.is_none()
    }

    pub fn ping_state(&self) -> PingState {
        ping_state_of(
            self.armed_at,
            self.last_ping_at,
            self.last_start_at,
            self.last_fail_at,
            self.last_exit_code,
        )
    }
}

/// Anchors on the later of the last success and the re-arm point, so a
/// pause/resume can't inherit stale silence or a stale failure.
pub(crate) fn ping_state_of(
    armed_at: DateTime<Utc>,
    last_ping_at: Option<DateTime<Utc>>,
    last_start_at: Option<DateTime<Utc>>,
    last_fail_at: Option<DateTime<Utc>>,
    last_exit_code: Option<u8>,
) -> PingState {
    PingState {
        success_at: last_ping_at.map_or(armed_at, |p| p.max(armed_at)),
        start_at: last_start_at,
        fail: last_fail_at.map(|at| Failure {
            at,
            exit_code: last_exit_code,
        }),
    }
}

/// A CHECK keeps the column in range, so anything outside it is corruption:
/// truncating would read 256 back as 0, reporting a failed run as a clean exit.
fn exit_code_of(stored: Option<i16>) -> Option<u8> {
    stored.and_then(|c| u8::try_from(c).ok())
}

/// Measured against the state as it stood *before* the signal landed.
/// Saturates: a run open past 49 days is a stuck job, not a negative duration.
fn run_ms_of(prev: &PingState, signal: PingSignal, at: DateTime<Utc>) -> Option<u32> {
    signal
        .is_finish()
        .then(|| prev.run_open_since())
        .flatten()
        .map(|started| {
            at.signed_duration_since(started)
                .num_milliseconds()
                .clamp(0, i64::from(u32::MAX)) as u32
        })
}

#[derive(Debug, Clone, Copy)]
pub struct PingAccepted {
    pub org_id: OrgId,
    pub target_id: Uuid,
    pub at: DateTime<Utc>,
    pub state: PingState,
    /// `None` on a start, and on a finish whose start never arrived.
    pub run_ms: Option<u32>,
    /// The signal that ended the pending wait. Nothing else reports on this
    /// monitor yet: it reaches the registry a refresh from now.
    pub first: bool,
    /// A paused monitor still accepts pings but reports no health.
    pub enabled: bool,
}

#[async_trait]
pub trait HeartbeatStore: Send + Sync {
    /// Mints a token if the row is absent. `None` when the target is not in `org`.
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>>;
    /// Never mints, so a read-scoped credential can't create a write capability.
    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>>;
    /// Target deletes cascade via the FK; this is for a kind switched away.
    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool>;
    /// `None` for unknown tokens and soft-deleted orgs, so both 404 the same way.
    async fn record_signal_by_token(
        &self,
        raw_token: &str,
        signal: PingSignal,
        exit_code: Option<u8>,
    ) -> Result<Option<PingAccepted>>;
}

pub struct PgHeartbeatStore {
    pool: PgPool,
    cipher: Option<Arc<Cipher>>,
}

impl PgHeartbeatStore {
    pub fn new(pool: PgPool, cipher: Option<Arc<Cipher>>) -> Self {
        Self { pool, cipher }
    }
}

#[derive(sqlx::FromRow)]
struct HeartbeatRow {
    token_enc: String,
    last_ping_at: Option<DateTime<Utc>>,
    armed_at: DateTime<Utc>,
    first_ping_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    last_start_at: Option<DateTime<Utc>>,
    last_fail_at: Option<DateTime<Utc>>,
    last_exit_code: Option<i16>,
}

impl HeartbeatRow {
    fn into_monitor(self, cipher: Option<&Cipher>) -> HeartbeatMonitor {
        HeartbeatMonitor {
            token: capability_token::open(&self.token_enc, cipher),
            last_ping_at: self.last_ping_at,
            armed_at: self.armed_at,
            first_ping_at: self.first_ping_at,
            created_at: self.created_at,
            last_start_at: self.last_start_at,
            last_fail_at: self.last_fail_at,
            last_exit_code: exit_code_of(self.last_exit_code),
        }
    }
}

const HB_COLUMNS: &str = "token_enc, last_ping_at, armed_at, first_ping_at, created_at, \
     last_start_at, last_fail_at, last_exit_code";

#[async_trait]
impl HeartbeatStore for PgHeartbeatStore {
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        if let Some(existing) = self.get(org, target_id).await? {
            return Ok(Some(existing));
        }
        let minted = capability_token::mint(self.cipher.as_deref())?;
        // SELECT-from-targets scopes the insert to the org; ON CONFLICT keeps a
        // token minted by a concurrent create stable.
        sqlx::query(
            "INSERT INTO heartbeat_monitors (target_id, org_id, token_hash, token_enc) \
             SELECT t.id, t.org_id, $3, $4 FROM targets t \
             WHERE t.id = $2 AND t.org_id = $1 \
             ON CONFLICT (target_id) DO NOTHING",
        )
        .bind(org.0)
        .bind(target_id)
        .bind(&minted.hash)
        .bind(&minted.sealed)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        self.get(org, target_id).await
    }

    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        let row: Option<HeartbeatRow> = sqlx::query_as(&format!(
            "SELECT {HB_COLUMNS} FROM heartbeat_monitors WHERE org_id = $1 AND target_id = $2"
        ))
        .bind(org.0)
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|r| r.into_monitor(self.cipher.as_deref())))
    }

    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool> {
        let res =
            sqlx::query("DELETE FROM heartbeat_monitors WHERE org_id = $1 AND target_id = $2")
                .bind(org.0)
                .bind(target_id)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn record_signal_by_token(
        &self,
        raw_token: &str,
        signal: PingSignal,
        exit_code: Option<u8>,
    ) -> Result<Option<PingAccepted>> {
        // The CTE is the only view of the start this signal closes; RETURNING
        // shows the new row. FOR UPDATE pins the CTE against inlining and makes
        // a statement that waited on a concurrent ping re-read what it
        // committed, instead of timing the run against a start already closed.
        let row: Option<AcceptedRow> = sqlx::query_as(
            "WITH prev AS ( \
                 SELECT hm.target_id, hm.last_ping_at, hm.last_start_at, hm.last_fail_at, \
                        hm.armed_at, hm.first_ping_at, t.enabled \
                 FROM heartbeat_monitors hm \
                 JOIN organizations o ON o.id = hm.org_id AND o.deleted_at IS NULL \
                 JOIN targets t ON t.id = hm.target_id \
                 WHERE hm.token_hash = $1 \
                 FOR UPDATE OF hm \
             ) \
             UPDATE heartbeat_monitors hm SET \
                 armed_at       = CASE WHEN hm.first_ping_at IS NULL AND $2 <> 'fail' \
                                       THEN now() ELSE hm.armed_at END, \
                 last_ping_at   = CASE WHEN $2 = 'success' THEN now() ELSE hm.last_ping_at END, \
                 last_start_at  = CASE WHEN $2 = 'start'   THEN now() ELSE hm.last_start_at END, \
                 last_fail_at   = CASE WHEN $2 = 'fail'    THEN now() ELSE hm.last_fail_at END, \
                 last_exit_code = CASE WHEN $2 = 'fail'    THEN $3   ELSE hm.last_exit_code END, \
                 first_ping_at  = COALESCE(hm.first_ping_at, now()) \
             FROM prev \
             WHERE hm.target_id = prev.target_id \
             RETURNING hm.org_id, hm.target_id, hm.armed_at, hm.last_ping_at, hm.last_start_at, \
                       hm.last_fail_at, hm.last_exit_code, \
                       prev.last_ping_at AS prev_ping_at, prev.last_start_at AS prev_start_at, \
                       prev.last_fail_at AS prev_fail_at, prev.armed_at AS prev_armed_at, \
                       prev.first_ping_at AS prev_first_ping_at, prev.enabled",
        )
        .bind(capability_token::hash(raw_token))
        .bind(signal.as_str())
        .bind(exit_code.map(i16::from))
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|r| r.into_accepted(signal)))
    }
}

#[derive(sqlx::FromRow)]
struct AcceptedRow {
    org_id: Uuid,
    target_id: Uuid,
    armed_at: DateTime<Utc>,
    last_ping_at: Option<DateTime<Utc>>,
    last_start_at: Option<DateTime<Utc>>,
    last_fail_at: Option<DateTime<Utc>>,
    last_exit_code: Option<i16>,
    prev_ping_at: Option<DateTime<Utc>>,
    prev_start_at: Option<DateTime<Utc>>,
    prev_fail_at: Option<DateTime<Utc>>,
    prev_armed_at: DateTime<Utc>,
    prev_first_ping_at: Option<DateTime<Utc>>,
    enabled: bool,
}

impl AcceptedRow {
    fn into_accepted(self, signal: PingSignal) -> PingAccepted {
        let state = ping_state_of(
            self.armed_at,
            self.last_ping_at,
            self.last_start_at,
            self.last_fail_at,
            exit_code_of(self.last_exit_code),
        );
        // Whichever column the statement's `now()` wrote holds the accept time.
        let at = match signal {
            PingSignal::Start => self.last_start_at,
            PingSignal::Success => self.last_ping_at,
            PingSignal::Fail => self.last_fail_at,
        }
        .unwrap_or(state.success_at);
        let prev = ping_state_of(
            self.prev_armed_at,
            self.prev_ping_at,
            self.prev_start_at,
            self.prev_fail_at,
            None,
        );
        PingAccepted {
            org_id: OrgId(self.org_id),
            target_id: self.target_id,
            at,
            state,
            run_ms: run_ms_of(&prev, signal, at),
            first: self.prev_first_ping_at.is_none(),
            enabled: self.enabled,
        }
    }
}

fn db_err(e: sqlx::Error) -> AppError {
    AppError::Other(anyhow::anyhow!("heartbeat_monitors: {e}"))
}

// ── In-memory store (no-DB harnesses) ─────────────────────────────────────────

struct MemHeartbeat {
    org: OrgId,
    target_id: Uuid,
    monitor: HeartbeatMonitor,
    token_hash: String,
}

/// No target/org tables here, so the org-membership and soft-delete guards are
/// absent; DB-backed tests cover those.
#[derive(Default)]
pub struct InMemoryHeartbeatStore {
    inner: std::sync::Mutex<Vec<MemHeartbeat>>,
}

impl InMemoryHeartbeatStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl HeartbeatStore for InMemoryHeartbeatStore {
    async fn ensure(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        let mut st = self.inner.lock().unwrap();
        if let Some(m) = st.iter().find(|m| m.org == org && m.target_id == target_id) {
            return Ok(Some(m.monitor.clone()));
        }
        let minted = capability_token::mint(None)?;
        let monitor = HeartbeatMonitor {
            token: Some(minted.raw),
            last_ping_at: None,
            armed_at: Utc::now(),
            first_ping_at: None,
            created_at: Utc::now(),
            last_start_at: None,
            last_fail_at: None,
            last_exit_code: None,
        };
        st.push(MemHeartbeat {
            org,
            target_id,
            monitor: monitor.clone(),
            token_hash: minted.hash,
        });
        Ok(Some(monitor))
    }

    async fn get(&self, org: OrgId, target_id: Uuid) -> Result<Option<HeartbeatMonitor>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.org == org && m.target_id == target_id)
            .map(|m| m.monitor.clone()))
    }

    async fn remove(&self, org: OrgId, target_id: Uuid) -> Result<bool> {
        let mut st = self.inner.lock().unwrap();
        let before = st.len();
        st.retain(|m| !(m.org == org && m.target_id == target_id));
        Ok(st.len() < before)
    }

    async fn record_signal_by_token(
        &self,
        raw_token: &str,
        signal: PingSignal,
        exit_code: Option<u8>,
    ) -> Result<Option<PingAccepted>> {
        let hash = capability_token::hash(raw_token);
        let now = Utc::now();
        Ok(self
            .inner
            .lock()
            .unwrap()
            .iter_mut()
            .find(|m| m.token_hash == hash)
            .map(|m| {
                let prev = m.monitor.ping_state();
                let first = m.monitor.is_pending();
                if first && signal != PingSignal::Fail {
                    m.monitor.armed_at = now;
                }
                m.monitor.first_ping_at = m.monitor.first_ping_at.or(Some(now));
                match signal {
                    PingSignal::Start => m.monitor.last_start_at = Some(now),
                    PingSignal::Success => m.monitor.last_ping_at = Some(now),
                    PingSignal::Fail => {
                        m.monitor.last_fail_at = Some(now);
                        m.monitor.last_exit_code = exit_code;
                    }
                }
                PingAccepted {
                    org_id: m.org,
                    target_id: m.target_id,
                    at: now,
                    state: m.monitor.ping_state(),
                    run_ms: run_ms_of(&prev, signal, now),
                    first,
                    // No targets table here; the pause path is a live-PG concern.
                    enabled: true,
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_is_idempotent_and_ping_round_trips() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let a = store.ensure(org, target).await.unwrap().unwrap();
        let b = store.ensure(org, target).await.unwrap().unwrap();
        assert_eq!(a.token, b.token, "repeated ensure must keep the token");
        assert!(a.last_ping_at.is_none());

        let ok = store
            .record_signal_by_token(a.token.as_deref().unwrap(), PingSignal::Success, None)
            .await
            .unwrap()
            .expect("token resolves");
        assert_eq!(ok.target_id, target);
        let read = store.get(org, target).await.unwrap().unwrap();
        assert_eq!(read.last_ping_at, Some(ok.at));
        assert!(
            store
                .record_signal_by_token("nope", PingSignal::Success, None)
                .await
                .unwrap()
                .is_none(),
            "unknown tokens record nothing"
        );
    }

    #[tokio::test]
    async fn a_finish_measures_the_run_its_start_opened() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let token = store
            .ensure(org, target)
            .await
            .unwrap()
            .unwrap()
            .token
            .unwrap();

        let started = store
            .record_signal_by_token(&token, PingSignal::Start, None)
            .await
            .unwrap()
            .unwrap();
        assert!(started.run_ms.is_none(), "a start closes nothing");
        assert!(started.state.run_open_since().is_some());

        let failed = store
            .record_signal_by_token(&token, PingSignal::Fail, Some(2))
            .await
            .unwrap()
            .unwrap();
        assert!(failed.run_ms.is_some(), "the fail closed the open run");
        assert_eq!(failed.state.failing().and_then(|f| f.exit_code), Some(2));
        assert!(failed.state.run_open_since().is_none());

        // Nothing is open now, so the next finish measures no run.
        let done = store
            .record_signal_by_token(&token, PingSignal::Success, None)
            .await
            .unwrap()
            .unwrap();
        assert!(done.run_ms.is_none(), "an unpaired finish times nothing");
        assert!(done.state.failing().is_none(), "success clears the failure");
    }

    #[tokio::test]
    async fn the_first_ping_of_any_signal_wires_the_monitor_up_once() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let fresh = store.ensure(org, target).await.unwrap().unwrap();
        assert!(
            fresh.first_ping_at.is_none(),
            "creating a monitor is not the job speaking"
        );

        // A start is the job speaking too, so it ends the pending state.
        let started = store
            .record_signal_by_token(fresh.token.as_deref().unwrap(), PingSignal::Start, None)
            .await
            .unwrap()
            .unwrap();
        let wired = store.get(org, target).await.unwrap().unwrap().first_ping_at;
        assert_eq!(wired, Some(started.at));

        store
            .record_signal_by_token(fresh.token.as_deref().unwrap(), PingSignal::Success, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get(org, target).await.unwrap().unwrap().first_ping_at,
            wired,
            "later pings leave the wired-up point alone"
        );
    }

    #[tokio::test]
    async fn removed_token_stops_recording() {
        let store = InMemoryHeartbeatStore::new();
        let org = OrgId(Uuid::new_v4());
        let target = Uuid::new_v4();
        let m = store.ensure(org, target).await.unwrap().unwrap();
        assert!(store.remove(org, target).await.unwrap());
        assert!(
            store
                .record_signal_by_token(m.token.as_deref().unwrap(), PingSignal::Success, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_out_of_range_exit_code_reads_as_unknown_not_success() {
        assert_eq!(exit_code_of(Some(137)), Some(137));
        assert_eq!(exit_code_of(Some(0)), Some(0));
        assert_eq!(exit_code_of(Some(256)), None);
        assert_eq!(exit_code_of(Some(-1)), None);
        assert_eq!(exit_code_of(None), None);
    }

    #[test]
    fn the_anchor_is_the_later_of_arm_and_success() {
        let armed = Utc::now();
        let earlier = armed - chrono::Duration::hours(1);
        let later = armed + chrono::Duration::hours(1);
        let at = |ping| ping_state_of(armed, ping, None, None, None).success_at;
        assert_eq!(at(None), armed);
        assert_eq!(at(Some(earlier)), armed);
        assert_eq!(at(Some(later)), later);
    }

    #[test]
    fn a_re_arm_outranks_a_failure_recorded_before_it() {
        // Resume must not inherit the failure the pause froze.
        let armed = Utc::now();
        let state = ping_state_of(
            armed,
            None,
            None,
            Some(armed - chrono::Duration::hours(1)),
            Some(1),
        );
        assert!(state.failing().is_none());
    }
}
