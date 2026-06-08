//! Aggregator that assembles a public status page payload from PostgreSQL and
//! ClickHouse.
//!
//! The aggregator is intentionally **read-only**, **idempotent**, and
//! **side-effect-free** — it owns no caches and no background tasks. Caching is
//! layered on top in [`super::cache`]; the incident materialisation writer
//! lives in its own module.
//!
//! Everything is scoped to a single page: a page selects its monitors via the
//! `status_page_components` join, and each binding carries the per-page public
//! name / group / order. Incidents, maintenance, and history are filtered to
//! the page's component (target_id) set; component names resolve from the
//! page's curation map, falling back to the monitor's own name.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use clickhouse::{Client as ClickhouseClient, Row};
use serde::Deserialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domain::{
    ComponentHistoryResponse, DayState, IncidentSeverity, IncidentStatusPhase, OrgId,
    PublicComponent, PublicComponentGroup, PublicComponentStatus, PublicIncident,
    PublicIncidentUpdate, PublicMaintenance, PublicStatusPage, StatusPageId,
};
use crate::error::Result;

use super::auto_incident_title;
use super::cache::HistoryIncidentMarker;
use super::overall_status::{Counters, component_status, overall_state, overall_status};

/// Aggregator-local configuration. Holds only the knobs the aggregator reads
/// directly; the shared `AppConfig` may shadow these later when env overrides
/// are wired through.
#[derive(Debug, Clone)]
pub struct AggregatorConfig {
    pub site_name: String,
    pub history_days: u32,
    pub recent_incidents_days: u32,
    pub max_recent_incidents: u32,
    pub upcoming_maintenance_horizon: ChronoDuration,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            site_name: "uptimepage".into(),
            history_days: 90,
            recent_incidents_days: 30,
            max_recent_incidents: 50,
            upcoming_maintenance_horizon: ChronoDuration::days(7),
        }
    }
}

const CH_TABLE: &str = "check_results";
// Day-resolution strip reads the hour rollup (13-month tail), so it spans the
// full `history_days` window independent of the shorter 1m-rollup TTL.
const CH_HISTORY_MV: &str = "check_results_1h";

/// One monitor as it sits on a page: its target id, the resolved public name
/// (per-page override or the monitor's own name), and the page-local grouping.
struct PageComponent {
    id: Uuid,
    name: String,
    description: Option<String>,
    group: Option<String>,
    sort_order: i32,
}

/// Page-scoped aggregator. Carries no ids of its own — every method takes the
/// `(page, org)` pair, so the compiler refuses a call site that forgot which
/// page is being built. `org` rides alongside `page` for tenant-scoped queries.
pub struct OrgAggregator {
    pg: PgPool,
    ch: ClickhouseClient,
    cfg: AggregatorConfig,
}

impl OrgAggregator {
    pub fn new(pg: PgPool, ch: ClickhouseClient, cfg: AggregatorConfig) -> Self {
        Self { pg, ch, cfg }
    }

    /// One atomic snapshot per call: page + 90-day popover markers + the
    /// `target_id → public name` map (cached for the incident read paths).
    pub async fn build(
        &self,
        page: StatusPageId,
        org: OrgId,
    ) -> Result<(
        PublicStatusPage,
        Vec<HistoryIncidentMarker>,
        HashMap<Uuid, String>,
    )> {
        let now = Utc::now();
        let components = self.load_page_components(page, org).await?;
        let component_ids: Vec<Uuid> = components.iter().map(|c| c.id).collect();
        let name_by_id: HashMap<Uuid, String> =
            components.iter().map(|c| (c.id, c.name.clone())).collect();

        let (
            (active_maintenance, upcoming_maintenance, maintenance_by_target),
            active_incidents,
            (recent_incidents, recent_incidents_has_more),
            history_markers,
        ) = tokio::try_join!(
            self.load_maintenance(org, now, &component_ids, &name_by_id),
            self.load_active_incidents(org, &component_ids, &name_by_id),
            self.load_recent_incidents(org, now, &component_ids, &name_by_id),
            self.load_history_markers(org, now, &component_ids, &name_by_id),
        )?;

        // Index by target_id so the per-component loop is O(1), not O(n²) — a
        // page with hundreds of monitors otherwise linear-scans both vecs per
        // component on every cache miss.
        let history_by_target: HashMap<Uuid, Vec<DayState>> = self
            .load_history_strips(org, &component_ids, now)
            .await?
            .into_iter()
            .collect();
        let recent_counters: HashMap<Uuid, Counters> = self
            .load_recent_counters(org, &component_ids, now)
            .await?
            .into_iter()
            .collect();

        let mut public_components: Vec<(Option<String>, i32, PublicComponent)> = components
            .iter()
            .map(|c| {
                let maint = maintenance_by_target.contains(&c.id);
                let counters = recent_counters.get(&c.id).copied().unwrap_or_default();
                let current = component_status(&counters, maint);
                let history = history_by_target
                    .get(&c.id)
                    .cloned()
                    .unwrap_or_else(|| vec![DayState::NoData; self.cfg.history_days as usize]);
                let pc = PublicComponent {
                    id: c.id,
                    name: c.name.clone(),
                    description: c.description.clone(),
                    current_status: current,
                    history,
                };
                (c.group.clone(), c.sort_order, pc)
            })
            .collect();

        // Sort within group: by sort_order ASC, then name ASC.
        public_components.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.name.cmp(&b.2.name))
        });

        // Ungrouped (None) renders last; sort_by above puts None first because
        // Option<T>::cmp orders None < Some. Flip the bucket after grouping.
        let mut groups: Vec<PublicComponentGroup> = Vec::new();
        let mut current_group: Option<String> = None;
        let mut current_items: Vec<PublicComponent> = Vec::new();
        for (g, _ord, pc) in public_components {
            if g != current_group {
                if !current_items.is_empty() || current_group.is_some() {
                    groups.push(PublicComponentGroup {
                        name: current_group.clone(),
                        components: std::mem::take(&mut current_items),
                    });
                }
                current_group = g;
            }
            current_items.push(pc);
        }
        if !current_items.is_empty() {
            groups.push(PublicComponentGroup {
                name: current_group,
                components: current_items,
            });
        }
        // Move the ungrouped bucket to the end.
        if let Some(idx) = groups.iter().position(|g| g.name.is_none()) {
            let ung = groups.remove(idx);
            groups.push(ung);
        }

        let component_statuses: Vec<PublicComponentStatus> = groups
            .iter()
            .flat_map(|g| g.components.iter().map(|c| c.current_status))
            .collect();
        let overall = overall_status(overall_state(&component_statuses));

        Ok((
            PublicStatusPage {
                overall,
                generated_at: now,
                site_name: self.cfg.site_name.clone(),
                groups,
                active_incidents,
                recent_incidents,
                recent_incidents_has_more,
                active_maintenance,
                upcoming_maintenance,
            },
            history_markers,
            name_by_id,
        ))
    }

    /// Per-component history endpoint (`GET /api/public/v1/components/{id}/history`).
    /// 404s (via the error) when the target isn't on this page.
    pub async fn component_history(
        &self,
        page: StatusPageId,
        org: OrgId,
        id: Uuid,
        days: u32,
    ) -> Result<ComponentHistoryResponse> {
        let now = Utc::now();
        let components = self.load_page_components(page, org).await?;
        let component = components
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("component not on this page"))?;

        let history_by_target = self.load_history_strips(org, &[id], now).await?;
        let history = history_by_target
            .into_iter()
            .find(|(target_id, _)| *target_id == id)
            .map(|(_, h)| h)
            .unwrap_or_else(|| vec![DayState::NoData; days as usize]);
        let history = pad_or_truncate(history, days as usize);

        Ok(ComponentHistoryResponse {
            component_id: id,
            component_name: component.name,
            days,
            history,
        })
    }

    // ── private helpers ─────────────────────────────────────────────────────

    /// The page's monitors, with per-page curation applied. Ordered by
    /// (group, sort_order, monitor name) to match the rendered layout.
    async fn load_page_components(
        &self,
        page: StatusPageId,
        org: OrgId,
    ) -> Result<Vec<PageComponent>> {
        let rows: Vec<PageComponentRow> = sqlx::query_as::<_, PageComponentRow>(
            r#"SELECT spc.target_id, t.name AS monitor_name,
                      spc.public_name, spc.public_description,
                      spc.public_group, spc.sort_order
               FROM status_page_components spc
               JOIN targets t ON t.id = spc.target_id AND t.org_id = spc.org_id
               WHERE spc.status_page_id = $1 AND spc.org_id = $2
               ORDER BY spc.public_group NULLS LAST, spc.sort_order, t.name"#,
        )
        .bind(page.0)
        .bind(org.0)
        .fetch_all(&self.pg)
        .await
        .context("load page components")?;
        Ok(rows
            .into_iter()
            .map(|r| PageComponent {
                id: r.target_id,
                name: r.public_name.unwrap_or(r.monitor_name),
                description: r.public_description,
                group: r.public_group,
                sort_order: r.sort_order,
            })
            .collect())
    }

    async fn load_maintenance(
        &self,
        org: OrgId,
        now: DateTime<Utc>,
        component_ids: &[Uuid],
        name_by_id: &HashMap<Uuid, String>,
    ) -> Result<(Vec<PublicMaintenance>, Vec<PublicMaintenance>, Vec<Uuid>)> {
        let horizon_end = now + self.cfg.upcoming_maintenance_horizon;
        let rows: Vec<MaintenanceRow> = sqlx::query_as::<_, MaintenanceRow>(
            r#"SELECT mw.id, mw.title, mw.description, mw.starts_at, mw.ends_at,
                      COALESCE(
                        ARRAY_AGG(mwc.target_id) FILTER (WHERE mwc.target_id IS NOT NULL),
                        '{}'::uuid[]
                      ) AS component_ids
               FROM maintenance_windows mw
               LEFT JOIN maintenance_window_components mwc ON mwc.maintenance_id = mw.id
               WHERE mw.org_id = $3
                 AND mw.ends_at > $1
                 AND mw.starts_at < $2
               GROUP BY mw.id
               ORDER BY mw.starts_at ASC"#,
        )
        .bind(now)
        .bind(horizon_end)
        .bind(org.0)
        .fetch_all(&self.pg)
        .await
        .context("load maintenance windows")?;

        let on_page: std::collections::HashSet<Uuid> = component_ids.iter().copied().collect();
        let mut active = Vec::new();
        let mut upcoming = Vec::new();
        let mut active_target_ids: Vec<Uuid> = Vec::new();
        for row in rows {
            // Only surface windows that touch at least one of THIS page's components.
            let on_page_components: Vec<Uuid> = row
                .component_ids
                .iter()
                .copied()
                .filter(|id| on_page.contains(id))
                .collect();
            if on_page_components.is_empty() {
                continue;
            }
            let names: Vec<String> = on_page_components
                .iter()
                .filter_map(|id| name_by_id.get(id).cloned())
                .collect();
            let pm = PublicMaintenance {
                id: row.id,
                title: row.title,
                description: row.description,
                starts_at: row.starts_at,
                ends_at: row.ends_at,
                affected_component_names: names,
            };
            if row.starts_at <= now && row.ends_at > now {
                active_target_ids.extend(on_page_components);
                active.push(pm);
            } else if row.starts_at > now {
                upcoming.push(pm);
            }
        }
        Ok((active, upcoming, active_target_ids))
    }

    async fn load_active_incidents(
        &self,
        org: OrgId,
        component_ids: &[Uuid],
        name_by_id: &HashMap<Uuid, String>,
    ) -> Result<Vec<PublicIncident>> {
        if component_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<IncidentRow> = sqlx::query_as::<_, IncidentRow>(
            r#"SELECT i.id, i.target_id,
                      i.started_at, i.ended_at, i.severity, i.status_at_start,
                      i.public_title, i.public_description
               FROM incidents i
               WHERE i.org_id = $1
                 AND i.ended_at IS NULL
                 AND i.target_id = ANY($2)
                 AND i.visibility = 'public'
               ORDER BY i.started_at DESC"#,
        )
        .bind(org.0)
        .bind(component_ids)
        .fetch_all(&self.pg)
        .await
        .context("load active incidents")?;
        self.hydrate_incidents(org, rows, name_by_id).await
    }

    async fn load_recent_incidents(
        &self,
        org: OrgId,
        now: DateTime<Utc>,
        component_ids: &[Uuid],
        name_by_id: &HashMap<Uuid, String>,
    ) -> Result<(Vec<PublicIncident>, bool)> {
        if component_ids.is_empty() {
            return Ok((Vec::new(), false));
        }
        let since = now - ChronoDuration::days(self.cfg.recent_incidents_days as i64);
        let peek_limit = self.cfg.max_recent_incidents as i64 + 1;
        let mut rows: Vec<IncidentRow> = sqlx::query_as::<_, IncidentRow>(
            r#"SELECT i.id, i.target_id,
                      i.started_at, i.ended_at, i.severity, i.status_at_start,
                      i.public_title, i.public_description
               FROM incidents i
               WHERE i.org_id = $3
                 AND i.started_at >= $1
                 AND i.target_id = ANY($4)
                 AND i.visibility = 'public'
               ORDER BY i.started_at DESC, i.id DESC
               LIMIT $2"#,
        )
        .bind(since)
        .bind(peek_limit)
        .bind(org.0)
        .bind(component_ids)
        .fetch_all(&self.pg)
        .await
        .context("load recent incidents")?;
        let has_more = rows.len() as u32 > self.cfg.max_recent_incidents;
        if has_more {
            rows.truncate(self.cfg.max_recent_incidents as usize);
        }
        let hydrated = self.hydrate_incidents(org, rows, name_by_id).await?;
        Ok((hydrated, has_more))
    }

    /// 90-day slim incident pool for the popover matcher. 1000-row cap guards
    /// against an incident-spam tenant blowing the rendered JSON.
    async fn load_history_markers(
        &self,
        org: OrgId,
        now: DateTime<Utc>,
        component_ids: &[Uuid],
        name_by_id: &HashMap<Uuid, String>,
    ) -> Result<Vec<HistoryIncidentMarker>> {
        if component_ids.is_empty() {
            return Ok(Vec::new());
        }
        let since = now - ChronoDuration::days(self.cfg.history_days as i64);
        let rows: Vec<HistoryMarkerRow> = sqlx::query_as::<_, HistoryMarkerRow>(
            r#"SELECT i.id, i.target_id,
                      i.public_title, i.status_at_start,
                      i.started_at, i.ended_at
               FROM incidents i
               WHERE i.org_id = $2
                 AND (i.ended_at IS NULL OR i.ended_at >= $1)
                 AND i.target_id = ANY($3)
                 AND i.visibility = 'public'
               ORDER BY i.started_at DESC
               LIMIT 1000"#,
        )
        .bind(since)
        .bind(org.0)
        .bind(component_ids)
        .fetch_all(&self.pg)
        .await
        .context("load history-window incident markers")?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let component_name = name_by_id.get(&r.target_id).cloned().unwrap_or_default();
                HistoryIncidentMarker {
                    id: r.id,
                    component_id: r.target_id,
                    title: truncate_title(r.public_title.unwrap_or_else(|| {
                        auto_incident_title(&component_name, &r.status_at_start)
                    })),
                    started_at: r.started_at,
                    ended_at: r.ended_at,
                }
            })
            .collect())
    }

    async fn hydrate_incidents(
        &self,
        org: OrgId,
        rows: Vec<IncidentRow>,
        name_by_id: &HashMap<Uuid, String>,
    ) -> Result<Vec<PublicIncident>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let updates: Vec<IncidentUpdateRow> = sqlx::query_as::<_, IncidentUpdateRow>(
            r#"SELECT incident_id, posted_at, phase, message
               FROM incident_updates
               WHERE incident_id = ANY($1) AND org_id = $2
               ORDER BY incident_id, posted_at ASC"#,
        )
        .bind(&ids)
        .bind(org.0)
        .fetch_all(&self.pg)
        .await
        .context("load incident updates")?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let component_name = name_by_id.get(&r.target_id).cloned().unwrap_or_default();
                let my_updates: Vec<PublicIncidentUpdate> = updates
                    .iter()
                    .filter(|u| u.incident_id == r.id)
                    .map(|u| PublicIncidentUpdate {
                        posted_at: u.posted_at,
                        phase: IncidentStatusPhase::from_db_str(&u.phase),
                        message: u.message.clone(),
                    })
                    .collect();
                let status_phase = my_updates
                    .last()
                    .map(|u| u.phase)
                    .unwrap_or(IncidentStatusPhase::Investigating);
                let title = r
                    .public_title
                    .clone()
                    .unwrap_or_else(|| auto_incident_title(&component_name, &r.status_at_start));
                PublicIncident {
                    id: r.id,
                    component_id: r.target_id,
                    component_name,
                    title,
                    started_at: r.started_at,
                    ended_at: r.ended_at,
                    severity: IncidentSeverity::from_db_str(&r.severity),
                    status_phase,
                    updates: my_updates,
                    postmortem: None,
                }
            })
            .collect())
    }

    /// Fetches per-day worst-minute classification for each component using the
    /// existing `check_results_1m` aggregating-merge view. Returns one
    /// `Vec<DayState>` per component, oldest-first, length = `history_days`.
    async fn load_history_strips(
        &self,
        org: OrgId,
        component_ids: &[Uuid],
        now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, Vec<DayState>)>> {
        if component_ids.is_empty() {
            return Ok(Vec::new());
        }
        let from = now - ChronoDuration::days(self.cfg.history_days as i64);
        let rows: Vec<HistoryDayRow> = self
            .ch
            .query(&format!(
                r#"SELECT
                    target_id,
                    toInt64(toUnixTimestamp(day)) AS day,
                    maxIf(toUInt8(total > 0 AND (total - up_) * 2 >= total), total > 0) AS any_major,
                    maxIf(toUInt8(total > 0 AND (total - up_) > 0), total > 0) AS any_failure,
                    sum(total) AS day_total
                FROM (
                    SELECT
                        target_id,
                        toStartOfDay(hour) AS day,
                        countMerge(total_checks) AS total,
                        countIfMerge(up_checks) AS up_
                    FROM {CH_HISTORY_MV}
                    WHERE org_id = ?
                      AND has(arrayMap(x -> toUUID(x), ?), target_id)
                      AND hour >= fromUnixTimestamp(?)
                      AND hour < fromUnixTimestamp(?)
                    GROUP BY target_id, hour
                )
                GROUP BY target_id, day
                ORDER BY target_id, day"#
            ))
            .bind(org.0)
            .bind(component_ids)
            .bind(from.timestamp())
            .bind(now.timestamp())
            .fetch_all::<HistoryDayRow>()
            .await
            .context("ch history strip")?;

        let mut out: Vec<(Uuid, Vec<DayState>)> = Vec::with_capacity(component_ids.len());
        for id in component_ids {
            let mut strip = vec![DayState::NoData; self.cfg.history_days as usize];
            for r in rows
                .iter()
                .filter(|r| r.target_id == *id && r.day_total > 0)
            {
                let day = ts_to_datetime(r.day);
                let idx = days_ago_index(day, now, self.cfg.history_days);
                if let Some(i) = idx {
                    let s = if r.any_major != 0 {
                        DayState::MajorOutage
                    } else if r.any_failure != 0 {
                        DayState::PartialOutage
                    } else {
                        DayState::Operational
                    };
                    strip[i] = s;
                }
            }
            out.push((*id, strip));
        }
        Ok(out)
    }

    /// Pulls raw `check_results` for the last 5 minutes — narrow enough that the
    /// cost is negligible (<= 5 × N rows) and gives the full
    /// up/down/degraded/error breakdown the component classifier needs.
    async fn load_recent_counters(
        &self,
        org: OrgId,
        component_ids: &[Uuid],
        now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, Counters)>> {
        if component_ids.is_empty() {
            return Ok(Vec::new());
        }
        let from = now - ChronoDuration::minutes(5);
        let rows: Vec<RecentCountRow> = self
            .ch
            .query(&format!(
                r#"SELECT
                       target_id,
                       countIf(status = 'up')       AS up_,
                       countIf(status = 'down')     AS down_,
                       countIf(status = 'degraded') AS degraded_,
                       countIf(status = 'error')    AS error_
                   FROM {CH_TABLE}
                   WHERE org_id = ?
                     AND has(arrayMap(x -> toUUID(x), ?), target_id)
                     AND timestamp >= fromUnixTimestamp(?)
                     AND timestamp <  fromUnixTimestamp(?)
                   GROUP BY target_id"#
            ))
            .bind(org.0)
            .bind(component_ids)
            .bind(from.timestamp())
            .bind(now.timestamp())
            .fetch_all::<RecentCountRow>()
            .await
            .context("ch recent counters")?;

        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.target_id,
                    Counters {
                        up: r.up_ as u32,
                        down: r.down_ as u32,
                        degraded: r.degraded_ as u32,
                        error: r.error_ as u32,
                    },
                )
            })
            .collect())
    }
}

// ── PG row types ────────────────────────────────────────────────────────────

#[derive(FromRow)]
struct PageComponentRow {
    target_id: Uuid,
    monitor_name: String,
    public_name: Option<String>,
    public_description: Option<String>,
    public_group: Option<String>,
    sort_order: i32,
}

#[derive(FromRow)]
struct MaintenanceRow {
    id: Uuid,
    title: String,
    description: Option<String>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    component_ids: Vec<Uuid>,
}

#[derive(FromRow)]
struct IncidentRow {
    id: Uuid,
    target_id: Uuid,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    severity: String,
    status_at_start: String,
    public_title: Option<String>,
    #[allow(dead_code)]
    public_description: Option<String>,
}

#[derive(FromRow)]
struct HistoryMarkerRow {
    id: Uuid,
    target_id: Uuid,
    public_title: Option<String>,
    status_at_start: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

#[derive(FromRow)]
struct IncidentUpdateRow {
    incident_id: Uuid,
    posted_at: DateTime<Utc>,
    phase: String,
    message: String,
}

// ── CH row types ────────────────────────────────────────────────────────────

#[derive(Row, Deserialize)]
struct HistoryDayRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    day: i64, // DateTime in seconds; ClickHouse `toStartOfDay` returns DateTime
    any_major: u8,
    any_failure: u8,
    day_total: u64,
}

#[derive(Row, Deserialize)]
struct RecentCountRow {
    #[serde(with = "clickhouse::serde::uuid")]
    target_id: Uuid,
    up_: u64,
    down_: u64,
    degraded_: u64,
    error_: u64,
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Cap popover title length so a runaway tenant can't blow up the inline strip
/// JSON. Snaps to the last char-boundary at or before the cap.
fn truncate_title(mut s: String) -> String {
    const MAX: usize = 140;
    if s.len() <= MAX {
        return s;
    }
    let mut end = MAX;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

fn ts_to_datetime(secs: i64) -> DateTime<Utc> {
    chrono::DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_else(Utc::now)
}

/// Maps a day timestamp to its slot in the `history_days`-long oldest-first
/// strip. Returns `None` when the day falls outside the window.
fn days_ago_index(day: DateTime<Utc>, now: DateTime<Utc>, history_days: u32) -> Option<usize> {
    let today = now.date_naive();
    let target = day.date_naive();
    let diff = (today - target).num_days();
    if diff < 0 || diff >= history_days as i64 {
        return None;
    }
    // Oldest first: idx 0 = oldest day, idx = history_days - 1 = today.
    Some((history_days as i64 - 1 - diff) as usize)
}

fn pad_or_truncate(mut v: Vec<DayState>, len: usize) -> Vec<DayState> {
    if v.len() == len {
        return v;
    }
    if v.len() > len {
        let extra = v.len() - len;
        v.drain(..extra);
        return v;
    }
    let pad = len - v.len();
    let mut out = Vec::with_capacity(len);
    out.extend(std::iter::repeat_n(DayState::NoData, pad));
    out.extend(v);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn days_ago_index_today_is_last_slot() {
        let now = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        assert_eq!(days_ago_index(now, now, 90), Some(89));
    }

    #[test]
    fn days_ago_index_oldest_in_window_is_first_slot() {
        let now = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let oldest = now - ChronoDuration::days(89);
        assert_eq!(days_ago_index(oldest, now, 90), Some(0));
    }

    #[test]
    fn days_ago_index_out_of_window_returns_none() {
        let now = Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap();
        let too_old = now - ChronoDuration::days(90);
        assert!(days_ago_index(too_old, now, 90).is_none());
        let future = now + ChronoDuration::days(1);
        assert!(days_ago_index(future, now, 90).is_none());
    }

    #[test]
    fn pad_or_truncate_pads_with_no_data_at_start() {
        let v = vec![DayState::Operational; 3];
        let out = pad_or_truncate(v, 5);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], DayState::NoData);
        assert_eq!(out[4], DayState::Operational);
    }

    #[test]
    fn pad_or_truncate_truncates_from_front() {
        let mut v = vec![DayState::NoData; 5];
        v.push(DayState::Operational);
        v.push(DayState::MajorOutage);
        let out = pad_or_truncate(v, 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], DayState::MajorOutage);
    }
}
