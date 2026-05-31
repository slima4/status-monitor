use std::net::IpAddr;

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
use crate::auth::scope::Scope;
use crate::domain::{CheckResult, NewTarget, OrgId, Target, TargetAlerts, TargetUpdate};
use crate::error::{AppError, Result};
use crate::security::SsrfGuard;
use crate::storage::TargetFilter;
use crate::web::{
    Authorized, CurrentOrg, RequestSource, TargetsDelete, TargetsExecute, TargetsRead,
    TargetsWrite, TokenScopes,
};
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
            ..Default::default()
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
            "limit": 50, "offset": 0, "has_more": false
        })),
        (status = 400, body = ApiError),
    ),
)]
pub async fn list(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<TargetsRead>,
    Query(query): Query<ListQuery>,
) -> Result<Redacted<PageOfTarget>> {
    let limit = query.effective_limit();
    let offset = query.offset;
    let filter = query.to_filter();
    // The filter struct owns its limit/offset; bump the limit by 1 to peek
    // a row past the page boundary and let the envelope decide has_more.
    let peek_filter = TargetFilter {
        limit: filter.limit.map(|n| n + 1),
        ..filter
    };
    let peek = state.target_store.list(org, peek_filter).await?;
    Ok(Redacted::new(PageEnvelope::from_peek(
        peek,
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
    Authorized(org, _): Authorized<TargetsRead>,
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
    Authorized(org, _): Authorized<TargetsWrite>,
    RequestSource(source): RequestSource,
    Json(mut new): Json<NewTarget>,
) -> Result<(
    StatusCode,
    AppendHeaders<[(axum::http::HeaderName, HeaderValue); 1]>,
    Redacted<Target>,
)> {
    let plan = state.quotas.limit_for_org(org).await?;
    let guard = ssrf_guard(&state);
    canonicalize_check(&mut new.check)?;
    validate_new_target(&new, &guard, i64::from(plan.min_check_interval_secs))?;
    verify_alert_channels(&state, org, &new.alerts).await?;
    validate_owner_is_member(&state, org, new.owner_user_id).await?;
    check_abuse(&state, org, &new.check)?;
    // Friendly pre-check; the store INSERT enforces the same cap atomically.
    state.quotas.check_can_create_targets(org, None, 1).await?;
    if new.public_status {
        state.quotas.check_public_components(org, None, 1).await?;
    }
    let t = state
        .target_store
        .create(org, new, source, i64::from(plan.max_targets))
        .await?;
    if t.public_status {
        state.public_source.invalidate(org).await;
    }
    dispatch_first_check(&state, org, &t);
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
    Authorized(org, _): Authorized<TargetsWrite>,
    RequestSource(source): RequestSource,
    Path(id): Path<Uuid>,
    Json(mut update): Json<TargetUpdate>,
) -> Result<Redacted<Target>> {
    if let Some(check) = update.check.as_mut() {
        canonicalize_check(check)?;
        validate_check(check, &ssrf_guard(&state))?;
        check_abuse(&state, org, check)?;
    }
    if let Some(alerts) = &update.alerts {
        validate_alerts(alerts)?;
        verify_alert_channels(&state, org, alerts).await?;
    }
    validate_public_target_field_updates(
        update.public_name.as_ref(),
        update.public_description.as_ref(),
        update.public_group.as_ref(),
    )?;
    if let Some(Some(g)) = update.group_name.as_ref() {
        validate_group_name(Some(g.as_str()))?;
    }
    if let Some(Some(uid)) = update.owner_user_id {
        validate_owner_is_member(&state, org, Some(uid)).await?;
    }
    if let Some(interval) = update.interval {
        let plan_min = i64::from(
            state
                .quotas
                .limit_for_org(org)
                .await?
                .min_check_interval_secs,
        );
        let requested = interval.as_secs() as i64;
        let kind_floor = if requested >= MAX_KIND_FLOOR_SECS {
            0
        } else {
            let kind = match update.check.as_ref() {
                Some(c) => c.kind(),
                None => state
                    .target_store
                    .get(org, id)
                    .await?
                    .map(|t| t.check.kind())
                    .unwrap_or("http"),
            };
            min_interval_secs_for_kind(kind) as i64
        };
        let effective_floor = plan_min.max(kind_floor);
        if requested < effective_floor {
            return Err(AppError::min_check_interval(
                requested,
                effective_floor,
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
    let touches_public = touches_public_view(&update);
    match state.target_store.update(org, id, update, source).await? {
        Some(t) => {
            if touches_public || t.public_status {
                state.public_source.invalidate(org).await;
            }
            Ok(Redacted::new(t))
        }
        None => Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        )),
    }
}

fn touches_public_view(u: &TargetUpdate) -> bool {
    u.public_status.is_some()
        || u.public_name.is_some()
        || u.public_description.is_some()
        || u.public_group.is_some()
        || u.public_sort_order.is_some()
        || u.name.is_some()
        || u.group_name.is_some()
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
    Authorized(org, _): Authorized<TargetsDelete>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    if state.target_store.delete(org, id).await? {
        state.public_source.invalidate(org).await;
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
    Authorized(org, _): Authorized<TargetsWrite>,
    RequestSource(source): RequestSource,
    Json(mut items): Json<Vec<NewTarget>>,
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
    for new in &mut items {
        canonicalize_check(&mut new.check)?;
        validate_new_target(new, &guard, i64::from(plan.min_check_interval_secs))?;
        verify_alert_channels(&state, org, &new.alerts).await?;
        check_abuse(&state, org, &new.check)?;
    }
    let owner_ids: std::collections::HashSet<Uuid> =
        items.iter().filter_map(|t| t.owner_user_id).collect();
    if !owner_ids.is_empty() {
        let pool = state.require_db()?;
        let members = crate::storage::orgs::list_members(pool, org).await?;
        let member_set: std::collections::HashSet<Uuid> =
            members.iter().map(|m| m.membership.user_id.0).collect();
        for uid in owner_ids {
            if !member_set.contains(&uid) {
                return Err(AppError::bad_request_field(
                    codes::OWNER_NOT_MEMBER,
                    format!("owner_user_id {uid} is not a member of this org"),
                    "owner_user_id",
                ));
            }
        }
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
        .bulk_create(org, items, source, i64::from(plan.max_targets))
        .await?;
    if out.iter().any(|t| t.public_status) {
        state.public_source.invalidate(org).await;
    }
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
    scopes: TokenScopes,
    Json(req): Json<BulkActionRequest>,
) -> Result<Json<BulkActionResponse>> {
    scopes.require(match &req.action {
        BulkAction::Delete => Scope::TargetsDelete,
        _ => Scope::TargetsWrite,
    })?;
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

    let action_touches_public = matches!(
        &req.action,
        BulkAction::Delete | BulkAction::SetGroup { .. }
    );
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
        BulkAction::SetGroup { group } => {
            // Trim + treat "" as clear so the wire format stays one shape
            // (omit field = no-op, send "" or null = clear).
            let normalized = group.as_deref().map(str::trim).filter(|s| !s.is_empty());
            state
                .target_store
                .set_group(org, &req.ids, normalized)
                .await?
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

    if action_touches_public && !succeeded.is_empty() {
        state.public_source.invalidate(org).await;
    }

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
    Authorized(org, _): Authorized<TargetsExecute>,
    Json(mut req): Json<TestRequest>,
) -> Result<Json<TestResponse>> {
    let guard = ssrf_guard(&state);
    canonicalize_check(&mut req.check)?;
    validate_check(&req.check, &guard)?;
    check_abuse(&state, org, &req.check)?;
    let domain_expiry_rt = state.worker_pool.domain_expiry_runtime();
    let deps = crate::worker::WorkerDeps {
        http: &state.http_clients,
        domain_expiry: &domain_expiry_rt,
    };
    let (result, probe) =
        crate::worker::execute_with_probe(Uuid::nil(), org.0, &req.check, &deps).await;
    let matched_expectations = matches!(result.status, crate::domain::CheckStatus::Up);
    let (response_headers_preview, response_body_snippet) = match probe {
        Some(p) => (p.response_headers_preview, p.response_body_snippet),
        None => (Vec::new(), None),
    };
    Ok(Json(TestResponse {
        matched_expectations,
        result,
        warnings: Vec::new(),
        response_headers_preview,
        response_body_snippet,
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
    Authorized(org, _): Authorized<TargetsExecute>,
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
        .run_once(target.id, org.0, &target.check, &host, q.force)
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
fn dispatch_first_check(state: &AppState, org: OrgId, target: &Target) {
    if !target.enabled {
        return;
    }
    let st = crate::scheduler::registry::ScheduledTarget::build(org, target.clone());
    state.worker_pool.dispatch(CheckTask {
        target: st.target,
        org_id: st.org_id,
        host_key: st.host_key,
        breaker_key: st.breaker_key,
        rdap_tld: st.rdap_tld,
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

/// Largest per-kind floor (tls_cert / domain_expiry = 3600s). Any
/// requested interval at or above this is guaranteed to clear every kind
/// floor and lets the PATCH path skip the existing-target read.
const MAX_KIND_FLOOR_SECS: i64 = 3_600;

fn min_interval_secs_for_kind(kind: &str) -> u64 {
    match kind {
        "tls_cert" | "domain_expiry" => 3_600,
        _ => 10,
    }
}

/// Per-resource validation, including the plan's check-interval floor. Both
/// `create` and `bulk_create` run this per item, so the floor is enforced by
/// construction on every path rather than in one handler (I4).
fn validate_new_target(new: &NewTarget, guard: &SsrfGuard, min_interval_secs: i64) -> Result<()> {
    let requested = new.interval.as_secs() as i64;
    let kind_floor = min_interval_secs_for_kind(new.check.kind()) as i64;
    let effective_floor = min_interval_secs.max(kind_floor);
    if requested < effective_floor {
        return Err(AppError::min_check_interval(
            requested,
            effective_floor,
            "free",
        ));
    }
    validate_check(&new.check, guard)?;
    validate_alerts(&new.alerts)?;
    validate_group_name(new.group_name.as_deref())?;
    validate_public_target_fields(
        new.public_name.as_deref(),
        new.public_description.as_deref(),
        new.public_group.as_deref(),
    )
}

/// Length-cap the public_name / public_description / public_group columns
/// that render onto the status page. The HTTP body limit (64 KiB single,
/// 8 MiB bulk) stops a single giant blob, but without per-field caps an
/// owner with a large `max_targets` quota could still bloat the rendered
/// status page by stuffing every target with the largest blob the body
/// limit allows.
fn validate_public_target_fields(
    name: Option<&str>,
    description: Option<&str>,
    group: Option<&str>,
) -> Result<()> {
    use crate::api::handlers::validation;
    if let Some(n) = name {
        validation::validate_title(n, "public_name")?;
    }
    validation::validate_description(description, "public_description")?;
    if let Some(g) = group {
        validation::check_length(g, "public_group", 50, codes::GROUP_TOO_LONG)?;
    }
    Ok(())
}

fn validate_group_name(group: Option<&str>) -> Result<()> {
    use crate::api::handlers::validation;
    if let Some(g) = group {
        validation::check_length(g, "group_name", 50, codes::GROUP_TOO_LONG)?;
    }
    Ok(())
}

/// The DB FK only proves the user exists, not that they're in this org —
/// without this check a target can be stamped with a cross-tenant user-id.
async fn validate_owner_is_member(state: &AppState, org: OrgId, owner: Option<Uuid>) -> Result<()> {
    let Some(uid) = owner else { return Ok(()) };
    let pool = state.require_db()?;
    let members = crate::storage::orgs::list_members(pool, org).await?;
    if !members.iter().any(|m| m.membership.user_id.0 == uid) {
        return Err(AppError::bad_request_field(
            codes::OWNER_NOT_MEMBER,
            format!("owner_user_id {uid} is not a member of this org"),
            "owner_user_id",
        ));
    }
    Ok(())
}

/// Update-path variant: extract the inner `&str` from `Option<&Option<String>>`
/// (skipping clears and omissions) and run the same caps.
fn validate_public_target_field_updates(
    name: Option<&Option<String>>,
    description: Option<&Option<String>>,
    group: Option<&Option<String>>,
) -> Result<()> {
    validate_public_target_fields(unset(name), unset(description), unset(group))
}

/// `Some(Some(s)) → Some(s)`; otherwise `None`. PATCH bodies model
/// "untouched" vs "clear to null" vs "set to value" with a double Option;
/// validation only runs on the set case.
fn unset(opt: Option<&Option<String>>) -> Option<&str> {
    opt.and_then(|inner| inner.as_deref())
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
async fn verify_alert_channels(state: &AppState, org: OrgId, alerts: &TargetAlerts) -> Result<()> {
    if alerts.is_empty() {
        return Ok(());
    }
    // One batched org-scoped query (mirrors maintenance's
    // `validate_component_ids`) instead of N point lookups.
    let ids: Vec<uuid::Uuid> = alerts.iter().map(|b| b.channel_id).collect();
    let known = state
        .notification_channel_store
        .existing_channel_ids(org, &ids)
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

/// Normalises the host/domain of the check in place: IDN-encoded, ASCII
/// lowercase, trailing dot stripped. After this runs, downstream stores the
/// canonical form and every layer (circuit breaker, host throttle, RDAP
/// singleflight) keys on the same string regardless of how the user typed it.
/// HTTP URLs are skipped — `url::Url` already canonicalises hosts on parse.
/// Returns `400` when IDN encoding fails on a user-supplied host.
fn canonicalize_check(check: &mut crate::domain::CheckSpec) -> Result<()> {
    use crate::domain::CheckSpec;
    use crate::worker::host_throttle::canonical_host_strict;
    use std::net::IpAddr;
    fn canon_host(host: &mut String, field: &'static str, code: &'static str) -> Result<()> {
        let raw = std::mem::take(host);
        let unbracketed = crate::security::unbracket(&raw);
        // IPs (literal or bracketed IPv6) bypass IDN — they have no host
        // shape to canonicalise.
        if unbracketed.parse::<IpAddr>().is_ok() {
            *host = unbracketed.to_owned();
            return Ok(());
        }
        match canonical_host_strict(unbracketed) {
            Ok(canon) => {
                *host = canon;
                Ok(())
            }
            Err(_) => Err(AppError::bad_request_field(
                code,
                format!("host '{raw}' is not a valid IDN domain"),
                field,
            )),
        }
    }
    match check {
        CheckSpec::Http(_) => Ok(()),
        CheckSpec::Tcp(tcp) => canon_host(&mut tcp.host, "check.host", codes::INVALID_TCP_HOST),
        CheckSpec::TlsCert(cert) => {
            canon_host(&mut cert.host, "check.host", codes::INVALID_TLS_CERT_PARAMS)
        }
        CheckSpec::DomainExpiry(d) => {
            canon_host(&mut d.domain, "check.domain", codes::INVALID_DOMAIN_PARAMS)
        }
        CheckSpec::Dns(d) => {
            canon_host(&mut d.domain, "check.domain", codes::INVALID_DNS_PARAMS)?;
            Ok(())
        }
    }
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
            if http.method == crate::domain::HttpMethod::Head
                && http.expected_body_contains.is_some()
            {
                return Err(AppError::bad_request_field(
                    codes::INVALID_HEAD_BODY_MATCH,
                    "expected_body_contains cannot be combined with method=HEAD (HEAD responses carry no body)",
                    "check.expected_body_contains",
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
        CheckSpec::Dns(d) => {
            if d.domain.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DNS_PARAMS,
                    "dns domain required",
                    "check.domain",
                ));
            }
            if let Some(resolver) = &d.resolver
                && !resolver.is_empty()
            {
                let sock = crate::http_client::parse_resolver_addr(resolver).map_err(|_| {
                    AppError::bad_request_field(
                        codes::INVALID_DNS_PARAMS,
                        format!("dns resolver '{resolver}' must be an IP or ip:port"),
                        "check.resolver",
                    )
                })?;
                // A custom resolver address is just another outbound
                // target; reuse the SSRF guard so users can't aim the
                // probe at an internal DNS server.
                check_ip(sock.ip(), guard)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::handlers::validation::{MAX_DESCRIPTION, MAX_TITLE};

    fn assert_bad_request_with_field(err: AppError, expected_field: &str) {
        match err {
            AppError::BadRequest { field: Some(f), .. } => assert_eq!(f, expected_field),
            other => panic!("expected BadRequest with field={expected_field}, got {other:?}"),
        }
    }

    #[test]
    fn validate_public_target_fields_accepts_none() {
        validate_public_target_fields(None, None, None).expect("all-None must pass");
    }

    #[test]
    fn validate_public_target_fields_accepts_normal_values() {
        validate_public_target_fields(Some("API"), Some("Public endpoint"), Some("Web"))
            .expect("normal values must pass");
    }

    #[test]
    fn validate_public_target_fields_rejects_oversized_name() {
        let big = "x".repeat(MAX_TITLE + 1);
        let err = validate_public_target_fields(Some(&big), None, None)
            .expect_err("over-cap must reject");
        assert_bad_request_with_field(err, "public_name");
    }

    #[test]
    fn validate_public_target_fields_rejects_oversized_description() {
        let big = "x".repeat(MAX_DESCRIPTION + 1);
        let err = validate_public_target_fields(None, Some(&big), None)
            .expect_err("over-cap must reject");
        assert_bad_request_with_field(err, "public_description");
    }

    #[test]
    fn validate_public_target_fields_rejects_oversized_group() {
        let big = "x".repeat(51);
        let err = validate_public_target_fields(None, None, Some(&big))
            .expect_err("over-cap must reject");
        assert_bad_request_with_field(err, "public_group");
    }

    #[test]
    fn validate_public_target_field_updates_passes_clears() {
        // Some(None) is a clear; no length check needed.
        validate_public_target_field_updates(Some(&None), Some(&None), Some(&None))
            .expect("clears must pass");
    }

    #[test]
    fn validate_public_target_field_updates_rejects_oversized_set() {
        let big = Some("x".repeat(MAX_DESCRIPTION + 1));
        let err = validate_public_target_field_updates(None, Some(&big), None)
            .expect_err("over-cap set must reject");
        assert_bad_request_with_field(err, "public_description");
    }

    #[test]
    fn touches_public_view_detects_visibility_and_render_fields() {
        let mut u = TargetUpdate::default();
        assert!(!touches_public_view(&u), "empty update is no-op");
        u.enabled = Some(false);
        assert!(
            !touches_public_view(&u),
            "enabled flag does not change public projection"
        );

        for u in [
            TargetUpdate {
                public_status: Some(true),
                ..Default::default()
            },
            TargetUpdate {
                public_status: Some(false),
                ..Default::default()
            },
            TargetUpdate {
                public_name: Some(Some("API".into())),
                ..Default::default()
            },
            TargetUpdate {
                public_description: Some(None),
                ..Default::default()
            },
            TargetUpdate {
                public_group: Some(Some("Web".into())),
                ..Default::default()
            },
            TargetUpdate {
                public_sort_order: Some(5),
                ..Default::default()
            },
            TargetUpdate {
                name: Some("new-name".into()),
                ..Default::default()
            },
            TargetUpdate {
                group_name: Some(Some("g".into())),
                ..Default::default()
            },
        ] {
            assert!(touches_public_view(&u), "{u:?} must invalidate");
        }
    }

    fn head_spec(body_match: Option<&str>) -> crate::domain::CheckSpec {
        use crate::domain::{CheckSpec, ExpectedStatus, HttpCheck, HttpMethod};
        use std::time::Duration;
        CheckSpec::Http(HttpCheck {
            url: "https://example.com/".parse().unwrap(),
            method: HttpMethod::Head,
            timeout: Duration::from_secs(5),
            follow_redirects: false,
            max_redirects: 0,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: body_match.map(str::to_owned),
            headers: Default::default(),
            body: None,
            verify_tls: true,
            basic_auth: None,
            bearer_token: None,
        })
    }

    #[test]
    fn validate_check_rejects_head_with_body_match() {
        use crate::security::ssrf::SsrfGuard;
        let err = validate_check(&head_spec(Some("hello")), &SsrfGuard::strict())
            .expect_err("HEAD + body match must reject");
        assert_bad_request_with_field(err, "check.expected_body_contains");
    }

    #[test]
    fn validate_check_accepts_head_without_body_match() {
        use crate::security::ssrf::SsrfGuard;
        validate_check(&head_spec(None), &SsrfGuard::strict())
            .expect("HEAD without body match must pass");
    }

    #[test]
    fn canonicalize_check_idn_encodes_tcp_host() {
        use crate::domain::{CheckSpec, TcpCheck};
        use std::time::Duration;
        let mut spec = CheckSpec::Tcp(TcpCheck {
            host: "Bähn.DE.".into(),
            port: 443,
            timeout: Duration::from_secs(5),
        });
        canonicalize_check(&mut spec).unwrap();
        match spec {
            CheckSpec::Tcp(tcp) => assert_eq!(tcp.host, "xn--bhn-qla.de"),
            _ => panic!("variant changed"),
        }
    }

    #[test]
    fn canonicalize_check_preserves_ipv6_brackets_stripped() {
        use crate::domain::{CheckSpec, TcpCheck};
        use std::time::Duration;
        let mut spec = CheckSpec::Tcp(TcpCheck {
            host: "[::1]".into(),
            port: 443,
            timeout: Duration::from_secs(5),
        });
        canonicalize_check(&mut spec).unwrap();
        match spec {
            CheckSpec::Tcp(tcp) => assert_eq!(tcp.host, "::1"),
            _ => panic!("variant changed"),
        }
    }

    #[test]
    fn canonicalize_check_lowercases_domain_expiry() {
        use crate::domain::{CheckSpec, DomainExpiryCheck};
        use std::time::Duration;
        let mut spec = CheckSpec::DomainExpiry(DomainExpiryCheck {
            domain: "EXAMPLE.COM.".into(),
            warn_days: 30,
            critical_days: 7,
            timeout: Duration::from_secs(5),
        });
        canonicalize_check(&mut spec).unwrap();
        match spec {
            CheckSpec::DomainExpiry(d) => assert_eq!(d.domain, "example.com"),
            _ => panic!("variant changed"),
        }
    }
}
