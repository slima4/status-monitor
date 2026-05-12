use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::CheckResult;
use crate::error::Result;
use crate::storage::{TimeRange, UptimeStats};

const RESULTS_LIMIT_DEFAULT: usize = 1_000;
const RESULTS_LIMIT_MAX: usize = 10_000;

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

impl RangeQuery {
    fn resolve(&self) -> TimeRange {
        let to = self.to.unwrap_or_else(Utc::now);
        let from = self
            .from
            .unwrap_or_else(|| to - Duration::try_hours(24).unwrap_or_default());
        TimeRange { from, to }
    }

    fn limit(&self) -> usize {
        self.limit
            .unwrap_or(RESULTS_LIMIT_DEFAULT)
            .min(RESULTS_LIMIT_MAX)
    }
}

pub async fn list_results(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Vec<CheckResult>>> {
    let range = q.resolve();
    let limit = q.limit();
    let out = state.results_store.list_results(id, range, limit).await?;
    Ok(Json(out))
}

pub async fn uptime(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<UptimeStats>> {
    let range = q.resolve();
    let stats = state.results_store.uptime(id, range).await?;
    Ok(Json(stats))
}
