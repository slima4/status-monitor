//! Trait + production impl for the data layer behind the public status API.
//!
//! The trait shape isolates handlers from the concrete storage backend so
//! tests can substitute a deterministic fake without spinning up Postgres or
//! ClickHouse, and so the handler code never reaches across into private
//! storage types.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::api::PageEnvelope;
use crate::api::public_error::PublicAppError;
use crate::domain::{
    ComponentHistoryResponse, IncidentSeverity, IncidentStatusPhase, OrgId, PublicIncident,
    PublicIncidentUpdate, PublicMaintenanceList, PublicStatusPage,
};

use super::aggregator::OrgAggregator;
use super::cache::{PageCache, PageCacheError};
use super::xml::xml_escape;

#[derive(Debug, Clone, Copy)]
pub struct IncidentListQuery {
    pub limit: u32,
    pub offset: u32,
    pub ongoing_only: bool,
}

impl Default for IncidentListQuery {
    fn default() -> Self {
        Self {
            limit: 25,
            offset: 0,
            ongoing_only: false,
        }
    }
}

/// Per-org public-status data layer. Every method takes the target `OrgId`
/// as its first parameter — there is no implicit default. The compiler
/// refuses to compile a handler that forgot which tenant's data is being
/// served, which is what prevents the cache from quietly serving one
/// tenant's page to another.
#[async_trait]
pub trait PublicSource: Send + Sync {
    async fn page(&self, org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError>;
    async fn component_history(
        &self,
        org: OrgId,
        id: Uuid,
        days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError>;
    async fn list_incidents(
        &self,
        org: OrgId,
        q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError>;
    async fn incident_by_id(&self, org: OrgId, id: Uuid) -> Result<PublicIncident, PublicAppError>;
    async fn maintenance(&self, org: OrgId) -> Result<PublicMaintenanceList, PublicAppError>;
    async fn incidents_rss(&self, org: OrgId, base_url: &str) -> Result<String, PublicAppError>;

    /// Drop any cached page for `org`. The settings handler calls this when
    /// `public_status_enabled` flips to `false` so the now-disabled org
    /// can't keep serving a cached page past TTL. Default no-op: backends
    /// without a cache (the test/noop source) have nothing to drop.
    async fn invalidate(&self, _org: OrgId) {}
}

pub struct OrgPublicSource {
    aggregator: Arc<OrgAggregator>,
    cache: PageCache,
    pg: PgPool,
    rss_lookback_days: u32,
    rss_max_items: u32,
    site_name: String,
}

impl OrgPublicSource {
    pub fn new(
        aggregator: Arc<OrgAggregator>,
        cache: PageCache,
        pg: PgPool,
        site_name: impl Into<String>,
    ) -> Self {
        Self {
            aggregator,
            cache,
            pg,
            rss_lookback_days: 90,
            rss_max_items: 50,
            site_name: site_name.into(),
        }
    }
}

#[async_trait]
impl PublicSource for OrgPublicSource {
    async fn page(&self, org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        let agg = self.aggregator.clone();
        let res = self
            .cache
            .get_or_compute(org, move || async move { agg.build(org).await })
            .await;
        match res {
            Ok(page) => Ok(page),
            Err(PageCacheError::Unavailable) => Err(PublicAppError::Unavailable),
        }
    }

    async fn component_history(
        &self,
        org: OrgId,
        id: Uuid,
        days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        if !(1..=365).contains(&days) {
            return Err(PublicAppError::InvalidDays);
        }
        self.aggregator
            .component_history(org, id, days)
            .await
            .map_err(|_| PublicAppError::NotFound)
    }

    async fn list_incidents(
        &self,
        org: OrgId,
        q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        let since = Utc::now() - ChronoDuration::days(self.rss_lookback_days as i64);
        let limit = q.limit.clamp(1, 100) as i64;
        let offset = q.offset as i64;
        let ongoing_only = q.ongoing_only;

        let rows: Vec<IncidentRow> = sqlx::query_as::<_, IncidentRow>(
            r#"SELECT i.id, i.target_id,
                      COALESCE(t.public_name, t.name) AS component_name,
                      i.started_at, i.ended_at, i.severity, i.status_at_start,
                      i.public_title, i.public_description
               FROM incidents i
               JOIN targets t ON t.id = i.target_id
               WHERE i.org_id = $5
                 AND t.org_id = $5
                 AND t.public_status = true
                 AND i.started_at >= $1
                 AND ($2 = false OR i.ended_at IS NULL)
               ORDER BY i.started_at DESC
               LIMIT $3 OFFSET $4"#,
        )
        .bind(since)
        .bind(ongoing_only)
        .bind(limit)
        .bind(offset)
        .bind(org.0)
        .fetch_all(&self.pg)
        .await
        .context("public list incidents")
        .map_err(PublicAppError::Internal)?;

        let total_row: (i64,) = sqlx::query_as(
            r#"SELECT count(*)
               FROM incidents i
               JOIN targets t ON t.id = i.target_id
               WHERE i.org_id = $3
                 AND t.org_id = $3
                 AND t.public_status = true
                 AND i.started_at >= $1
                 AND ($2 = false OR i.ended_at IS NULL)"#,
        )
        .bind(since)
        .bind(ongoing_only)
        .bind(org.0)
        .fetch_one(&self.pg)
        .await
        .context("public count incidents")
        .map_err(PublicAppError::Internal)?;

        let incidents = self.hydrate(org, rows).await?;
        Ok(PageEnvelope::new(
            incidents,
            total_row.0.max(0) as u64,
            q.limit,
            q.offset,
        ))
    }

    async fn incident_by_id(&self, org: OrgId, id: Uuid) -> Result<PublicIncident, PublicAppError> {
        let row: Option<IncidentRow> = sqlx::query_as::<_, IncidentRow>(
            r#"SELECT i.id, i.target_id,
                      COALESCE(t.public_name, t.name) AS component_name,
                      i.started_at, i.ended_at, i.severity, i.status_at_start,
                      i.public_title, i.public_description
               FROM incidents i
               JOIN targets t ON t.id = i.target_id
               WHERE i.id = $1
                 AND i.org_id = $2
                 AND t.org_id = $2
                 AND t.public_status = true"#,
        )
        .bind(id)
        .bind(org.0)
        .fetch_optional(&self.pg)
        .await
        .context("public get incident")
        .map_err(PublicAppError::Internal)?;

        let row = row.ok_or(PublicAppError::NotFound)?;
        let mut hydrated = self.hydrate(org, vec![row]).await?;
        hydrated.pop().ok_or(PublicAppError::NotFound)
    }

    async fn maintenance(&self, org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        let page = self.page(org).await?;
        Ok(PublicMaintenanceList {
            active: page.active_maintenance.clone(),
            upcoming: page.upcoming_maintenance.clone(),
        })
    }

    async fn incidents_rss(&self, org: OrgId, base_url: &str) -> Result<String, PublicAppError> {
        let q = IncidentListQuery {
            limit: self.rss_max_items,
            offset: 0,
            ongoing_only: false,
        };
        let page = self.list_incidents(org, q).await?;
        Ok(build_rss(&self.site_name, base_url, &page.items))
    }

    async fn invalidate(&self, org: OrgId) {
        self.cache.invalidate(org).await;
    }
}

impl OrgPublicSource {
    async fn hydrate(
        &self,
        org: OrgId,
        rows: Vec<IncidentRow>,
    ) -> Result<Vec<PublicIncident>, PublicAppError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let updates: Vec<UpdateRow> = sqlx::query_as::<_, UpdateRow>(
            r#"SELECT incident_id, posted_at, phase, message
               FROM incident_updates
               WHERE incident_id = ANY($1) AND org_id = $2
               ORDER BY incident_id, posted_at ASC"#,
        )
        .bind(&ids)
        .bind(org.0)
        .fetch_all(&self.pg)
        .await
        .context("public list incident updates")
        .map_err(PublicAppError::Internal)?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let mut my_updates: Vec<PublicIncidentUpdate> = updates
                    .iter()
                    .filter(|u| u.incident_id == r.id)
                    .map(|u| PublicIncidentUpdate {
                        posted_at: u.posted_at,
                        phase: IncidentStatusPhase::from_db_str(&u.phase),
                        message: u.message.clone(),
                    })
                    .collect();
                my_updates.sort_by_key(|u| u.posted_at);
                let status_phase = my_updates
                    .last()
                    .map(|u| u.phase)
                    .unwrap_or(IncidentStatusPhase::Investigating);
                let title = r
                    .public_title
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", r.component_name, r.status_at_start));
                PublicIncident {
                    id: r.id,
                    component_id: r.target_id,
                    component_name: r.component_name,
                    title,
                    started_at: r.started_at,
                    ended_at: r.ended_at,
                    severity: IncidentSeverity::from_db_str(&r.severity),
                    status_phase,
                    updates: my_updates,
                }
            })
            .collect())
    }
}

#[derive(FromRow)]
struct IncidentRow {
    id: Uuid,
    target_id: Uuid,
    component_name: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    severity: String,
    status_at_start: String,
    public_title: Option<String>,
    #[allow(dead_code)]
    public_description: Option<String>,
}

#[derive(FromRow)]
struct UpdateRow {
    incident_id: Uuid,
    posted_at: DateTime<Utc>,
    phase: String,
    message: String,
}

/// Minimal RSS 2.0 builder — keeps us free of an extra crate just to emit
/// a few dozen lines of XML. Items are the most recent public incidents.
pub fn build_rss(site_name: &str, base_url: &str, items: &[PublicIncident]) -> String {
    let now = Utc::now().to_rfc2822();
    let mut out = String::with_capacity(512 + items.len() * 256);
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push_str("\n<rss version=\"2.0\"><channel>");
    out.push_str(&format!(
        "<title>{}</title><link>{}</link><description>Operational status</description><lastBuildDate>{}</lastBuildDate>",
        xml_escape(&format!("{site_name} Status Incidents")),
        xml_escape(base_url),
        now,
    ));
    for i in items {
        let pub_date = i
            .updates
            .last()
            .map(|u| u.posted_at)
            .unwrap_or(i.started_at)
            .to_rfc2822();
        let body: String = i
            .updates
            .iter()
            .map(|u| format!("[{}] {}", phase_label(u.phase), u.message))
            .collect::<Vec<_>>()
            .join(" \n");
        out.push_str("<item>");
        out.push_str(&format!("<title>{}</title>", xml_escape(&i.title)));
        out.push_str(&format!("<guid isPermaLink=\"false\">{}</guid>", i.id));
        out.push_str(&format!(
            "<link>{}/status/incidents/{}</link>",
            xml_escape(base_url),
            i.id
        ));
        out.push_str(&format!("<pubDate>{pub_date}</pubDate>"));
        out.push_str(&format!("<description>{}</description>", xml_escape(&body)));
        out.push_str("</item>");
    }
    out.push_str("</channel></rss>");
    out
}

fn phase_label(p: IncidentStatusPhase) -> &'static str {
    match p {
        IncidentStatusPhase::Investigating => "investigating",
        IncidentStatusPhase::Identified => "identified",
        IncidentStatusPhase::Monitoring => "monitoring",
        IncidentStatusPhase::Resolved => "resolved",
        IncidentStatusPhase::Postmortem => "postmortem",
    }
}

/// A `PublicSource` that returns an empty page and `NotFound` for everything
/// else. Useful as a placeholder in test rigs that don't exercise the public
/// surface, and as a safe fallback for environments without ClickHouse.
pub struct NoopPublicSource {
    site_name: String,
}

impl NoopPublicSource {
    pub fn new(site_name: impl Into<String>) -> Self {
        Self {
            site_name: site_name.into(),
        }
    }
}

impl Default for NoopPublicSource {
    fn default() -> Self {
        Self::new("status-monitor")
    }
}

#[async_trait]
impl PublicSource for NoopPublicSource {
    async fn page(&self, _org: OrgId) -> Result<Arc<PublicStatusPage>, PublicAppError> {
        Ok(Arc::new(PublicStatusPage {
            overall: crate::domain::OverallStatus {
                state: crate::domain::OverallState::Operational,
                label: "All Systems Operational".into(),
            },
            generated_at: Utc::now(),
            site_name: self.site_name.clone(),
            groups: Vec::new(),
            active_incidents: Vec::new(),
            recent_incidents: Vec::new(),
            active_maintenance: Vec::new(),
            upcoming_maintenance: Vec::new(),
        }))
    }

    async fn component_history(
        &self,
        _org: OrgId,
        _id: Uuid,
        _days: u32,
    ) -> Result<ComponentHistoryResponse, PublicAppError> {
        Err(PublicAppError::NotFound)
    }

    async fn list_incidents(
        &self,
        _org: OrgId,
        q: IncidentListQuery,
    ) -> Result<PageEnvelope<PublicIncident>, PublicAppError> {
        Ok(PageEnvelope::new(Vec::new(), 0, q.limit, q.offset))
    }

    async fn incident_by_id(
        &self,
        _org: OrgId,
        _id: Uuid,
    ) -> Result<PublicIncident, PublicAppError> {
        Err(PublicAppError::NotFound)
    }

    async fn maintenance(&self, _org: OrgId) -> Result<PublicMaintenanceList, PublicAppError> {
        Ok(PublicMaintenanceList {
            active: Vec::new(),
            upcoming: Vec::new(),
        })
    }

    async fn incidents_rss(&self, _org: OrgId, base_url: &str) -> Result<String, PublicAppError> {
        Ok(build_rss(&self.site_name, base_url, &[]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_skeleton_well_formed_with_no_items() {
        let xml = build_rss("Site", "https://example.com", &[]);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains("<rss version=\"2.0\""));
        assert!(xml.contains("<channel>"));
        assert!(xml.contains("</channel></rss>"));
        assert!(xml.contains("Site Status Incidents"));
    }
}
