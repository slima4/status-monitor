//! Region-agent surface: config pull + result ingest. Authenticated by the
//! agent's own bearer token, which resolves to a region + agent id — never org
//! membership and never the request body.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::ad_hoc_dispatch::DeliveredResult;
use crate::api::error::codes;
use crate::app::AppState;
use crate::domain::agent_wire::{
    AgentTargetDto, AgentTargetsResponse, DispatchBatch, DispatchKind, DispatchReport,
    IngestRequest, IngestResponse,
};
use crate::error::{AppError, Result};
use crate::storage::admin::AdminRepo;
use crate::web::AgentIdentity;

/// Max interactive checks handed to one agent per long-poll return.
const DISPATCH_CLAIM_LIMIT: usize = 32;

const INGEST_MAX_BATCH: usize = 10_000;
/// Reject results timestamped further ahead than this. Past timestamps are
/// allowed — an agent buffering through a control-plane outage legitimately
/// drains older results — but a future timestamp would land in the wrong CH
/// partition and skew the TTL window.
const MAX_FUTURE_SKEW_SECS: i64 = 300;

pub async fn pull_targets(
    State(state): State<AppState>,
    headers: HeaderMap,
    agent: AgentIdentity,
) -> Result<Response> {
    let region = agent.region;
    let pool = state.require_db()?.clone();
    let repo = AdminRepo::new(pool, state.cipher.clone(), "agent_config_pull");

    // Validate the cheap etag first so an unchanged poll returns 304 without
    // decrypting any credentials.
    let etag = repo.region_pull_etag(&region).await?;
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == etag)
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }

    let targets = repo.list_enabled_targets_for_region(&region).await?;
    let body = AgentTargetsResponse {
        region,
        targets: targets
            .into_iter()
            .map(|(org, target)| AgentTargetDto {
                org_id: org.0,
                target,
            })
            .collect(),
    };
    Ok((StatusCode::OK, [(header::ETAG, etag)], Json(body)).into_response())
}

pub async fn ingest_results(
    State(state): State<AppState>,
    agent: AgentIdentity,
    Json(req): Json<IngestRequest>,
) -> Result<Response> {
    let region = &agent.region;
    if req.results.len() > INGEST_MAX_BATCH {
        return Err(AppError::unprocessable(
            codes::AGENT_BATCH_TOO_LARGE,
            format!(
                "batch of {} results exceeds the {INGEST_MAX_BATCH} cap",
                req.results.len()
            ),
        ));
    }

    // Idempotent retry: a batch_id we already committed is a no-op.
    if state.agent_ingest_dedup.get(&req.batch_id).is_some() {
        return Ok(accepted(0, 0, true));
    }
    if req.results.is_empty() {
        return Ok(accepted(0, 0, false));
    }

    let pool = state.require_db()?.clone();
    let repo = AdminRepo::new(pool, None, "agent_ingest");
    let assigned = repo.assigned_targets_for_region(region).await?;

    // Drop — never reject the whole batch on — a single bad row: a future-skewed
    // timestamp (one host with a wrong clock would otherwise poison every other
    // result) or a target not assigned to this region (anti-spoof; also covers a
    // benign reassignment race). The authoritative org_id is stamped from the
    // assignment, never the agent-supplied value.
    let total = req.results.len();
    let cutoff = chrono::Utc::now() + chrono::Duration::seconds(MAX_FUTURE_SKEW_SECS);
    let mut skewed = 0usize;
    let mut foreign = 0usize;
    let mut results = req.results;
    results.retain_mut(|r| {
        if r.timestamp > cutoff {
            skewed += 1;
            return false;
        }
        match assigned.get(&r.target_id) {
            Some(org) => {
                r.org_id = org.0;
                true
            }
            None => {
                foreign += 1;
                false
            }
        }
    });
    let dropped = skewed + foreign;
    if dropped > 0 {
        tracing::warn!(
            region = %region,
            agent_id = %agent.agent_id,
            total,
            dropped_skew = skewed,
            dropped_foreign = foreign,
            "agent ingest dropped rows"
        );
    }

    if results.is_empty() {
        // Nothing written → don't mark the batch consumed, so a resend after the
        // assignment settles is reprocessed rather than swallowed as a duplicate.
        return Ok(accepted(0, dropped, false));
    }

    state
        .result_sink
        .write_batch_tagged(&results, region, &agent.agent_id)
        .await?;
    // Mark consumed only after a successful write so a failed write stays
    // retriable.
    state.agent_ingest_dedup.insert(req.batch_id, ());
    Ok(accepted(results.len(), dropped, false))
}

fn accepted(n: usize, dropped: usize, duplicate: bool) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(IngestResponse {
            accepted: n,
            dropped,
            duplicate,
        }),
    )
        .into_response()
}

/// Long-poll for interactive checks in this agent's region: held open until a
/// check is dispatched or the hold window elapses (then returns empty and the
/// agent reconnects). Region + identity come from the bearer token.
pub async fn claim_dispatch(
    State(state): State<AppState>,
    agent: AgentIdentity,
) -> Result<Json<DispatchBatch>> {
    let claim = state.ad_hoc.claim(&agent.region, DISPATCH_CLAIM_LIMIT);
    // Return empty immediately on shutdown so a held long-poll doesn't stall
    // graceful drain for the full hold window; the agent reconnects on restart.
    let checks = match &state.shutdown {
        Some(token) => tokio::select! {
            c = claim => c,
            _ = token.cancelled() => Vec::new(),
        },
        None => claim.await,
    };
    Ok(Json(DispatchBatch { checks }))
}

/// Report the result of one check. Routes it to the waiting request; for
/// `check_now` also persists to ClickHouse under the check's authoritative org +
/// target (never the agent-supplied fields). No-op if no waiter is registered.
pub async fn submit_dispatch_result(
    State(state): State<AppState>,
    agent: AgentIdentity,
    Json(req): Json<DispatchReport>,
) -> Result<Response> {
    let result_for_ch = req.result.clone();
    let delivered = DeliveredResult {
        result: req.result,
        response_headers_preview: req.response_headers_preview,
        response_body_snippet: req.response_body_snippet,
    };
    if let Some(meta) = state.ad_hoc.complete(req.check_id, delivered)
        && meta.kind == DispatchKind::CheckNow
        && let Some(target_id) = meta.target_id
    {
        let mut result = result_for_ch;
        result.target_id = target_id;
        result.org_id = meta.org_id;
        state
            .result_sink
            .write_batch_tagged(
                std::slice::from_ref(&result),
                &agent.region,
                &agent.agent_id,
            )
            .await?;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}
