//! Operator endpoints for notification-channel CRUD + a send-test action.
//!
//! Standard `ApiError` envelope. Mounted under `/api/v1/notification-channels`.
//! Every handler resolves the caller's tenant via [`CurrentOrg`] and threads
//! it into the store, so a channel is only ever visible to its owning org.
//! Secrets are sealed at rest by the store and are never echoed back: every
//! read path returns through [`Redacted`].

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::AppendHeaders;
use chrono::Utc;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::redaction::Redacted;
use crate::app::AppState;
use crate::domain::{
    CheckStatus, NewNotificationChannel, NotificationChannel, NotificationChannelUpdate,
    validate_channel_name,
};
use crate::error::{AppError, Result};
use crate::notifier::build_notifier;
use crate::notifier::event::{AlertEvent, AlertKind};
use crate::web::CurrentOrg;

/// Result of `POST /{id}/test`. A `false` never reaches the client — a failed
/// delivery is a 422 (`CHANNEL_TEST_FAILED`) — but the explicit field keeps
/// the success body self-describing for API consumers.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TestNotificationResponse {
    pub delivered: bool,
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels",
    tag = "notification-channels",
    summary = "Create a notification channel",
    request_body(content = NewNotificationChannel, example = json!({
        "name": "Ops Slack",
        "config": {
            "type": "slack",
            "webhook_url": "https://hooks.slack.com/services/T000/B000/XXXX"
        },
        "enabled": true
    })),
    responses(
        (status = 201, body = NotificationChannel,
            headers(("Location" = String, description = "URL of the new channel"))),
        (status = 400, body = ApiError,
            description = "Invalid name, invalid transport config, or the redaction \
                           sentinel was re-submitted in place of a real secret"),
        (status = 422, body = ApiError,
            description = "Channel name already in use, or the plan's channel limit \
                           is reached"),
    ),
)]
pub async fn create(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Json(new): Json<NewNotificationChannel>,
) -> Result<(
    StatusCode,
    AppendHeaders<[(axum::http::HeaderName, HeaderValue); 1]>,
    Redacted<NotificationChannel>,
)> {
    validate_name(&new.name)?;
    validate_config(&new.config)?;
    // Friendly pre-check; the store INSERT enforces the same cap atomically
    // under a per-org advisory lock.
    state
        .quotas
        .check_can_create_notification_channel(org, None)
        .await?;
    let limit = i64::from(
        state
            .quotas
            .limit_for_org(org)
            .await?
            .max_notification_channels,
    );
    let ch = state
        .notification_channel_store
        .create(org, new, limit)
        .await?;
    let location = HeaderValue::from_str(&format!("/api/v1/notification-channels/{}", ch.id))
        .expect("uuid produces ascii-only path");
    Ok((
        StatusCode::CREATED,
        AppendHeaders([(header::LOCATION, location)]),
        Redacted::new(ch),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/notification-channels",
    tag = "notification-channels",
    summary = "List notification channels",
    responses(
        (status = 200, body = [NotificationChannel]),
    ),
)]
pub async fn list(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
) -> Result<Redacted<Vec<NotificationChannel>>> {
    Ok(Redacted::new(
        state.notification_channel_store.list(org).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/notification-channels/{id}",
    tag = "notification-channels",
    summary = "Get a notification channel",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = NotificationChannel),
        (status = 404, body = ApiError),
    ),
)]
pub async fn get(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
) -> Result<Redacted<NotificationChannel>> {
    state
        .notification_channel_store
        .get(org, id)
        .await?
        .map(Redacted::new)
        .ok_or_else(channel_not_found)
}

#[utoipa::path(
    patch,
    path = "/api/v1/notification-channels/{id}",
    tag = "notification-channels",
    summary = "Edit a notification channel",
    description = "Omit fields you don't want to change. A `config` that still \
                   carries the `***` sentinel returns 400 — omit `config` to \
                   keep the stored secret unchanged.",
    params(("id" = Uuid, Path)),
    request_body(content = NotificationChannelUpdate),
    responses(
        (status = 200, body = NotificationChannel),
        (status = 400, body = ApiError),
        (status = 404, body = ApiError),
        (status = 422, body = ApiError, description = "Channel name already in use"),
    ),
)]
pub async fn update(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
    Json(update): Json<NotificationChannelUpdate>,
) -> Result<Redacted<NotificationChannel>> {
    if let Some(name) = &update.name {
        validate_name(name)?;
    }
    if let Some(cfg) = &update.config {
        validate_config(cfg)?;
    }
    let updated = state
        .notification_channel_store
        .update(org, id, update)
        .await?
        .ok_or_else(channel_not_found)?;
    // Drop any cached resolution so the next AlertEngine dispatch picks up
    // the new URL / token / enabled flag instead of waiting out the TTL.
    state.alert_channel_cache.invalidate(org, id);
    Ok(Redacted::new(updated))
}

#[utoipa::path(
    delete,
    path = "/api/v1/notification-channels/{id}",
    tag = "notification-channels",
    summary = "Delete a notification channel",
    description = "Targets bound to a deleted channel simply stop alerting \
                   through it; the binding is ignored at resolve time.",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = ApiError),
    ),
)]
pub async fn delete(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    if state.notification_channel_store.delete(org, id).await? {
        state.alert_channel_cache.invalidate(org, id);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(channel_not_found())
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels/{id}/test",
    tag = "notification-channels",
    summary = "Send a synthetic test alert to a notification channel",
    description = "Delivers one clearly-labelled test alert through the \
                   channel's transport so the operator can confirm the \
                   webhook/token works before binding targets to it. Works on \
                   a disabled channel too.",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = TestNotificationResponse),
        (status = 404, body = ApiError),
        (status = 422, body = ApiError, description = "The channel's transport rejected the test delivery"),
    ),
)]
pub async fn test_send(
    State(state): State<AppState>,
    CurrentOrg(org): CurrentOrg,
    Path(id): Path<Uuid>,
) -> Result<Json<TestNotificationResponse>> {
    let channel = state
        .notification_channel_store
        .get(org, id)
        .await?
        .ok_or_else(channel_not_found)?;
    let notifier = build_notifier(&channel.config, &state.outbound_http)?;
    let event = AlertEvent {
        target_id: Uuid::nil(),
        target_name: "uptimepage test notification".to_string(),
        kind: AlertKind::Down,
        consecutive_failures: 1,
        last_status: CheckStatus::Down,
        last_error: Some(
            "This is a test notification confirming the channel is configured correctly."
                .to_string(),
        ),
        timestamp: Utc::now(),
    };
    notifier.notify(&event).await.map_err(|e| {
        AppError::unprocessable(
            codes::CHANNEL_TEST_FAILED,
            format!("test delivery failed: {e}"),
        )
    })?;
    Ok(Json(TestNotificationResponse { delivered: true }))
}

// ── Validation ──────────────────────────────────────────────────────────

fn channel_not_found() -> AppError {
    AppError::not_found(codes::CHANNEL_NOT_FOUND, "notification channel not found")
}

fn validate_name(name: &str) -> Result<()> {
    validate_channel_name(name)
        .map_err(|m| AppError::bad_request_field(codes::CHANNEL_NAME_INVALID, m, "name"))
}

/// Redaction-sentinel guard first (so a `GET → PATCH` round-trip or a
/// copy-pasted redacted create reports `REDACTION_SENTINEL`, not a generic
/// invalid-URL — `***` does not parse as a URL), then the structural
/// transport check.
fn validate_config(cfg: &crate::domain::ChannelConfig) -> Result<()> {
    if cfg.has_redaction_sentinel() {
        return Err(AppError::bad_request_field(
            codes::REDACTION_SENTINEL,
            "config still contains the redaction sentinel; send the real secret \
             or omit config to keep the stored value",
            "config",
        ));
    }
    cfg.validate()
        .map_err(|m| AppError::bad_request_field(codes::INVALID_CHANNEL_CONFIG, m, "config"))?;
    Ok(())
}
