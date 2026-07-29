use std::net::IpAddr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::AppendHeaders;
use serde::{Deserialize, Serialize};
use url::Host;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::ad_hoc_dispatch::DeliveredResult;
use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::page::{PageEnvelope, PageOfTarget};
use crate::api::redaction::{REDACTED, Redacted};
use crate::api::types::{
    BulkAction, BulkActionFailure, BulkActionRequest, BulkActionResponse, TestRequest, TestResponse,
};
use crate::app::AppState;
use crate::auth::scope::Scope;
use crate::domain::agent_wire::{DispatchKind, DispatchedCheck};
use crate::domain::{
    CheckResult, CheckSpec, NewTarget, OrgId, RegionIncidentPolicy, Target, TargetAlerts,
    TargetUpdate, min_interval_secs_for_kind,
};
use crate::error::{AppError, Result};
use crate::security::SsrfGuard;
use crate::storage::TargetFilter;
use crate::web::{
    Authorized, CurrentOrg, RequestSource, TargetsDelete, TargetsExecute, TargetsRead,
    TargetsWrite, TokenScopes,
};

const BULK_MAX: usize = 10_000;
const LIST_LIMIT_DEFAULT: usize = 50;
const LIST_LIMIT_MAX: usize = 10_000;
const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

use super::invalidate_pages_for;

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
    gate_flow(&new.check, &plan)?;
    validate_new_target(&new, &guard, i64::from(plan.min_check_interval_secs))?;
    let available = state.target_store.available_regions().await?;
    validate_region_policy(new.region_policy, available.len())?;
    verify_alert_channels(&state, org, &new.alerts).await?;
    validate_owner_is_member(&state, org, new.owner_user_id).await?;
    check_abuse(&state, org, &new.check)?;
    validate_variable_refs(&state, org, &new.check).await?;
    // Friendly pre-check; the store INSERT enforces the same cap atomically.
    state.quotas.check_can_create_targets(org, None, 1).await?;
    if matches!(&new.check, CheckSpec::Flow(_)) {
        state.quotas.check_can_create_flow(org, None, 1).await?;
    }
    let default_region = state.cfg.scheduler.effective_default_region().to_string();
    let available = flow_restrict_regions(&state, &new.check, available).await?;
    let regions = default_region_set(available, plan.max_regions, &default_region);
    ensure_flow_regions_covered(&state, &new.check, &regions).await?;
    // None → the store applies the column default (Majority).
    let t = state
        .target_store
        .create(
            org,
            new,
            source,
            i64::from(plan.max_targets),
            i64::from(plan.max_flow_checks),
        )
        .await?;
    if t.check.is_passive() {
        ensure_heartbeat(&state, org, t.id).await?;
    }
    // The store seeds only the default region; widen to the full set. Passive
    // kinds have no probe region, so skip the seed.
    if regions.len() > 1 && !t.check.is_passive() {
        state
            .target_store
            .set_target_regions(org, t.id, &regions)
            .await?;
    }
    dispatch_first_check(&state, org, &t, &regions).await;
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
    description = "Omit fields you don't want to change. For HTTP credentials: omit `basic_auth`/`bearer_token` (or send null) to keep the stored value, send an empty sentinel (`[\"\",\"\"]` / `\"\"`) to clear it, or a real value to replace it. The redaction sentinels `[\"***\",\"***\"]` / `\"***\"` return 400, so never echo them back.",
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
    // Read once and share: the flow paths, the credential carry, and the
    // interval floor all want the same row.
    let mut stored_target: Option<Target> = None;
    if let Some(check) = update.check.as_mut() {
        canonicalize_check(check)?;
        if let crate::domain::CheckSpec::Http(http) = check {
            let (cleared_basic, cleared_bearer) = take_cleared_credentials(http);
            let (carry_basic, carry_bearer) = carry_flags(http, cleared_basic, cleared_bearer);
            if (carry_basic || carry_bearer)
                && let Some(existing) = state.target_store.get(org, id).await?
                && let crate::domain::CheckSpec::Http(stored) = &existing.check
            {
                carry_credentials(http, stored, carry_basic, carry_bearer);
            }
        }
        // For a flow edit, read the stored monitor once: carry masked fill
        // secrets forward (an untouched `***` keeps the stored value) before
        // validation, and reuse it to tell a net-new flow from an edit.
        if matches!(check, crate::domain::CheckSpec::Flow(_)) {
            stored_target = state.target_store.get(org, id).await?;
        }
        if let crate::domain::CheckSpec::Flow(flow) = check
            && let Some(existing) = &stored_target
            && let crate::domain::CheckSpec::Flow(stored) = &existing.check
        {
            carry_flow_secrets(flow, stored);
        }
        validate_check(check, &ssrf_guard(&state))?;
        check_abuse(&state, org, check)?;
        validate_variable_refs(&state, org, check).await?;
        if matches!(check, crate::domain::CheckSpec::Flow(_)) {
            let was_flow = matches!(
                stored_target.as_ref().map(|t| &t.check),
                Some(crate::domain::CheckSpec::Flow(_))
            );
            // Gate + count only a net-new flow; editing an existing one stays
            // allowed even if the plan has since dropped the capability, so a
            // downgraded org can still fix a monitor it already runs.
            if !was_flow {
                let plan = state.quotas.limit_for_org(org).await?;
                gate_flow(check, &plan)?;
                state.quotas.check_can_create_flow(org, None, 1).await?;
            }
            if let Some(regions) = state.target_store.regions_for_target(org, id).await? {
                ensure_flow_regions_covered(&state, check, &regions).await?;
            }
        }
    }
    if let Some(alerts) = &update.alerts {
        validate_alerts(alerts)?;
        verify_alert_channels(&state, org, alerts).await?;
    }
    validate_alert_confirmations(update.alert_confirmations)?;
    validate_renotify_interval(update.renotify_interval_secs)?;
    if update.region_policy.is_some() {
        let available = state.target_store.available_regions().await?;
        validate_region_policy(update.region_policy, available.len())?;
    }
    if let Some(Some(g)) = update.group_name.as_ref() {
        validate_group_name(Some(g.as_str()))?;
    }
    if let Some(Some(uid)) = update.owner_user_id {
        validate_owner_is_member(&state, org, Some(uid)).await?;
    }
    validate_patch_interval(&state, org, id, &update, stored_target.as_ref()).await?;
    // The disabled→enabled re-arm is folded into the store's enable statement,
    // so this path (and every other enable surface) inherits it.
    let check_rewritten = update.check.is_some();
    match state.target_store.update(org, id, update, source).await? {
        Some(t) => {
            if check_rewritten {
                sync_heartbeat_kind(&state, org, &t).await?;
            }
            invalidate_pages_for(&state, org, &[id]).await;
            Ok(Redacted::new(t))
        }
        None => Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        )),
    }
}

/// Mint (or keep) the ping-token row for a heartbeat-kind target. Its anchor
/// becomes resident on the next scheduler refresh, the same refresh that
/// admits the target into evaluation, so the ordering is safe.
async fn ensure_heartbeat(state: &AppState, org: OrgId, target_id: Uuid) -> Result<()> {
    state
        .heartbeat_store
        .ensure(org, target_id)
        .await?
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("heartbeat row missing for {target_id}")))?;
    Ok(())
}

/// Reconcile heartbeat state after a check rewrite: a heartbeat kind keeps the
/// ping token, any other kind revokes it. Idempotent, so concurrent rewrites
/// converge on the final kind.
async fn sync_heartbeat_kind(state: &AppState, org: OrgId, t: &Target) -> Result<()> {
    if t.check.is_passive() {
        ensure_heartbeat(state, org, t.id).await?;
    } else {
        state.heartbeat_store.remove(org, t.id).await?;
    }
    Ok(())
}

/// The regions a monitor probes from. A single-region deployment is one entry.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TargetRegions {
    pub regions: Vec<String>,
}

/// One region in the catalog returned by `GET /api/v1/regions`.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegionInfo {
    pub id: String,
    pub name: String,
    pub city: String,
    pub country_code: Option<String>,
    pub continent: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// The enabled region catalog a monitor may be assigned to.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegionCatalog {
    pub regions: Vec<RegionInfo>,
}

#[utoipa::path(
    get, path = "/api/v1/regions", tag = "targets",
    summary = "List the available probe regions",
    responses((status = 200, body = RegionCatalog)),
)]
pub async fn list_regions(
    State(state): State<AppState>,
    Authorized(_org, _): Authorized<TargetsRead>,
) -> Result<Json<RegionCatalog>> {
    let regions = state
        .regions_detailed()
        .await?
        .into_iter()
        .map(|r| RegionInfo {
            id: r.id,
            name: r.name,
            city: r.city,
            country_code: r.country_code,
            continent: r.continent,
            latitude: r.latitude,
            longitude: r.longitude,
        })
        .collect();
    Ok(Json(RegionCatalog { regions }))
}

#[utoipa::path(
    get, path = "/api/v1/targets/{id}/regions", tag = "targets",
    summary = "List the regions a monitor probes from",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = TargetRegions), (status = 404, body = ApiError)),
)]
pub async fn get_target_regions(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<TargetsRead>,
    Path(id): Path<Uuid>,
) -> Result<Json<TargetRegions>> {
    match state.target_store.regions_for_target(org, id).await? {
        Some(regions) => Ok(Json(TargetRegions { regions })),
        None => Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        )),
    }
}

#[utoipa::path(
    put, path = "/api/v1/targets/{id}/regions", tag = "targets",
    summary = "Set the regions a monitor probes from",
    params(("id" = Uuid, Path)), request_body = TargetRegions,
    responses(
        (status = 200, body = TargetRegions),
        (status = 404, body = ApiError),
        (status = 422, body = ApiError),
    ),
)]
pub async fn set_target_regions(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<TargetsWrite>,
    Path(id): Path<Uuid>,
    Json(req): Json<TargetRegions>,
) -> Result<Json<TargetRegions>> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "target not found"))?;
    if target.check.is_passive() {
        return Err(AppError::unprocessable(
            codes::REGION_INVALID,
            "heartbeat monitors receive pings; they are not probed from regions",
        ));
    }
    let mut regions: Vec<String> = req
        .regions
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    regions.sort();
    regions.dedup();
    if regions.is_empty() {
        return Err(AppError::unprocessable(
            codes::REGION_INVALID,
            "at least one region is required",
        ));
    }
    state
        .quotas
        .check_region_assignment(org, None, regions.len() as i64)
        .await?;
    let available: std::collections::HashSet<String> = state
        .target_store
        .available_regions()
        .await?
        .into_iter()
        .collect();
    if let Some(bad) = regions.iter().find(|r| !available.contains(*r)) {
        return Err(AppError::unprocessable(
            codes::REGION_INVALID,
            format!("unknown or disabled region: {bad}"),
        ));
    }
    if !state
        .target_store
        .set_target_regions(org, id, &regions)
        .await?
    {
        return Err(AppError::not_found(
            codes::TARGET_NOT_FOUND,
            "target not found",
        ));
    }
    Ok(Json(TargetRegions { regions }))
}

/// Ping surface of a heartbeat monitor: the inbound URL the customer's system
/// calls, and the last accepted ping.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HeartbeatInfo {
    /// Full ping URL. `null` when the stored token can't be decrypted (KEK
    /// rotated out).
    pub ping_url: Option<String>,
    pub last_ping_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Read-only projection of a heartbeat's ping surface, shared by the API
/// handler and the detail page. Never mints: `ping_url` is `None` while the
/// row is still provisioning or when the token can't be decrypted.
pub(crate) async fn heartbeat_info(
    state: &AppState,
    org: OrgId,
    target_id: Uuid,
) -> Result<HeartbeatInfo> {
    let hb = state.heartbeat_store.get(org, target_id).await?;
    Ok(HeartbeatInfo {
        ping_url: hb.as_ref().and_then(|h| h.token.as_deref()).map(|t| {
            format!(
                "{}/ping/{t}",
                state.cfg.auth.public_base_url.trim_end_matches('/')
            )
        }),
        last_ping_at: hb.and_then(|h| h.last_ping_at),
    })
}

#[utoipa::path(
    get, path = "/api/v1/targets/{id}/heartbeat", tag = "targets",
    summary = "Get a heartbeat monitor's ping URL and last ping",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = HeartbeatInfo),
        (status = 404, body = ApiError),
    ),
)]
pub async fn get_heartbeat(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<TargetsWrite>,
    Path(id): Path<Uuid>,
) -> Result<Json<HeartbeatInfo>> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "target not found"))?;
    if !target.check.is_passive() {
        return Err(AppError::not_found(
            codes::HEARTBEAT_NOT_CONFIGURED,
            "this monitor is not a heartbeat",
        ));
    }
    Ok(Json(heartbeat_info(&state, org, id).await?))
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
    // Resolve curated pages before the FK cascade clears the join rows.
    let pages = state
        .status_page_store
        .pages_for_targets(org, &[id])
        .await
        .unwrap_or_default();
    if state.target_store.delete(org, id).await? {
        for page in pages {
            state.public_source.invalidate(page).await;
        }
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
    let available = state.target_store.available_regions().await?;
    for new in &mut items {
        canonicalize_check(&mut new.check)?;
        gate_flow(&new.check, &plan)?;
        validate_new_target(new, &guard, i64::from(plan.min_check_interval_secs))?;
        validate_region_policy(new.region_policy, available.len())?;
        verify_alert_channels(&state, org, &new.alerts).await?;
        check_abuse(&state, org, &new.check)?;
        validate_variable_refs(&state, org, &new.check).await?;
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
    // Captured before the move so each item's explicit policy survives the
    // bulk insert and is reapplied once the full region set is assigned.
    let item_policies: Vec<Option<RegionIncidentPolicy>> =
        items.iter().map(|i| i.region_policy).collect();
    let default_region = state.cfg.scheduler.effective_default_region().to_string();
    let regions = default_region_set(available, plan.max_regions, &default_region);
    for new in &items {
        ensure_flow_regions_covered(&state, &new.check, &regions).await?;
    }
    let flow_count = items
        .iter()
        .filter(|i| matches!(&i.check, CheckSpec::Flow(_)))
        .count() as i64;
    if flow_count > 0 {
        state
            .quotas
            .check_can_create_flow(org, None, flow_count)
            .await?;
    }
    let out = state
        .target_store
        .bulk_create(
            org,
            items,
            source,
            i64::from(plan.max_targets),
            i64::from(plan.max_flow_checks),
        )
        .await?;
    // Heartbeat items get their ping-token rows from the next scheduler
    // refresh (self-heal), not a per-item mint loop, so bulk stays one batch.
    if regions.len() > 1 {
        let derived = RegionIncidentPolicy::default();
        // Flow items only probe from capable regions, so assign them the capable
        // subset rather than the full set the other kinds get.
        let flow_regions: Vec<String> = if flow_count > 0 {
            let capable = flow_capable_set(&state).await?;
            regions
                .iter()
                .filter(|r| capable.contains(*r))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        for (t, explicit) in out.iter().zip(item_policies) {
            let assigned = if matches!(&t.check, CheckSpec::Flow(_)) {
                &flow_regions
            } else {
                &regions
            };
            state
                .target_store
                .set_target_regions(org, t.id, assigned)
                .await?;
            state
                .target_store
                .update(
                    org,
                    t.id,
                    TargetUpdate {
                        region_policy: Some(explicit.unwrap_or(derived)),
                        ..Default::default()
                    },
                    source,
                )
                .await?;
        }
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

    let succeeded = match &req.action {
        BulkAction::Enable => state.target_store.set_enabled(org, &req.ids, true).await?,
        BulkAction::Disable => state.target_store.set_enabled(org, &req.ids, false).await?,
        BulkAction::Delete => {
            // Capture curated pages before the cascade drops the join rows.
            let pages = state
                .status_page_store
                .pages_for_targets(org, &req.ids)
                .await
                .unwrap_or_default();
            let succeeded = state.target_store.delete_bulk(org, &req.ids).await?;
            for page in pages {
                state.public_source.invalidate(page).await;
            }
            succeeded
        }
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
        (status = 503, description = "No probe available; probing runs on agents", body = ApiError),
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
    reject_passive_probe(&req.check)?;
    let requested = req.region.filter(|r| !r.trim().is_empty());
    let region = if matches!(&req.check, CheckSpec::Flow(_)) {
        let plan = state.quotas.limit_for_org(org).await?;
        gate_flow(&req.check, &plan)?;
        let prefer: Vec<String> = requested.into_iter().collect();
        pick_flow_region(&state, &prefer).await?
    } else {
        requested.unwrap_or_else(|| state.cfg.scheduler.effective_default_region().to_string())
    };
    let view = run_ad_hoc(&state, org, &region, DispatchKind::Test, None, req.check).await?;
    let matched_expectations = matches!(view.result.status, crate::domain::CheckStatus::Up);
    Ok(Json(TestResponse {
        matched_expectations,
        result: view.result,
        warnings: Vec::new(),
        response_headers_preview: view.response_headers_preview,
        response_body_snippet: view.response_body_snippet,
        region: Some(region),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/targets/{id}/check-now",
    tag = "targets",
    summary = "Run an immediate check against an existing target",
    description = "Dispatches a one-off check to an agent in the target's region and waits for the result. Uses the target's stored (un-redacted) credentials; the result IS persisted, same as a scheduled check. Returns 503 if no agent is available to run it.",
    params(
        ("id" = Uuid, Path),
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
        (status = 503, description = "No probe available; probing runs on agents", body = ApiError),
    ),
)]
pub async fn check_now(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<TargetsExecute>,
    Path(id): Path<Uuid>,
) -> Result<Json<CheckResult>> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found(codes::TARGET_NOT_FOUND, "target not found"))?;
    Ok(Json(check_now_via_dispatch(&state, org, &target).await?))
}

/// Run an immediate check on `target` via an agent in its region and return the
/// result. Shared by the REST check-now handler and the MCP tool so both go
/// through the same region-aware dispatch (the agent persists the result).
pub(crate) async fn check_now_via_dispatch(
    state: &AppState,
    org: OrgId,
    target: &Target,
) -> Result<CheckResult> {
    reject_passive_probe(&target.check)?;
    let region = if matches!(&target.check, CheckSpec::Flow(_)) {
        let assigned = state
            .target_store
            .regions_for_target(org, target.id)
            .await?
            .unwrap_or_default();
        pick_flow_region(state, &assigned).await?
    } else {
        resolve_check_now_region(state, org, target.id).await?
    };
    let view = run_ad_hoc(
        state,
        org,
        &region,
        DispatchKind::CheckNow,
        Some(target.id),
        target.check.clone(),
    )
    .await?;
    Ok(view.result)
}

/// Dispatch a target's first check in every assigned region so its status
/// appears within a second of creation instead of waiting up to a full
/// config-pull cycle. Fire-and-forget per region, and only where an agent is
/// already holding the long-poll — regions without a live agent are covered by
/// their next scheduled pull. The agent's result POST persists each result.
async fn dispatch_first_check(state: &AppState, org: OrgId, target: &Target, regions: &[String]) {
    if !target.enabled || target.check.is_passive() {
        return;
    }
    // Restrict a flow first-check to capable regions: a false Error elsewhere
    // would never be overwritten (routing later withholds flow from that region).
    let is_flow = matches!(&target.check, CheckSpec::Flow(_));
    let flow_regions: Vec<String> = if is_flow {
        let mut c = state
            .target_store
            .flow_capable_regions()
            .await
            .unwrap_or_default();
        if state.cfg.flow.enabled {
            c.push(state.cfg.scheduler.effective_default_region().to_string());
        }
        c
    } else {
        Vec::new()
    };
    for region in regions {
        if is_flow && !flow_regions.contains(region) {
            continue;
        }
        if !state.ad_hoc.region_live(region) {
            continue;
        }
        // Fire-and-forget: the dropped receiver is fine — the agent's result
        // POST still persists the check-now result; we just don't wait for it.
        let _rx = state.ad_hoc.dispatch(
            region,
            DispatchedCheck {
                id: Uuid::now_v7(),
                kind: DispatchKind::CheckNow,
                org_id: org.0,
                target_id: Some(target.id),
                spec: target.check.clone(),
            },
        );
    }
}

/// Dispatch an interactive check to an agent holding `region` and wait for the
/// result. 503 when no agent is holding the region (fast) or none answers in
/// time.
async fn run_ad_hoc(
    state: &AppState,
    org: OrgId,
    region: &str,
    kind: DispatchKind,
    target_id: Option<Uuid>,
    check: CheckSpec,
) -> Result<DeliveredResult> {
    // Chokepoint: every interactive dispatch funnels through here, so a future
    // caller can't hand a passive check to an agent by omission.
    reject_passive_probe(&check)?;
    if !state.ad_hoc.region_live(region) {
        return Err(AppError::service_unavailable(
            codes::PROBE_UNAVAILABLE,
            format!("no probe available in region '{region}'; probing runs on agents"),
        ));
    }
    let (check, secrets) = resolve_spec_variables(state, org, check).await?;
    let check_id = Uuid::now_v7();
    let rx = state.ad_hoc.dispatch(
        region,
        DispatchedCheck {
            id: check_id,
            kind,
            org_id: org.0,
            target_id,
            spec: check,
        },
    );
    match tokio::time::timeout(crate::ad_hoc_dispatch::RESULT_WAIT, rx).await {
        Ok(Ok(mut delivered)) => {
            scrub_secrets(&mut delivered, &secrets);
            Ok(delivered)
        }
        _ => {
            state.ad_hoc.abandon(check_id);
            Err(AppError::service_unavailable(
                codes::PROBE_UNAVAILABLE,
                "no probe completed this check in time; the region's agent may be offline",
            ))
        }
    }
}

/// Region to run a check-now in: the target's assigned region with a live
/// agent, else its first region (run_ad_hoc then 503s), else the default
/// region for an unassigned target.
async fn resolve_check_now_region(state: &AppState, org: OrgId, id: Uuid) -> Result<String> {
    let regions = state
        .target_store
        .regions_for_target(org, id)
        .await?
        .unwrap_or_default();
    if regions.is_empty() {
        return Ok(state.cfg.scheduler.effective_default_region().to_string());
    }
    if let Some(r) = regions.iter().find(|r| state.ad_hoc.region_live(r)) {
        return Ok(r.clone());
    }
    Ok(regions[0].clone())
}

/// Pick a flow-capable region for an interactive flow check. `prefer` = the
/// caller's candidates (a target's regions or an explicit test region), empty for
/// any. Prefers a live region so the dispatch doesn't 503; errors if none qualify.
/// Regions that can actually run a flow: agents self-reporting the capability,
/// plus the control-plane region when it runs the engine in-process.
pub(crate) async fn flow_capable_set(
    state: &AppState,
) -> Result<std::collections::HashSet<String>> {
    let mut capable: std::collections::HashSet<String> = state
        .target_store
        .flow_capable_regions()
        .await?
        .into_iter()
        .collect();
    if state.cfg.flow.enabled {
        capable.insert(state.cfg.scheduler.effective_default_region().to_string());
    }
    Ok(capable)
}

/// Narrow an assigned region set to those that can run a flow. Regions that
/// can't never pull the flow, so leaving them in skews its aggregate status.
/// Pass-through for non-flow checks.
async fn flow_restrict_regions(
    state: &AppState,
    check: &CheckSpec,
    available: Vec<String>,
) -> Result<Vec<String>> {
    if !matches!(check, CheckSpec::Flow(_)) {
        return Ok(available);
    }
    let capable = flow_capable_set(state).await?;
    Ok(available
        .into_iter()
        .filter(|r| capable.contains(r.as_str()))
        .collect())
}

async fn pick_flow_region(state: &AppState, prefer: &[String]) -> Result<String> {
    let capable: Vec<String> = flow_capable_set(state).await?.into_iter().collect();
    if capable.is_empty() {
        return Err(AppError::service_unavailable(
            codes::PROBE_UNAVAILABLE,
            "no flow-capable agent is available",
        ));
    }
    let candidates: Vec<String> = if prefer.is_empty() {
        capable
    } else {
        let filtered: Vec<String> = prefer
            .iter()
            .filter(|r| capable.contains(*r))
            .cloned()
            .collect();
        if filtered.is_empty() {
            return Err(AppError::unprocessable(
                codes::NO_FLOW_CAPABLE_AGENT,
                "no flow-capable agent runs in the requested region",
            ));
        }
        filtered
    };
    Ok(candidates
        .iter()
        .find(|r| state.ad_hoc.region_live(r))
        .cloned()
        .unwrap_or_else(|| candidates[0].clone()))
}

/// Substitute `{{var}}` references in an interactive check (test / check-now)
/// before it is dispatched, so the operator probes against the real resolved
/// values. Returns the secret plaintexts substituted so the caller can scrub
/// them from a captured response. A missing variable or a policy violation
/// surfaces as a 422 here. Non-HTTP or no-variable specs pass through.
async fn resolve_spec_variables(
    state: &AppState,
    org: OrgId,
    spec: CheckSpec,
) -> Result<(CheckSpec, Vec<String>)> {
    use crate::worker::interpolate::{
        flow_uses_vars, resolve_flow_spec, resolve_http_spec, uses_vars,
    };

    let needs_vars = match &spec {
        CheckSpec::Http(http) => uses_vars(http),
        CheckSpec::Flow(flow) => flow_uses_vars(flow),
        _ => false,
    };
    if !needs_vars {
        return Ok((spec, Vec::new()));
    }
    let vars = state.variable_store.resolve_map(org).await?;
    let unresolved = |e: crate::worker::interpolate::ResolveError| {
        AppError::unprocessable(codes::UNRESOLVED_VARIABLE, e.to_string())
    };
    let resolved = match &spec {
        CheckSpec::Http(http) => {
            CheckSpec::Http(resolve_http_spec(http, &vars).map_err(unresolved)?)
        }
        CheckSpec::Flow(flow) => {
            CheckSpec::Flow(resolve_flow_spec(flow, &vars).map_err(unresolved)?)
        }
        _ => spec,
    };
    Ok((resolved, secret_values(&vars)))
}

/// Reject a monitor whose `{{var}}` references don't all resolve against the
/// org's variables — an unknown key or a secret used in a field that forbids it.
/// Fails fast at save instead of silently dropping the monitor at probe time.
/// Non-HTTP or no-variable specs are a no-op.
async fn validate_variable_refs(state: &AppState, org: OrgId, check: &CheckSpec) -> Result<()> {
    use crate::worker::interpolate::{
        flow_uses_vars, repoint_risk, resolve_flow_spec, resolve_http_spec, uses_vars,
    };

    let unresolved = |e: crate::worker::interpolate::ResolveError| {
        AppError::unprocessable(codes::UNRESOLVED_VARIABLE, e.to_string())
    };
    match check {
        CheckSpec::Http(http) if uses_vars(http) => {
            let vars = state.variable_store.resolve_map(org).await?;
            resolve_http_spec(http, &vars)
                .map(drop)
                .map_err(unresolved)?;
            if let Some(risk) = repoint_risk(http, &vars) {
                tracing::warn!(
                    org = %org.0,
                    url_variables = ?risk.url_keys,
                    secret_headers = ?risk.secret_header_keys,
                    "monitor combines a url variable with a secret header; repointing the \
                     url variable would send the secret to a different host"
                );
            }
        }
        CheckSpec::Flow(flow) if flow_uses_vars(flow) => {
            let vars = state.variable_store.resolve_map(org).await?;
            resolve_flow_spec(flow, &vars)
                .map(drop)
                .map_err(unresolved)?;
        }
        _ => {}
    }
    Ok(())
}

/// Secret plaintexts in the org's map, used to scrub an interactive probe's
/// captured response. Short values are skipped: scrubbing a one or two character
/// string would mangle unrelated response text for no real protection.
fn secret_values(vars: &crate::domain::VarMap) -> Vec<String> {
    vars.values()
        .filter(|v| v.is_secret && v.value.len() >= 4)
        .map(|v| v.value.clone())
        .collect()
}

/// Replace any resolved secret echoed back in an interactive probe's captured
/// response (body snippet + header values) with `***`, so a value a secret
/// variable supplied is never shown back through the test surface.
fn scrub_secrets(delivered: &mut DeliveredResult, secrets: &[String]) {
    fn redact(s: &mut String, secrets: &[String]) {
        for secret in secrets {
            if s.contains(secret.as_str()) {
                *s = s.replace(secret.as_str(), "***");
            }
        }
    }
    if secrets.is_empty() {
        return;
    }
    if let Some(snippet) = delivered.response_body_snippet.as_mut() {
        redact(snippet, secrets);
    }
    for h in &mut delivered.response_headers_preview {
        redact(&mut h.value, secrets);
    }
    // A flow's error string is its main output and can echo a resolved secret
    // (e.g. a page that reflects a submitted value); scrub it like the rest.
    if let Some(err) = delivered.result.error.as_mut() {
        redact(err, secrets);
    }
}

fn ssrf_guard(state: &AppState) -> SsrfGuard {
    SsrfGuard::new(state.cfg.security.allow_private_targets)
}

/// Interactive probing (test-now / check-now) is meaningless for a passive
/// check, since there is nothing to reach out to.
fn reject_passive_probe(check: &CheckSpec) -> Result<()> {
    if check.is_passive() {
        return Err(AppError::bad_request(
            codes::HEARTBEAT_NOT_PROBEABLE,
            "heartbeat monitors receive pings from your systems; there is nothing to probe",
        ));
    }
    Ok(())
}

/// Reject a flow monitor whose plan has not enabled the gated capability. Runs on
/// every admission path (create, update, bulk).
fn gate_flow(check: &CheckSpec, plan: &crate::domain::Plan) -> Result<()> {
    if matches!(check, CheckSpec::Flow(_)) && plan.max_flow_checks <= 0 {
        return Err(AppError::forbidden_code(
            codes::FLOW_CHECKS_DISABLED,
            "flow monitors are not available on your plan",
        ));
    }
    Ok(())
}

/// Reject a flow monitor whose regions have no node that can run it — otherwise
/// it is accepted and then silently never probed. Capable = an enabled
/// flow-capable agent in the region, or the control plane's own region when it
/// runs flow in-process. No-op for non-flow checks.
async fn ensure_flow_regions_covered(
    state: &AppState,
    check: &CheckSpec,
    regions: &[String],
) -> Result<()> {
    if !matches!(check, CheckSpec::Flow(_)) {
        return Ok(());
    }
    let capable = flow_capable_set(state).await?;
    if regions.iter().any(|r| capable.contains(r)) {
        return Ok(());
    }
    Err(AppError::unprocessable(
        codes::NO_FLOW_CAPABLE_AGENT,
        "no flow-capable agent runs in this monitor's region; enable the flow \
         engine on an agent there before creating the monitor",
    ))
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

/// Largest per-kind floor (domain_expiry). Any requested interval at or above
/// this is guaranteed to clear every kind floor and lets the PATCH path skip
/// the existing-target read. Set too low, the skip waves through an interval
/// the kind would reject, so a test holds it to the real maximum.
const MAX_KIND_FLOOR_SECS: i64 = 43_200;

/// The PATCH counterpart of the floor check in [`validate_new_target`]. A kind
/// change is validated against the stored interval, since switching to a slower
/// kind while omitting `interval` would otherwise keep a cadence that kind
/// rejects. A missing target is left for the update itself to 404.
async fn validate_patch_interval(
    state: &AppState,
    org: OrgId,
    id: Uuid,
    update: &TargetUpdate,
    prefetched: Option<&Target>,
) -> Result<()> {
    let requested = update.interval.map(|i| i.as_secs() as i64);
    if requested.is_none() && update.check.is_none() {
        return Ok(());
    }
    // The row is only worth reading for the half the request leaves out, and an
    // interval above every kind floor answers the kind half on its own.
    let needs_row = requested.is_none()
        || (update.check.is_none() && requested.is_some_and(|r| r < MAX_KIND_FLOOR_SECS));
    let fetched = match (needs_row, prefetched) {
        (true, None) => state.target_store.get(org, id).await?,
        _ => None,
    };
    let stored = prefetched.or(fetched.as_ref());
    let Some(requested) = requested.or_else(|| stored.map(|t| t.interval.as_secs() as i64)) else {
        return Ok(());
    };
    let kind = update
        .check
        .as_ref()
        .map(|c| c.kind())
        .or_else(|| stored.map(|t| t.check.kind()));
    let plan_min = i64::from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .min_check_interval_secs,
    );
    // No kind means the read was skipped, which happens only when no kind floor
    // could bind. The plan floor applies either way.
    let effective_floor = plan_min.max(kind.map_or(0, |k| min_interval_secs_for_kind(k) as i64));
    if requested < effective_floor {
        return Err(AppError::min_check_interval(
            requested,
            effective_floor,
            "free",
        ));
    }
    Ok(())
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
    validate_alert_confirmations(Some(new.alert_confirmations))?;
    validate_renotify_interval(Some(new.renotify_interval_secs))?;
    validate_group_name(new.group_name.as_deref())
}

/// The outage reminder cadence is either off (0) or no tighter than a minute —
/// a sub-minute reminder would just spam responders.
fn validate_renotify_interval(secs: Option<u32>) -> Result<()> {
    if matches!(secs, Some(n) if n > 0 && n < 60) {
        return Err(AppError::bad_request_field(
            codes::INVALID_ALERT_CONFIG,
            "renotify_interval_secs must be 0 (off) or at least 60",
            "renotify_interval_secs",
        ));
    }
    Ok(())
}

/// One confirmation minimum — alerting after zero failures is meaningless.
fn validate_alert_confirmations(n: Option<u32>) -> Result<()> {
    if matches!(n, Some(0)) {
        return Err(AppError::bad_request_field(
            codes::INVALID_ALERT_CONFIG,
            "alert_confirmations must be >= 1",
            "alert_confirmations",
        ));
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

/// Structural-only (no I/O): reject duplicate channel bindings. The bound
/// `channel_id` is checked to exist in the caller's org by the async
/// [`verify_alert_channels`] — kept separate so the sync per-item path
/// (`validate_new_target`, also used by bulk) stays sync.
fn validate_alerts(alerts: &TargetAlerts) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for (i, b) in alerts.iter().enumerate() {
        // A monitor delivers each open/resolve once per bound channel; a
        // duplicate binding would just double-page the same channel.
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

const MAX_QUORUM: u32 = 64;

/// `any`/`majority`/`all` (and `None`) always pass — they track the live region
/// count. A fixed `count` must be in `1..=min(64, region_count)`; a count larger
/// than the regions that exist can never be met.
fn validate_region_policy(policy: Option<RegionIncidentPolicy>, region_count: usize) -> Result<()> {
    let max = (region_count as u32).clamp(1, MAX_QUORUM);
    match policy {
        Some(RegionIncidentPolicy::Count(n)) if !(1..=max).contains(&n) => {
            Err(AppError::unprocessable(
                codes::INVALID_REGION_POLICY,
                format!("region count must be between 1 and {max}"),
            ))
        }
        _ => Ok(()),
    }
}

/// The default region set for a new monitor: every enabled region, capped at the
/// plan's `max_regions` (default region kept first when the cap bites), so the
/// default can never exceed the quota.
pub(crate) fn default_region_set(
    available: Vec<String>,
    max_regions: i32,
    default_region: &str,
) -> Vec<String> {
    let cap = max_regions.max(1) as usize;
    let set: Vec<String> = if available.len() <= cap {
        available
    } else {
        let mut v = vec![default_region.to_string()];
        for r in available {
            if r != default_region && v.len() < cap {
                v.push(r);
            }
        }
        v
    };
    if set.is_empty() {
        vec![default_region.to_string()]
    } else {
        set
    }
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

/// An empty-string credential means "clear"; an omitted one is kept.
fn take_cleared_credentials(http: &mut crate::domain::HttpCheck) -> (bool, bool) {
    let cleared_basic = matches!(&http.basic_auth, Some((u, p)) if u.is_empty() && p.is_empty());
    let cleared_bearer = matches!(http.bearer_token.as_deref(), Some(""));
    if cleared_basic {
        http.basic_auth = None;
    }
    if cleared_bearer {
        http.bearer_token = None;
    }
    (cleared_basic, cleared_bearer)
}

/// A credential carries from the stored target only when omitted and not cleared.
fn carry_flags(
    http: &crate::domain::HttpCheck,
    cleared_basic: bool,
    cleared_bearer: bool,
) -> (bool, bool) {
    (
        http.basic_auth.is_none() && !cleared_basic,
        http.bearer_token.is_none() && !cleared_bearer,
    )
}

fn carry_credentials(
    http: &mut crate::domain::HttpCheck,
    stored: &crate::domain::HttpCheck,
    carry_basic: bool,
    carry_bearer: bool,
) {
    if carry_basic {
        http.basic_auth = stored.basic_auth.clone();
    }
    if carry_bearer {
        http.bearer_token = stored.bearer_token.clone();
    }
}

/// Carry each masked (`***`) fill value forward from the stored flow, matched by
/// selector, so an edit that leaves a login secret untouched keeps it. A
/// sentinel with no stored match survives and `validate_check` rejects it, which
/// tells the user to re-enter that value.
fn carry_flow_secrets(new: &mut crate::domain::FlowCheck, stored: &crate::domain::FlowCheck) {
    use crate::domain::FlowStep;
    for step in &mut new.steps {
        if let FlowStep::Fill { selector, value } = step
            && value == REDACTED
            && let Some(FlowStep::Fill { value: prev, .. }) = stored
                .steps
                .iter()
                .find(|s| matches!(s, FlowStep::Fill { selector: ss, .. } if ss == selector))
        {
            *value = prev.clone();
        }
    }
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
        // Host lives in the URL, already normalized on parse.
        CheckSpec::Http(_) | CheckSpec::Heartbeat(_) | CheckSpec::Flow(_) => Ok(()),
        CheckSpec::Tcp(tcp) => canon_host(&mut tcp.host, "check.host", codes::INVALID_TCP_HOST),
        CheckSpec::Ping(p) => canon_host(&mut p.host, "check.host", codes::INVALID_PING_HOST),
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

// Zero = instant fail; the cap stops a tenant hogging a worker slot.
fn validate_timeout(timeout: std::time::Duration) -> Result<()> {
    if !(100..=60_000).contains(&timeout.as_millis()) {
        return Err(AppError::bad_request_field(
            codes::INVALID_TIMEOUT,
            "timeout must be between 100 and 60000 ms",
            "check.timeout",
        ));
    }
    Ok(())
}

// Bound the alert lead-time window; warn must stay above critical.
fn validate_cert_days(warn_days: u32, critical_days: u32, code: &'static str) -> Result<()> {
    for (val, field) in [
        (warn_days, "check.warn_days"),
        (critical_days, "check.critical_days"),
    ] {
        if !(1..=365).contains(&val) {
            return Err(AppError::bad_request_field(
                code,
                "warn_days and critical_days must be between 1 and 365",
                field,
            ));
        }
    }
    if warn_days <= critical_days {
        return Err(AppError::bad_request_field(
            code,
            "warn_days must be > critical_days",
            "check.warn_days",
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
            validate_timeout(http.timeout)?;
            if http.max_redirects > crate::domain::HttpCheck::MAX_REDIRECTS {
                return Err(AppError::bad_request_field(
                    codes::INVALID_HTTP_PARAMS,
                    format!(
                        "max_redirects must be at most {}",
                        crate::domain::HttpCheck::MAX_REDIRECTS
                    ),
                    "check.max_redirects",
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
            // Port 0 is unroutable, and rejecting it everywhere keeps the
            // ping throttle's pseudo-port 0 collision-free across kinds.
            if http.url.port() == Some(0) {
                return Err(AppError::bad_request_field(
                    codes::INVALID_URL_FORMAT,
                    "url port must be > 0",
                    "check.url",
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
            validate_timeout(tcp.timeout)?;
            let host = crate::security::unbracket(&tcp.host);
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_ip(ip, guard)?;
            }
        }
        CheckSpec::Ping(p) => {
            if p.host.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_PING_HOST,
                    "ping host required",
                    "check.host",
                ));
            }
            validate_timeout(p.timeout)?;
            let host = crate::security::unbracket(&p.host);
            if let Ok(ip) = host.parse::<IpAddr>() {
                check_ip(ip, guard)?;
            }
        }
        CheckSpec::Heartbeat(h) => {
            // Sub-minute periods can't be judged by once-a-minute evaluation;
            // the 30-day ceiling keeps the maths in range.
            const MAX_MS: u128 = 30 * 24 * 3_600 * 1_000;
            if !(60_000..=MAX_MS).contains(&h.period.as_millis()) {
                return Err(AppError::bad_request_field(
                    codes::INVALID_HEARTBEAT_PARAMS,
                    "heartbeat period must be between 1 minute and 30 days",
                    "check.period",
                ));
            }
            if h.grace.as_millis() > MAX_MS {
                return Err(AppError::bad_request_field(
                    codes::INVALID_HEARTBEAT_PARAMS,
                    "heartbeat grace must be at most 30 days",
                    "check.grace",
                ));
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
            validate_timeout(cert.timeout)?;
            validate_cert_days(
                cert.warn_days,
                cert.critical_days,
                codes::INVALID_TLS_CERT_PARAMS,
            )?;
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
            // Accepting a check that can never succeed would alert forever.
            if let Some(tld) = d.domain.rsplit('.').next()
                && !crate::worker::registration::is_monitorable(&tld.to_ascii_lowercase())
            {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DOMAIN_PARAMS,
                    format!(
                        "the .{} registry does not publish domain expiry dates, so this domain cannot be monitored for expiry",
                        tld.to_ascii_lowercase()
                    ),
                    "check.domain",
                ));
            }
            validate_timeout(d.timeout)?;
            validate_cert_days(d.warn_days, d.critical_days, codes::INVALID_DOMAIN_PARAMS)?;
        }
        CheckSpec::Dns(d) => {
            if d.domain.is_empty() {
                return Err(AppError::bad_request_field(
                    codes::INVALID_DNS_PARAMS,
                    "dns domain required",
                    "check.domain",
                ));
            }
            validate_timeout(d.timeout)?;
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
        CheckSpec::Flow(flow) => {
            use crate::domain::{FlowCheck, FlowStep};
            const FLOW: &str = codes::INVALID_FLOW_PARAMS;
            if !(1_000..=120_000).contains(&flow.timeout.as_millis()) {
                return Err(AppError::bad_request_field(
                    FLOW,
                    "flow timeout must be between 1000 and 120000 ms",
                    "check.timeout",
                ));
            }
            if !(100..=60_000).contains(&flow.step_timeout.as_millis()) {
                return Err(AppError::bad_request_field(
                    FLOW,
                    "flow step_timeout must be between 100 and 60000 ms",
                    "check.step_timeout",
                ));
            }
            if flow.steps.is_empty() {
                return Err(AppError::bad_request_field(
                    FLOW,
                    "flow requires at least one step",
                    "check.steps",
                ));
            }
            if flow.steps.len() > FlowCheck::MAX_STEPS {
                return Err(AppError::bad_request_field(
                    FLOW,
                    format!("flow allows at most {} steps", FlowCheck::MAX_STEPS),
                    "check.steps",
                ));
            }
            // No assertion → the flow can't fail, reporting Up even when login is
            // broken; require an explicit success signal.
            let asserts = flow
                .steps
                .iter()
                .any(|s| matches!(s, FlowStep::AssertText { .. } | FlowStep::AssertUrl { .. }));
            if !asserts {
                return Err(AppError::bad_request_field(
                    FLOW,
                    "flow requires at least one assert_text or assert_url step",
                    "check.steps",
                ));
            }
            validate_flow_url(&flow.start_url, guard)?;
            for step in &flow.steps {
                match step {
                    FlowStep::Goto { url } => validate_flow_url(url, guard)?,
                    FlowStep::Click { selector } | FlowStep::WaitFor { selector } => {
                        require_nonempty_step(selector)?
                    }
                    FlowStep::Fill { selector, value } => {
                        require_nonempty_step(selector)?;
                        if value == REDACTED {
                            return Err(AppError::bad_request_field(
                                codes::REDACTION_SENTINEL,
                                "fill value contains redaction sentinel — re-supply the real value",
                                "check.steps",
                            ));
                        }
                    }
                    FlowStep::AssertText { selector, contains } => {
                        if let Some(sel) = selector {
                            require_nonempty_step(sel)?;
                        }
                        require_nonempty_step(contains)?;
                    }
                    FlowStep::AssertUrl { contains } => require_nonempty_step(contains)?,
                }
            }
        }
    }
    Ok(())
}

/// Save-time scheme + host + SSRF gate for a flow's nav URLs, mirroring the HTTP
/// gate. Runtime egress is separately sandboxed in the engine.
fn validate_flow_url(url: &url::Url, guard: &SsrfGuard) -> Result<()> {
    let scheme = url.scheme();
    if !ALLOWED_SCHEMES.contains(&scheme) {
        return Err(AppError::bad_request_field(
            codes::INVALID_URL_SCHEME,
            format!("url scheme '{scheme}' not allowed"),
            "check.url",
        ));
    }
    if url.port() == Some(0) {
        return Err(AppError::bad_request_field(
            codes::INVALID_URL_FORMAT,
            "url port must be > 0",
            "check.url",
        ));
    }
    match url.host() {
        Some(Host::Ipv4(v4)) => check_ip(IpAddr::V4(v4), guard),
        Some(Host::Ipv6(v6)) => check_ip(IpAddr::V6(v6), guard),
        Some(Host::Domain(d)) if !d.is_empty() => Ok(()),
        _ => Err(AppError::bad_request_field(
            codes::INVALID_URL_FORMAT,
            "url missing host",
            "check.url",
        )),
    }
}

fn require_nonempty_step(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(AppError::bad_request_field(
            codes::INVALID_FLOW_PARAMS,
            "flow step selector and assertion values must not be empty",
            "check.steps",
        ));
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

    #[test]
    fn the_floor_skip_covers_every_kind() {
        for kind in CheckSpec::ALL_KINDS {
            assert!(
                MAX_KIND_FLOOR_SECS >= crate::domain::min_interval_secs_for_kind(kind) as i64,
                "{kind} floors above the skip, so PATCH would accept a rejected interval"
            );
        }
    }

    #[test]
    fn scrub_secrets_redacts_echoed_values_in_body_and_headers() {
        use crate::ad_hoc_dispatch::DeliveredResult;
        use crate::domain::CheckResult;
        use crate::domain::agent_wire::HeaderPreview;

        let mut delivered = DeliveredResult {
            result: CheckResult::error(Uuid::nil(), Uuid::nil(), "x"),
            response_body_snippet: Some("{\"echo\":\"sk-live-secret\"}".into()),
            response_headers_preview: vec![HeaderPreview {
                name: "x-echo".into(),
                value: "sk-live-secret".into(),
            }],
        };
        scrub_secrets(&mut delivered, &["sk-live-secret".to_string()]);
        assert_eq!(
            delivered.response_body_snippet.as_deref(),
            Some("{\"echo\":\"***\"}")
        );
        assert_eq!(delivered.response_headers_preview[0].value, "***");
    }

    #[test]
    fn secret_values_skips_short_and_plain() {
        use crate::domain::ResolvedVar;
        let mut vars = crate::domain::VarMap::new();
        vars.insert(
            "k".into(),
            ResolvedVar {
                value: "sk-live-secret".into(),
                is_secret: true,
            },
        );
        vars.insert(
            "short".into(),
            ResolvedVar {
                value: "ab".into(),
                is_secret: true,
            },
        );
        vars.insert(
            "plain".into(),
            ResolvedVar {
                value: "api.example.com".into(),
                is_secret: false,
            },
        );
        let secrets = secret_values(&vars);
        assert_eq!(secrets, vec!["sk-live-secret".to_string()]);
    }

    fn assert_bad_request_with_field(err: AppError, expected_field: &str) {
        match err {
            AppError::BadRequest { field: Some(f), .. } => assert_eq!(f, expected_field),
            other => panic!("expected BadRequest with field={expected_field}, got {other:?}"),
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
    fn validate_check_rejects_tcp_empty_port() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"tcp","host":"db.example.com","port":null,"timeout":3000}"#,
        )
        .expect("empty port must deserialize, not 422");
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("tcp check with empty port must reject");
        assert_bad_request_with_field(err, "check.port");
    }

    #[test]
    fn validate_check_rejects_tls_cert_empty_port() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"tls_cert","host":"example.com","port":null,"warn_days":14,"critical_days":3,"timeout":5000}"#,
        )
        .expect("empty port must deserialize, not 422");
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("tls_cert check with empty port must reject");
        assert_bad_request_with_field(err, "check.port");
    }

    #[test]
    fn validate_check_rejects_flow_without_assertion() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r##"{"type":"flow","start_url":"https://app.example.com/login","steps":[{"op":"fill","selector":"#pw","value":"x"},{"op":"click","selector":"#submit"}],"timeout":30000,"step_timeout":5000,"verify_tls":true}"##,
        )
        .expect("flow must deserialize");
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("a flow with no assertion can never fail and must reject");
        assert_bad_request_with_field(err, "check.steps");
    }

    #[test]
    fn validate_check_accepts_valid_login_flow() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r##"{"type":"flow","start_url":"https://app.example.com/login","steps":[{"op":"fill","selector":"#email","value":"u@x.com"},{"op":"fill","selector":"#pw","value":"{{password}}"},{"op":"click","selector":"#submit"},{"op":"assert_url","contains":"/dashboard"}],"timeout":30000,"step_timeout":5000,"verify_tls":true}"##,
        )
        .expect("flow must deserialize");
        validate_check(&spec, &SsrfGuard::strict()).expect("valid login flow must pass");
    }

    #[test]
    fn validate_check_flow_blocks_internal_goto() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r##"{"type":"flow","start_url":"https://app.example.com/login","steps":[{"op":"goto","url":"http://169.254.169.254/latest/meta-data/"},{"op":"assert_text","selector":null,"contains":"x"}],"timeout":30000,"step_timeout":5000,"verify_tls":true}"##,
        )
        .expect("flow must deserialize");
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("a goto to a metadata IP must be SSRF-blocked");
        assert_bad_request_with_field(err, "check.url");
    }

    #[test]
    fn validate_check_rejects_http_timeout_over_max() {
        use crate::domain::CheckSpec;
        use crate::security::ssrf::SsrfGuard;
        use std::time::Duration;
        let mut h = http_auth(None, None);
        h.timeout = Duration::from_millis(999_999);
        let err = validate_check(&CheckSpec::Http(h), &SsrfGuard::strict())
            .expect_err("http timeout above 60000 ms must reject");
        assert_bad_request_with_field(err, "check.timeout");
    }

    #[test]
    fn validate_check_rejects_http_zero_timeout() {
        use crate::domain::CheckSpec;
        use crate::security::ssrf::SsrfGuard;
        use std::time::Duration;
        let mut h = http_auth(None, None);
        h.timeout = Duration::ZERO;
        let err = validate_check(&CheckSpec::Http(h), &SsrfGuard::strict())
            .expect_err("zero timeout must reject");
        assert_bad_request_with_field(err, "check.timeout");
    }

    #[test]
    fn validate_check_rejects_excess_redirects() {
        use crate::domain::CheckSpec;
        use crate::security::ssrf::SsrfGuard;
        let mut h = http_auth(None, None);
        h.max_redirects = crate::domain::HttpCheck::MAX_REDIRECTS + 1;
        let err = validate_check(&CheckSpec::Http(h), &SsrfGuard::strict())
            .expect_err("max_redirects above the ceiling must reject");
        assert_bad_request_with_field(err, "check.max_redirects");
    }

    #[test]
    fn validate_check_rejects_tcp_timeout_out_of_range() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"tcp","host":"db.example.com","port":5432,"timeout":999999}"#,
        )
        .unwrap();
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("tcp timeout above 60000 ms must reject");
        assert_bad_request_with_field(err, "check.timeout");
    }

    #[test]
    fn validate_check_rejects_tls_warn_days_over_max() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"tls_cert","host":"example.com","port":443,"warn_days":999,"critical_days":3,"timeout":5000}"#,
        )
        .unwrap();
        let err = validate_check(&spec, &SsrfGuard::strict())
            .expect_err("warn_days above 365 must reject");
        assert_bad_request_with_field(err, "check.warn_days");
    }

    #[test]
    fn validate_check_accepts_tls_days_in_range() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"tls_cert","host":"example.com","port":443,"warn_days":30,"critical_days":7,"timeout":5000}"#,
        )
        .unwrap();
        validate_check(&spec, &SsrfGuard::strict()).expect("valid tls days must pass");
    }

    #[test]
    fn validate_check_rejects_domain_expiry_zero_days() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"domain_expiry","domain":"example.com","warn_days":0,"critical_days":0,"timeout":5000}"#,
        )
        .unwrap();
        let err = validate_check(&spec, &SsrfGuard::strict()).expect_err("zero days must reject");
        assert_bad_request_with_field(err, "check.warn_days");
    }

    #[test]
    fn validate_check_rejects_domain_expiry_for_registry_without_public_expiry() {
        use crate::security::ssrf::SsrfGuard;
        for domain in ["denic.DE", "europa.eu"] {
            let spec: crate::domain::CheckSpec = serde_json::from_str(&format!(
                r#"{{"type":"domain_expiry","domain":"{domain}","warn_days":30,"critical_days":7,"timeout":5000}}"#
            ))
            .unwrap();
            let err = validate_check(&spec, &SsrfGuard::strict())
                .expect_err("registry without public expiry must reject");
            assert_bad_request_with_field(err, "check.domain");
        }
    }

    #[test]
    fn validate_check_accepts_domain_expiry_for_whois_only_tld() {
        use crate::security::ssrf::SsrfGuard;
        let spec: crate::domain::CheckSpec = serde_json::from_str(
            r#"{"type":"domain_expiry","domain":"postyourstartup.co","warn_days":30,"critical_days":7,"timeout":5000}"#,
        )
        .unwrap();
        validate_check(&spec, &SsrfGuard::strict()).expect("whois-only TLD is monitorable");
    }

    fn http_auth(basic: Option<(&str, &str)>, bearer: Option<&str>) -> crate::domain::HttpCheck {
        use crate::domain::{ExpectedStatus, HttpCheck, HttpMethod};
        use std::time::Duration;
        HttpCheck {
            url: "https://example.com/".parse().unwrap(),
            method: HttpMethod::Get,
            timeout: Duration::from_secs(5),
            follow_redirects: false,
            max_redirects: 0,
            expected_status: ExpectedStatus::Exact(200),
            expected_body_contains: None,
            headers: Default::default(),
            body: None,
            verify_tls: true,
            basic_auth: basic.map(|(u, p)| (u.to_owned(), p.to_owned())),
            bearer_token: bearer.map(str::to_owned),
        }
    }

    #[test]
    fn credentials_empty_sentinel_clears() {
        let mut h = http_auth(Some(("", "")), Some(""));
        let (cb, cbr) = take_cleared_credentials(&mut h);
        assert!(cb && cbr);
        assert!(h.basic_auth.is_none() && h.bearer_token.is_none());
    }

    #[test]
    fn credentials_omitted_carry_from_stored() {
        let stored = http_auth(Some(("u", "p")), Some("tok"));
        let mut incoming = http_auth(None, None);
        let (cb, cbr) = take_cleared_credentials(&mut incoming);
        let (carry_basic, carry_bearer) = carry_flags(&incoming, cb, cbr);
        carry_credentials(&mut incoming, &stored, carry_basic, carry_bearer);
        assert_eq!(incoming.basic_auth, Some(("u".into(), "p".into())));
        assert_eq!(incoming.bearer_token.as_deref(), Some("tok"));
    }

    #[test]
    fn credentials_cleared_not_carried_back() {
        let stored = http_auth(Some(("u", "p")), Some("tok"));
        let mut incoming = http_auth(Some(("", "")), Some(""));
        let (cb, cbr) = take_cleared_credentials(&mut incoming);
        let (carry_basic, carry_bearer) = carry_flags(&incoming, cb, cbr);
        carry_credentials(&mut incoming, &stored, carry_basic, carry_bearer);
        assert!(incoming.basic_auth.is_none() && incoming.bearer_token.is_none());
    }

    #[test]
    fn credentials_replacement_kept_over_stored() {
        let stored = http_auth(Some(("u", "p")), Some("old"));
        let mut incoming = http_auth(Some(("new", "pw")), Some("new"));
        let (cb, cbr) = take_cleared_credentials(&mut incoming);
        let (carry_basic, carry_bearer) = carry_flags(&incoming, cb, cbr);
        carry_credentials(&mut incoming, &stored, carry_basic, carry_bearer);
        assert_eq!(incoming.basic_auth, Some(("new".into(), "pw".into())));
        assert_eq!(incoming.bearer_token.as_deref(), Some("new"));
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
