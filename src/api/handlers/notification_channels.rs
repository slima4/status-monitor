//! Operator endpoints for notification-channel CRUD + a send-test action.
//!
//! Standard `ApiError` envelope. Mounted under `/api/v1/notification-channels`.
//! Every handler resolves the caller's tenant via the scope-gated
//! `Authorized<…>` extractor (which wraps `CurrentOrg`) and threads the org
//! into the store, so a channel is only ever visible to its owning org.
//! Secrets are sealed at rest by the store and are never echoed back: every
//! read path returns through [`Redacted`].

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::AppendHeaders;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::ApiError;
use crate::api::error::codes;
use crate::api::redaction::Redacted;
use crate::app::AppState;
use crate::auth::sha256_hex;
use crate::auth::token_hash::generate_raw_token;
use crate::domain::{
    ChannelConfig, IncidentSeverity, IncidentUrgency, NewNotificationChannel, NotificationChannel,
    NotificationChannelUpdate, NotificationReason, validate_channel_name,
};
use crate::error::{AppError, Result};
use crate::notifier::build_notifier;
use crate::notifier::event::IncidentNotice;
use crate::storage::{LinkCodeStatus, MintOutcome};
use crate::web::{
    Authorized, ChannelsDelete, ChannelsExecute, ChannelsRead, ChannelsWrite, CurrentUser,
    RequestSource,
};

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
    Authorized(org, _): Authorized<ChannelsWrite>,
    RequestSource(source): RequestSource,
    Json(new): Json<NewNotificationChannel>,
) -> Result<(
    StatusCode,
    AppendHeaders<[(axum::http::HeaderName, HeaderValue); 1]>,
    Redacted<NotificationChannel>,
)> {
    validate_name(&new.name)?;
    reject_managed_kind(&new.config)?;
    validate_config(&new.config)?;
    check_channel_abuse(&state, org, &new.config)?;
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
        .create(org, new, source, limit)
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
    Authorized(org, _): Authorized<ChannelsRead>,
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
    Authorized(org, _): Authorized<ChannelsRead>,
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
    Authorized(org, _): Authorized<ChannelsWrite>,
    RequestSource(source): RequestSource,
    Path(id): Path<Uuid>,
    Json(update): Json<NotificationChannelUpdate>,
) -> Result<Redacted<NotificationChannel>> {
    if let Some(name) = &update.name {
        validate_name(name)?;
    }
    if let Some(cfg) = &update.config {
        reject_managed_kind(cfg)?;
        validate_config(cfg)?;
        check_channel_abuse(&state, org, cfg)?;
    }
    let updated = state
        .notification_channel_store
        .update(org, id, update, source)
        .await?
        .ok_or_else(channel_not_found)?;
    Ok(Redacted::new(updated))
}

#[utoipa::path(
    delete,
    path = "/api/v1/notification-channels/{id}",
    tag = "notification-channels",
    summary = "Delete a notification channel",
    description = "The channel's alert bindings are removed from every \
                   monitor; re-adding the channel later starts unbound.",
    params(("id" = Uuid, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = ApiError),
    ),
)]
pub async fn delete(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<ChannelsDelete>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    // The bot-leave decision below needs the transport + chat id, gone once
    // the row is.
    let channel = state.notification_channel_store.get(org, id).await?;
    // Scrub before delete: a stale {channel_id} left in targets.alerts
    // fails channel-existence validation on the next whole-array alerts
    // update of that monitor. This order keeps the operation retryable —
    // a scrub failure leaves the channel in place, so the client's DELETE
    // retry re-runs both steps. A PATCH racing the delete can still
    // re-introduce a binding; the escalation engine tolerates missing
    // channels at resolve time.
    state.target_store.unbind_channel(org, id).await?;
    if state.notification_channel_store.delete(org, id).await? {
        if let Some(ch) = channel {
            maybe_leave_telegram_group(&state, &ch);
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(channel_not_found())
    }
}

/// Best-effort: deleting the last channel linked to a Telegram group walks
/// the bot out; it stays while another org's channel still points there.
/// A re-link racing the count can lose the bot — the chat just re-invites it.
fn maybe_leave_telegram_group(state: &AppState, ch: &NotificationChannel) {
    let ChannelConfig::TelegramApp(cfg) = &ch.config else {
        return;
    };
    let Ok(chat_id) = cfg.chat_id.parse::<i64>() else {
        return;
    };
    let Some(token) = state.cfg.telegram.delivery_token() else {
        return;
    };
    if chat_id >= 0 {
        return;
    }
    let client = crate::telegram::TelegramClient::new(state.outbound_http.clone(), token);
    let store = state.notification_channel_store.clone();
    let external_ref = cfg.chat_id.clone();
    tokio::spawn(async move {
        match store
            .count_by_external_ref(crate::domain::ChannelKind::TelegramApp, &external_ref)
            .await
        {
            Ok(0) => {
                if let Err(err) = client.leave_chat(chat_id).await {
                    tracing::warn!(?err, chat_id, "telegram leaveChat failed");
                }
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, chat_id, "telegram leave check failed"),
        }
    });
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
    Authorized(org, _): Authorized<ChannelsExecute>,
    Path(id): Path<Uuid>,
) -> Result<Json<TestNotificationResponse>> {
    let channel = state
        .notification_channel_store
        .get(org, id)
        .await?
        .ok_or_else(channel_not_found)?;
    // A stored config can predate a deny-list entry — gate the test too.
    check_channel_abuse(&state, org, &channel.config)?;
    deliver_test(&state, &channel.config).await?;
    Ok(Json(TestNotificationResponse { delivered: true }))
}

/// Body of `POST /test`: a full transport config to exercise without saving.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TestChannelConfigRequest {
    pub config: ChannelConfig,
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels/test",
    tag = "notification-channels",
    summary = "Send a test alert through an unsaved transport config",
    description = "Delivers one clearly-labelled test alert through the config \
                   in the request body without persisting anything, so the \
                   operator can verify a webhook/token before creating or \
                   saving a channel. The config is validated exactly as on \
                   create.",
    request_body(content = TestChannelConfigRequest, example = json!({
        "config": {
            "type": "slack",
            "webhook_url": "https://hooks.slack.com/services/T000/B000/XXXX"
        }
    })),
    responses(
        (status = 200, body = TestNotificationResponse),
        (status = 400, body = ApiError, description = "Invalid transport config or a redaction sentinel in place of a real secret"),
        (status = 422, body = ApiError, description = "The transport rejected the test delivery"),
    ),
)]
pub async fn test_config(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<ChannelsExecute>,
    Json(req): Json<TestChannelConfigRequest>,
) -> Result<Json<TestNotificationResponse>> {
    // Same spam vector as create: the test would message a caller-supplied
    // chat id with the operator bot.
    reject_managed_kind(&req.config)?;
    validate_config(&req.config)?;
    check_channel_abuse(&state, org, &req.config)?;
    deliver_test(&state, &req.config).await?;
    Ok(Json(TestNotificationResponse { delivered: true }))
}

// ── Central-bot Telegram linking ─────────────────────────────────────────

const TELEGRAM_LINK_TTL_MINUTES: i64 = 15;
/// Outstanding codes per org — bounds drive-by minting.
const TELEGRAM_LINK_MAX_OUTSTANDING: i64 = 5;

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct TelegramLinkRequest {
    /// Optional name for the channel created when the code is consumed;
    /// defaults to the linked chat's title.
    #[serde(default)]
    #[schema(example = "Ops Telegram", max_length = 100)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TelegramLinkResponse {
    /// Poll handle for `GET /telegram-link/{id}`.
    pub id: Uuid,
    /// The raw single-use code — shown once, never stored.
    pub code: String,
    /// `https://t.me/<bot>?start=<code>` — links a private chat.
    pub deep_link: String,
    /// `https://t.me/<bot>?startgroup=<code>` — adds the bot to a group the
    /// user picks and links it. Same code; whichever chat consumes it wins.
    pub group_deep_link: String,
    pub expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TelegramLinkStatusResponse {
    /// `pending`, `consumed`, or `expired`.
    pub status: &'static str,
    /// The created channel, present once `status` is `consumed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<Uuid>,
}

#[utoipa::path(
    post,
    path = "/api/v1/notification-channels/telegram-link",
    tag = "notification-channels",
    summary = "Mint a single-use Telegram link code",
    description = "Returns a short-lived code wrapped in a t.me deep link. \
                   Sending it to the bot (tap Start) creates a `telegram_app` \
                   channel for the chat. 404 on deployments without a central \
                   bot configured.",
    request_body(content = TelegramLinkRequest, example = json!({ "name": "Ops Telegram" })),
    responses(
        (status = 201, body = TelegramLinkResponse),
        (status = 400, body = ApiError, description = "Invalid channel-name hint"),
        (status = 422, body = ApiError, description = "Too many outstanding link codes"),
    ),
)]
pub async fn telegram_link_mint(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<ChannelsWrite>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<TelegramLinkRequest>,
) -> Result<(StatusCode, Json<TelegramLinkResponse>)> {
    require_central_bot(&state)?;
    let name = match req.name.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(n) => {
            validate_name(n)?;
            Some(n.to_string())
        }
    };
    let code = generate_raw_token();
    let expires_at = Utc::now() + chrono::Duration::minutes(TELEGRAM_LINK_TTL_MINUTES);
    let outcome = state
        .telegram_link_code_store
        .mint(
            org,
            Some(user),
            &sha256_hex(&code),
            name.as_deref(),
            expires_at,
            TELEGRAM_LINK_MAX_OUTSTANDING,
        )
        .await?;
    let minted = match outcome {
        MintOutcome::Created(c) => c,
        MintOutcome::LimitReached => {
            return Err(AppError::unprocessable(
                codes::TELEGRAM_LINK_LIMIT,
                "too many outstanding link codes; wait for one to expire or complete a pending link",
            ));
        }
    };
    let bot = state.cfg.telegram.bot_username.trim_start_matches('@');
    Ok((
        StatusCode::CREATED,
        Json(TelegramLinkResponse {
            id: minted.id,
            deep_link: format!("https://t.me/{bot}?start={code}"),
            group_deep_link: format!("https://t.me/{bot}?startgroup={code}"),
            code,
            expires_at: minted.expires_at,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/notification-channels/telegram-link/{id}",
    tag = "notification-channels",
    summary = "Poll a Telegram link code",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, body = TelegramLinkStatusResponse),
        (status = 404, body = ApiError),
    ),
)]
pub async fn telegram_link_status(
    State(state): State<AppState>,
    Authorized(org, _): Authorized<ChannelsRead>,
    Path(id): Path<Uuid>,
) -> Result<Json<TelegramLinkStatusResponse>> {
    require_central_bot(&state)?;
    let status = state
        .telegram_link_code_store
        .status(org, id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(codes::TELEGRAM_LINK_NOT_FOUND, "link code not found")
        })?;
    Ok(Json(match status {
        LinkCodeStatus::Pending => TelegramLinkStatusResponse {
            status: "pending",
            channel_id: None,
        },
        LinkCodeStatus::Consumed { channel_id } => TelegramLinkStatusResponse {
            status: "consumed",
            channel_id: Some(channel_id),
        },
        LinkCodeStatus::Expired => TelegramLinkStatusResponse {
            status: "expired",
            channel_id: None,
        },
    }))
}

/// One synthetic, clearly-labelled delivery through `config`'s transport.
/// Shared by the saved-channel and ad-hoc test endpoints so both exercise
/// the exact notifier path real incidents use.
async fn deliver_test(state: &AppState, config: &ChannelConfig) -> Result<()> {
    let central =
        state
            .cfg
            .telegram
            .delivery_token()
            .map(|bot_token| crate::notifier::CentralTelegram {
                bot_token,
                budget: &state.telegram_send_budget,
            });
    let notifier = build_notifier(config, &state.outbound_http, central)?;
    let notice = IncidentNotice {
        incident_id: Uuid::nil(),
        reason: NotificationReason::Opened,
        monitor_name: Some("uptimepage test notification".to_string()),
        title: None,
        severity: IncidentSeverity::Minor,
        urgency: IncidentUrgency::Low,
        started_at: Utc::now(),
        ended_at: None,
        error_sample: Some(
            "This is a test notification confirming the channel is configured correctly."
                .to_string(),
        ),
        regions_down: Vec::new(),
        regions_up: Vec::new(),
        url: None,
    };
    notifier.notify_incident(&notice).await.map_err(|e| {
        AppError::unprocessable(
            codes::CHANNEL_TEST_FAILED,
            format!("test delivery failed: {e}"),
        )
    })
}

// ── Validation ──────────────────────────────────────────────────────────

fn channel_not_found() -> AppError {
    AppError::not_found(codes::CHANNEL_NOT_FOUND, "notification channel not found")
}

/// Without a central bot the linking surface answers as absent.
fn require_central_bot(state: &AppState) -> Result<()> {
    if state.cfg.telegram.enabled() {
        Ok(())
    } else {
        Err(AppError::not_found(
            codes::TELEGRAM_LINK_NOT_FOUND,
            "telegram linking is not available on this deployment",
        ))
    }
}

/// A caller-supplied operator-managed config (`telegram_app` chat id) would
/// let anyone alert-spam an arbitrary destination with our credentials —
/// only the transport's own flow may mint one.
fn reject_managed_kind(cfg: &ChannelConfig) -> Result<()> {
    if cfg.operator_managed() {
        return Err(AppError::unprocessable(
            codes::CHANNEL_KIND_MANAGED,
            "telegram channels are created by linking a chat through the bot; \
             mint a link code instead of supplying config",
        ));
    }
    Ok(())
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

/// Deny-list gate for the config's outbound URL, mirroring the targets
/// test path: a hit is recorded as an `abuse_blocked` quota event and
/// rejected. Transports with a fixed vendor endpoint expose no URL and
/// pass through.
fn check_channel_abuse(
    state: &AppState,
    org: crate::domain::OrgId,
    config: &ChannelConfig,
) -> Result<()> {
    let Some(url) = config.abuse_url() else {
        return Ok(());
    };
    let Some(hit) = state.abuse.inspect_url(url) else {
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
