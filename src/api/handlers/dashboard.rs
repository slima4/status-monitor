use std::time::{Duration as StdDuration, Instant};

use axum::Json;
use axum::extract::State;
use chrono::{Duration, Utc};

use crate::api::ApiError;
use crate::api::types::{DashboardSummary, Last24hSummary, SystemSummary};
use crate::app::AppState;
use crate::error::Result;
use crate::storage::TimeRange;

const CACHE_TTL: StdDuration = StdDuration::from_secs(5);

#[utoipa::path(
    get,
    path = "/api/v1/dashboard/summary",
    tag = "dashboard",
    summary = "Fleet-wide aggregated metrics for the operator dashboard",
    description = "Composed from ClickHouse rollups + Postgres counts + in-process gauges. Cached in-process for 5 seconds.",
    responses(
        (status = 200, body = DashboardSummary, example = json!({
            "targets": {"total": 42, "enabled": 40, "disabled": 2},
            "current_status": {"up": 38, "down": 1, "degraded": 1, "error": 0, "unknown": 2},
            "last_24h": {"checks_total": 50400, "checks_up": 50360, "uptime_pct": 99.92, "incidents": 3},
            "system": {"in_flight_checks": 5, "result_queue_depth": 12, "dropped_results_last_5m": 0, "circuit_breakers_open": 0}
        })),
        (status = 503, body = ApiError, description = "One or more data sources unavailable"),
    ),
)]
pub async fn dashboard_summary(State(state): State<AppState>) -> Result<Json<DashboardSummary>> {
    if let Some((built, snapshot)) = state.dashboard_cache.lock().clone()
        && built.elapsed() < CACHE_TTL
    {
        return Ok(Json(snapshot));
    }

    let now = Utc::now();
    let range = TimeRange {
        from: now - Duration::try_hours(24).unwrap_or_default(),
        to: now,
    };

    let (targets, current_status, (checks_total, checks_up, incidents)) = tokio::try_join!(
        state.target_store.summary(),
        state.results_store.current_status_breakdown(range),
        state.results_store.last_n_summary(range),
    )?;

    let uptime_pct = if checks_total > 0 {
        (checks_up as f64 / checks_total as f64) * 100.0
    } else {
        0.0
    };

    let summary = DashboardSummary {
        targets,
        current_status,
        last_24h: Last24hSummary {
            checks_total,
            checks_up,
            uptime_pct,
            incidents,
        },
        system: SystemSummary {
            in_flight_checks: u32::try_from(state.worker_pool.in_flight()).unwrap_or(u32::MAX),
            result_queue_depth: u32::try_from(state.worker_pool.result_queue_depth())
                .unwrap_or(u32::MAX),
            // Cumulative since process start; the field name keeps `_last_5m`
            // for response-shape stability.
            dropped_results_last_5m: state.worker_pool.dropped_results(),
            circuit_breakers_open: u32::try_from(state.worker_pool.open_breakers())
                .unwrap_or(u32::MAX),
        },
    };

    *state.dashboard_cache.lock() = Some((Instant::now(), summary.clone()));
    Ok(Json(summary))
}
