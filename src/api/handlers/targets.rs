use std::net::IpAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::AppendHeaders;
use serde::Deserialize;
use url::Host;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::page::{PageEnvelope, PageOfTarget};
use crate::api::redaction::{REDACTED, Redacted};
use crate::api::types::{
    BulkAction, BulkActionFailure, BulkActionRequest, BulkActionResponse, TestRequest, TestResponse,
};
use crate::app::AppState;
use crate::domain::{CheckResult, NewTarget, OrgId, Target, TargetAlerts, TargetUpdate};
use crate::error::{AppError, Result};
use crate::security::SsrfGuard;
use crate::storage::TargetFilter;
use crate::web::CurrentOrg;
use crate::worker::{CheckTask, host_for_spec};

const BULK_MAX: usize = 10_000;
const LIST_LIMIT_DEFAULT: usize = 50;
const LIST_LIMIT_MAX: usize = 10_000;
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Page size (default 50, max 10000).
    pub limit: Option<usize>,
    /// Page offset (default 0).
    #[serde(default)]
    pub offset: usize,
    /// Filter by exact tag match.
    pub tag: Option<String>,
    /// Filter by enabled flag.
    pub enabled: Option<bool>,
}

impl ListQuery {
    fn effective_limit(&self) -> usize {
        self.limit.unwrap_or(LIST_LIMIT_DEFAULT).min(LIST_LIMIT_MAX)
    }

    fn to_filter(&self) -> TargetFilter {
        TargetFilter {
            limit: Some(self.effective_limit()),
            offset: self.offset,
            tag: self.tag.clone(),
            enabled: self.enabled,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/targets",
    tag = "targets",
    summary = "List targets (paginated)",
    params(ListQuery),
    responses(
        (status = 200, body = PageOfTarget, example = json!({
            "items": [{
                "id": "01h7m8z4n6v0e1m7v7y6x8x8x8",
                "name": "api prod",
                "check": {"type": "http", "url": "https://example.com/healthz", "method": "GET"},
                "interval": 60,
                "enabled": true,
                "tags": ["prod"],
                "created_at": "2026-05-13T12:00:00.000Z",
                "updated_at": "2026-05-13T12:00:00.000Z"
            }],
            "total": 1, "limit": 50, "offset": 0
        })),
        (status = 400, body = ApiError),
    ),
)]
pub async fn list(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Query(query): Query<ListQuery>,
) -> Result<Redacted<PageOfTarget>> {
    let limit = query.effective_limit();
    let offset = query.offset;
    let filter = query.to_filter();
    let (items, total) = tokio::try_join!(
        state.target_store.list(org, filter.clone()),
        state.target_store.count(org, filter),
    )?;
    Ok(Redacted::new(PageEnvelope::new(
        items,
        total,
        limit as u32,
        offset as u32,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/targets/{id}",
    tag = "targets",
    summary = "Get one target",
    params(("id" = Uuid, Path, description = "Target id")),
    responses(
        (status = 200, body = Target, example = json!({
            "id": "01h7m8z4n6v0e1m7v7y6x8x8x8",
            "name": "api prod",
            "check": {"type": "http", "url": "https://example.com/healthz", "method": "GET"},
            "interval": 60,
            "enabled": true,
            "tags": ["prod"]
        })),
        (status = 404, body = ApiError, example = json!({
            "error": {"code": "TARGET_NOT_FOUND", "message": "target not found", "field": null, "details": null, "trace_id": null}
        })),
    ),
)]
pub async fn get(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
) -> Result<Redacted<Target>> {
    match state.target_store.get(org, id).await? {
        Some(t) => Ok(Redacted::new(t)),
        None => Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        )),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/targets",
    tag = "targets",
    summary = "Create a target",
    request_body(content = NewTarget, description = "Target definition", example = json!({
        "name": "api prod",
        "check": {
            "type": "http",
            "url": "https://example.com/healthz",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": true,
            "max_redirects": 5,
            "expected_status": {"kind": "exact", "value": 200},
            "headers": {},
            "verify_tls": true
        },
        "interval": 60,
        "tags": ["prod"]
    })),
    responses(
        (status = 201, description = "Created", body = Target),
        (status = 400, description = "Validation error", body = ApiError, example = json!({
            "error": {"code": "INVALID_URL_SCHEME", "message": "url scheme 'ftp' not allowed", "field": "check.url", "details": null, "trace_id": null}
        })),
        (status = 409, description = "Duplicate (if uniqueness constraint exists)", body = ApiError),
        (status = 503, body = ApiError),
    ),
)]
pub async fn create(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Json(new): Json<NewTarget>,
) -> Result<(
    StatusCode,
    AppendHeaders<[(axum::http::HeaderName, HeaderValue); 1]>,
    Redacted<Target>,
)> {
    let plan = state.quotas.limit_for_org(org).await?;
    let guard = ssrf_guard(&state);
    validate_new_target(&new, &guard, i64::from(plan.min_check_interval_secs))?;
    verify_alert_channels(&state, &new.alerts).await?;
    check_abuse(&state, org, &new.check)?;
    // Friendly pre-check; the store INSERT enforces the same cap atomically.
    state.quotas.check_can_create_targets(org, None, 1).await?;
    if new.public_status {
        state.quotas.check_public_components(org, None, 1).await?;
    }
    let t = state
        .target_store
        .create(org, new, i64::from(plan.max_targets))
        .await?;
    dispatch_first_check(&state, &t);
    // UUID hex is always ASCII-safe → infallible.
    let location = HeaderValue::from_str(&format!("/api/v1/targets/{}", t.id))
        .expect("uuid produces ascii-only path");
    Ok((
        StatusCode::CREATED,
        AppendHeaders([(header::LOCATION, location)]),
        Redacted::new(t),
    ))
}

#[utoipa::path(
    patch,
    path = "/api/v1/targets/{id}",
    tag = "targets",
    summary = "Partial update of a target",
    description = "Omit fields you don't want to change. Submitting the redaction sentinels `[\"***\",\"***\"]` or `\"***\"` for credentials returns 400 — omit the field instead to leave it unchanged.",
    params(("id" = Uuid, Path)),
    request_body(content = TargetUpdate, example = json!({
        "enabled": false,
        "tags": ["prod", "frozen"]
    })),
    responses(
        (status = 200, body = Target),
        (status = 400, description = "Validation error; includes REDACTION_SENTINEL code if `***` submitted", body = ApiError, example = json!({
            "error": {"code": "REDACTION_SENTINEL", "message": "bearer_token contains redaction sentinel", "field": "check.bearer_token", "details": null, "trace_id": null}
        })),
        (status = 404, body = ApiError),
    ),
)]
pub async fn update(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
    Json(update): Json<TargetUpdate>,
) -> Result<Redacted<Target>> {
    if let Some(check) = &update.check {
        validate_check(check, &ssrf_guard(&state))?;
        check_abuse(&state, org, check)?;
    }
    if let Some(alerts) = &update.alerts {
        validate_alerts(alerts)?;
        verify_alert_channels(&state, alerts).await?;
    }
    // The check-interval floor applies to PATCH too — otherwise a target
    // created at the floor could be lowered below it, evading the plan.
    if let Some(interval) = update.interval {
        let min = state
            .quotas
            .limit_for_org(org)
            .await?
            .min_check_interval_secs;
        let requested = interval.as_secs() as i64;
        if requested < i64::from(min) {
            return Err(AppError::min_check_interval(
                requested,
                i64::from(min),
                "free",
            ));
        }
    }
    // Flipping a private target public consumes a public-components slot.
    // create/bulk already gate this; the PATCH path must too, or the cap is
    // trivially bypassed by creating private then editing public.
    if update.public_status == Some(true)
        && let Some(existing) = state.target_store.get(org, id).await?
        && !existing.public_status
    {
        state.quotas.check_public_components(org, None, 1).await?;
    }
    match state.target_store.update(org, id, update).await? {
        Some(t) => Ok(Redacted::new(t)),
        None => Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        )),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/targets/{id}",
    tag = "targets",
    summary = "Delete a target",
    description = "Deletes target metadata. Historical results are retained until normal retention expires (90 days).",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = ApiError, example = json!({
            "error": {"code": "TARGET_NOT_FOUND", "message": "target not found", "field": null, "details": null, "trace_id": null}
        })),
    ),
)]
pub async fn delete(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    if state.target_store.delete(org, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        ))
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/targets/bulk",
    tag = "targets",
    summary = "Create up to 10 000 targets in one call",
    request_body(content = Vec<NewTarget>, example = json!([{
        "name": "api-a",
        "check": {"type": "tcp", "host": "db.example.com", "port": 5432, "timeout": 3000},
        "interval": 30
    }])),
    responses(
        (status = 201, description = "All created", body = Vec<Target>),
        (status = 400, description = "Empty payload or per-entry validation error; nothing was created", body = ApiError, example = json!({
            "error": {"code": "BULK_EMPTY", "message": "empty bulk payload", "field": null, "details": null, "trace_id": null}
        })),
        (status = 413, description = "Payload exceeds 10 000 items", body = ApiError),
        (status = 503, body = ApiError),
    ),
)]
pub async fn bulk_create(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Json(items): Json<Vec<NewTarget>>,
) -> Result<(StatusCode, Redacted<Vec<Target>>)> {
    if items.is_empty() {
        return Err(AppError::bad_request(
            codes::BULK_EMPTY,
            "empty bulk payload",
        ));
    }
    if items.len() > BULK_MAX {
        return Err(AppError::payload_too_large(
            codes::BULK_TOO_LARGE,
            format!("bulk size {} exceeds max {BULK_MAX}", items.len()),
        ));
    }
    let plan = state.quotas.limit_for_org(org).await?;
    let guard = ssrf_guard(&state);
    for new in &items {
        validate_new_target(new, &guard, i64::from(plan.min_check_interval_secs))?;
        verify_alert_channels(&state, &new.alerts).await?;
        check_abuse(&state, org, &new.check)?;
    }
    let n = items.len() as i64;
    // Quantity-aware friendly pre-check; the store INSERT re-enforces the
    // same `current + n <= limit` bound atomically against a concurrent bulk.
    state.quotas.check_can_create_targets(org, None, n).await?;
    let public = items.iter().filter(|t| t.public_status).count() as i64;
    if public > 0 {
        state
            .quotas
            .check_public_components(org, None, public)
            .await?;
    }
    let out = state
        .target_store
        .bulk_create(org, items, i64::from(plan.max_targets))
        .await?;
    Ok((StatusCode::CREATED, Redacted::new(out)))
}

#[utoipa::path(
    post,
    path = "/api/v1/targets/bulk-action",
    tag = "targets",
    summary = "Apply enable/disable/delete/tag-add/tag-remove to many targets",
    description = "Partial failure is allowed — the response lists which ids succeeded and which failed and why. Up to 10 000 ids per request.",
    request_body(content = BulkActionRequest, example = json!({
        "ids": ["01h7m8z4n6v0e1m7v7y6x8x8x8"],
        "action": {"type": "disable"}
    })),
    responses(
        (status = 200, body = BulkActionResponse, example = json!({
            "succeeded": ["01h7m8z4n6v0e1m7v7y6x8x8x8"],
            "failed": []
        })),
        (status = 400, description = "Malformed request (e.g., empty ids, unknown action)", body = ApiError),
        (status = 413, body = ApiError),
    ),
)]
pub async fn bulk_action(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Json(req): Json<BulkActionRequest>,
) -> Result<Json<BulkActionResponse>> {
    if req.ids.is_empty() {
        return Err(AppError::bad_request(
            codes::BULK_EMPTY,
            "bulk-action requires at least one id",
        ));
    }
    if req.ids.len() > BULK_MAX {
        return Err(AppError::payload_too_large(
            codes::BULK_TOO_LARGE,
            format!("bulk size {} exceeds max {BULK_MAX}", req.ids.len()),
        ));
    }

    let succeeded = match &req.action {
        BulkAction::Enable => state.target_store.set_enabled(org, &req.ids, true).await?,
        BulkAction::Disable => state.target_store.set_enabled(org, &req.ids, false).await?,
        BulkAction::Delete => state.target_store.delete_bulk(org, &req.ids).await?,
        BulkAction::TagAdd { tags } => {
            if tags.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_ALERT_CONFIG,
                    "tag_add requires at least one tag",
                    "action.tags",
                ));
            }
            state.target_store.add_tags(org, &req.ids, tags).await?
        }
        BulkAction::TagRemove { tags } => {
            if tags.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_ALERT_CONFIG,
                    "tag_remove requires at least one tag",
                    "action.tags",
                ));
            }
            state.target_store.remove_tags(org, &req.ids, tags).await?
        }
    };

    let failed: Vec<BulkActionFailure> = req
        .ids
        .iter()
        .filter(|id| !succeeded.contains(id))
        .map(|id| BulkActionFailure {
            id: *id,
            code: codes::TARGET_NOT_FOUND,
            message: "target not found".into(),
        })
        .collect();

    Ok(Json(BulkActionResponse { succeeded, failed }))
}

#[utoipa::path(
    post,
    path = "/api/v1/targets/test",
    tag = "targets",
    summary = "Run a one-shot check against a CheckSpec without persisting anything",
    description = "Used by the UI's 'Test now' button on create/edit forms. Runs through the same validation (SSRF, schema) as create. Result is not stored.",
    request_body(content = TestRequest, example = json!({
        "check": {
            "type": "http",
            "url": "https://example.com/",
            "method": "GET",
            "timeout": 5000,
            "follow_redirects": true,
            "max_redirects": 5,
            "expected_status": {"kind": "exact", "value": 200},
            "headers": {},
            "verify_tls": true
        }
    })),
    responses(
        (status = 200, body = TestResponse, example = json!({
            "result": {"target_id": "00000000-0000-0000-0000-000000000000", "timestamp": "2026-05-13T12:00:00.000Z", "status": "up", "duration_ms": 142},
            "matched_expectations": true,
            "warnings": []
        })),
        (status = 400, body = ApiError),
    ),
)]
pub async fn test_check(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Json(req): Json<TestRequest>,
) -> Result<Json<TestResponse>> {
    let guard = ssrf_guard(&state);
    validate_check(&req.check, &guard)?;
    check_abuse(&state, org, &req.check)?;
    let result = crate::worker::execute(Uuid::nil(), &req.check, &state.http_clients).await;
    let matched_expectations = matches!(result.status, crate::domain::CheckStatus::Up);
    Ok(Json(TestResponse {
        matched_expectations,
        result,
        warnings: Vec::new(),
    }))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CheckNowQuery {
    /// Bypass circuit breaker.
    #[serde(default)]
    pub force: bool,
}

#[utoipa::path(
    post,
    path = "/api/v1/targets/{id}/check-now",
    tag = "targets",
    summary = "Run an immediate check against an existing target",
    description = "Uses the target's stored (un-redacted) credentials. Result IS persisted, same as a scheduled check. Returns 422 if the target's circuit breaker is currently open (use ?force=true to bypass).",
    params(
        ("id" = Uuid, Path),
        CheckNowQuery,
    ),
    responses(
        (status = 200, body = CheckResult, example = json!({
            "target_id": "01h7m8z4n6v0e1m7v7y6x8x8x8",
            "timestamp": "2026-05-13T12:00:00.000Z",
            "status": "up",
            "duration_ms": 142,
            "response_code": 200
        })),
        (status = 404, body = ApiError),
        (status = 422, description = "Circuit breaker open", body = ApiError, example = json!({
            "error": {"code": "CIRCUIT_OPEN", "message": "circuit breaker is open for host 'example.com'; retry with ?force=true to bypass", "field": null, "details": null, "trace_id": null}
        })),
    ),
)]
pub async fn check_now(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
    Query(q): Query<CheckNowQuery>,
) -> Result<Json<CheckResult>> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "target not found"))?;
    let host = host_for_spec(&target.check);
    let result = state
        .worker_pool
        .run_once(target.id, &target.check, &host, q.force)
        .await
        .ok_or_else(|| {
            AppError::unprocessable(
                codes::CIRCUIT_OPEN,
                format!(
                    "circuit breaker is open for host '{host}'; retry with ?force=true to bypass"
                ),
            )
        })?;
    state
        .result_sink
        .write_batch(std::slice::from_ref(&result))
        .await?;
    Ok(Json(result))
}

/// Dispatch one immediate check for a just-created target so the UI shows a
/// status within seconds instead of waiting up to a full registry-refresh
/// interval for the scheduler to pick the target up. Runs through the normal
/// worker pool — semaphore-bounded and persisted via the same fan-out as
/// scheduled checks — so if the pool is saturated the check is dropped and
/// the scheduler's own pickup covers it.
fn dispatch_first_check(state: &AppState, target: &Target) {
    if !target.enabled {
        return;
    }
    state.worker_pool.dispatch(CheckTask {
        target: Arc::new(target.clone()),
    });
}

fn ssrf_guard(state: &AppState) -> SsrfGuard {
    SsrfGuard::new(state.cfg.security.allow_private_targets)
}

/// Abuse admission control for one user-supplied check. Every handler that
/// accepts a `CheckSpec` (create, bulk per item, update, test) routes through
/// this single chokepoint, so a denylisted URL/domain can never enter the
/// store — and every block is audited fire-and-forget to `quota_events`.
fn check_abuse(state: &AppState, org: OrgId, check: &crate::domain::CheckSpec) -> Result<()> {
    let Some(hit) = state.abuse.inspect(check) else {
        return Ok(());
    };
    crate::quotas::service::record_quota_event(
        state.db.clone(),
        Some(org),
        None,
        "abuse_blocked",
        Some(hit.quota_name()),
        serde_json::json!({ "detail": hit.detail }),
        None,
    );
    Err(hit.into_app_error())
}

/// Per-resource validation, including the plan's check-interval floor. Both
/// `create` and `bulk_create` run this per item, so the floor is enforced by
/// construction on every path rather than in one handler (I4).
fn validate_new_target(new: &NewTarget, guard: &SsrfGuard, min_interval_secs: i64) -> Result<()> {
    let requested = new.interval.as_secs() as i64;
    if requested < min_interval_secs {
        return Err(AppError::min_check_interval(
            requested,
            min_interval_secs,
            "free",
        ));
    }
    validate_check(&new.check, guard)?;
    validate_alerts(&new.alerts)
}

/// Structural-only (no I/O): each binding's `after_failures` floor. The bound
/// `channel_id` is checked to exist in the caller's org by the async
/// [`verify_alert_channels`] — kept separate so the sync per-item path
/// (`validate_new_target`, also used by bulk) stays sync.
fn validate_alerts(alerts: &TargetAlerts) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for (i, b) in alerts.iter().enumerate() {
        if b.after_failures == 0 {
            return Err(AppError::bad_request_field(
                codes::INVALID_ALERT_CONFIG,
                format!("alerts[{i}]: after_failures must be >= 1"),
                format!("alerts[{i}].after_failures"),
            ));
        }
        // Two bindings to the same channel share one engine AlertState, so
        // the second would silently override the first's policy. Reject it
        // for predictable behaviour.
        if !seen.insert(b.channel_id) {
            return Err(AppError::bad_request_field(
                codes::INVALID_ALERT_CONFIG,
                format!(
                    "alerts[{i}]: duplicate binding for channel {}",
                    b.channel_id
                ),
                format!("alerts[{i}].channel_id"),
            ));
        }
    }
    Ok(())
}

/// Reject a binding to a channel the caller's org doesn't own (the store is
/// org-scoped, so a foreign or deleted id resolves to `None`). Closes the
/// IDOR where a target could otherwise reference another tenant's channel.
async fn verify_alert_channels(state: &AppState, alerts: &TargetAlerts) -> Result<()> {
    if alerts.is_empty() {
        return Ok(());
    }
    // One batched org-scoped query (mirrors maintenance's
    // `validate_component_ids`) instead of N point lookups.
    let ids: Vec<uuid::Uuid> = alerts.iter().map(|b| b.channel_id).collect();
    let known = state
        .notification_channel_store
        .existing_channel_ids(&ids)
        .await?;
    if let Some(missing) = ids.iter().find(|id| !known.contains(id)) {
        return Err(AppError::bad_request_field(
            codes::INVALID_ALERT_CONFIG,
            format!("notification channel {missing} does not exist"),
            "alerts.channel_id",
        ));
    }
    Ok(())
}

fn validate_check(check: &crate::domain::CheckSpec, guard: &SsrfGuard) -> Result<()> {
    use crate::domain::CheckSpec;
    match check {
        CheckSpec::Http(http) => {
            let scheme = http.url.scheme();
            if !ALLOWED_SCHEMES.contains(&scheme) {
                return Err(AppError::bad_request_field(
                    codes::INVALID_URL_SCHEME,
                    format!("url scheme '{scheme}' not allowed"),
                    "check.url",
                ));
            }
            if let Some((u, p)) = &http.basic_auth
                && (u == REDACTED || p == REDACTED)
            {
                return Err(AppError::bad_request_field(
                    codes::REDACTION_SENTINEL,
                    "basic_auth contains redaction sentinel — re-supply the real credential",
                    "check.basic_auth",
                ));
            }
            if http.bearer_token.as_deref() == Some(REDACTED) {
                return Err(AppError::bad_request_field(
                    codes::REDACTION_SENTINEL,
                    "bearer_token contains redaction sentinel — re-supply the real credential",
                    "check.bearer_token",
                ));
            }
            // Plain http already sends creds in the clear, so this rule only
            // protects the https + forged-cert MITM path that bypasses the
            // confidentiality the operator was relying on.
            if !http.verify_tls
                && scheme == "https"
                && (http.basic_auth.is_some() || http.bearer_token.is_some())
            {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TLS_CRED_COMBO,
                    "verify_tls = false cannot be combined with basic_auth or bearer_token over https — credentials would be exposed to any host presenting a forged certificate",
                    "check.verify_tls",
                ));
            }
            match http.url.host() {
                Some(Host::Ipv4(v4)) => check_ip(IpAddr::V4(v4), guard)?,
                Some(Host::Ipv6(v6)) => check_ip(IpAddr::V6(v6), guard)?,
                Some(Host::Domain("")) => {
                    return Err(AppError::bad_request_field(
                        codes::INVALID_URL_FORMAT,
                        "url missing host",
                        "check.url",
                    ));
                }
                Some(Host::Domain(_)) => {}
                None => {
                    return Err(AppError::bad_request_field(
                        codes::INVALID_URL_FORMAT,
                        "url missing host",
                        "check.url",
                    ));
                }
            }
        }
        CheckSpec::Tcp(tcp) => {
            if tcp.host.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TCP_HOST,
                    "tcp host required",
                    "check.host",
                ));
            }
            if tcp.port == 0 {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TCP_PORT,
                    "tcp port must be > 0",
                    "check.port",
                ));
            }
            let host = crate::security::unbracket(&tcp.host);
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_ip(ip, guard)?;
            }
        }
        CheckSpec::TlsCert(cert) => {
            if cert.host.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TLS_CERT_PARAMS,
                    "tls_cert host required",
                    "check.host",
                ));
            }
            if cert.port == 0 {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TLS_CERT_PARAMS,
                    "tls_cert port must be > 0",
                    "check.port",
                ));
            }
            if cert.warn_days <= cert.critical_days {
                return Err(AppError::bad_request_field(
                    codes::INVALID_TLS_CERT_PARAMS,
                    "tls_cert warn_days must be > critical_days",
                    "check.warn_days",
                ));
            }
            let host = crate::security::unbracket(&cert.host);
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_ip(ip, guard)?;
            }
        }
        CheckSpec::DomainExpiry(d) => {
            if d.domain.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DOMAIN_PARAMS,
                    "domain_expiry domain required",
                    "check.domain",
                ));
            }
            // Require at least one non-empty label on each side of the final
            // dot — rejects degenerate inputs like ".", ".a", "a." that would
            // pass a naive `.contains('.')` gate.
            let well_formed = d
                .domain
                .rsplit_once('.')
                .is_some_and(|(label, tld)| !label.is_empty() && !tld.is_empty());
            if !well_formed {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DOMAIN_PARAMS,
                    "domain_expiry domain must be of the form 'name.tld'",
                    "check.domain",
                ));
            }
            if d.warn_days <= d.critical_days {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DOMAIN_PARAMS,
                    "domain_expiry warn_days must be > critical_days",
                    "check.warn_days",
                ));
            }
        }
    }
    Ok(())
}

fn check_ip(ip: IpAddr, guard: &SsrfGuard) -> Result<()> {
    guard.check(ip).map_err(|err| {
        AppError::bad_request_field(codes::SSRF_BLOCKED, err.to_string(), "check.url")
    })
}
