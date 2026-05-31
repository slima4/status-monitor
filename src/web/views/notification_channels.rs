//! Server-rendered notification-channel pages under `/settings/notifications`:
//! a list (with send-test and delete row actions) and a create/edit form.
//!
//! Mutations are driven from the page against the JSON API
//! (`/api/v1/notification-channels`), so this module only renders chrome and
//! prefills the form. The org is resolved by [`CurrentOrg`] exactly as the API
//! resolves it, so the UI always acts on the caller's own tenant.
//!
//! Secrets never reach the browser: the edit form prefills config from the
//! redacted channel (every secret-bearing field is `***`). Editing config
//! therefore goes through a single "Replace transport config" toggle — when
//! off, the form omits `config` from the PATCH and the stored secret is kept;
//! when on, the operator re-enters the whole config (the API rejects a
//! re-submitted `***`).

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{ChannelConfig, NotificationChannel};
use crate::error::AppError;
use crate::web::CurrentOrg;
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::views::{json_pretty, resolve_org};

const TAB_NOTIFICATIONS: &str = "notifications";

pub struct ChannelRow {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub enabled: bool,
    pub created: chrono::DateTime<chrono::Utc>,
    /// `terraform`/`api` chip for externally-managed channels; `None` (UI) hides it.
    pub managed_by: Option<&'static str>,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/notifications.html")]
pub struct ChannelsPage {
    pub active_tab: &'static str,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/notifications_partial.html")]
pub struct ChannelsPartial {
    pub channels: Vec<ChannelRow>,
}

/// One config field group. Only the fields for the selected `kind` are
/// submitted (the JS prunes the rest); on edit, secret-bearing fields carry
/// the `***` sentinel until the operator opts to replace the config.
pub struct ConfigFields {
    pub slack_webhook_url: String,
    pub webhook_url: String,
    pub webhook_headers_json: String,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
}

impl Default for ConfigFields {
    fn default() -> Self {
        Self {
            slack_webhook_url: String::new(),
            webhook_url: String::new(),
            webhook_headers_json: "{}".into(),
            telegram_bot_token: String::new(),
            telegram_chat_id: String::new(),
        }
    }
}

pub struct ChannelFormModel {
    /// `"create"` or `"edit"`. Drives the heading, submit verb, and (when not
    /// `"create"`) the "Replace transport config" toggle.
    pub mode: &'static str,
    pub action: String,
    pub submit_method: &'static str,
    pub name: String,
    pub enabled: bool,
    pub kind: &'static str,
    pub config: ConfigFields,
}

#[derive(Template, WebTemplate)]
#[template(path = "settings/notification_form.html")]
pub struct ChannelFormPage {
    pub active_tab: &'static str,
    pub form: ChannelFormModel,
}

pub async fn index(org: Result<CurrentOrg, AppError>) -> Response {
    match resolve_org(org, "/settings/notifications") {
        Ok(_) => ChannelsPage {
            active_tab: TAB_NOTIFICATIONS,
        }
        .into_response(),
        Err(resp) => *resp,
    }
}

pub async fn list_partial(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
) -> WebResult<Response> {
    let org = match resolve_org(org, "/settings/notifications") {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let channels = state
        .notification_channel_store
        .list(org)
        .await?
        .into_iter()
        .map(|c| ChannelRow {
            id: c.id.to_string(),
            name: c.name,
            kind: c.kind.as_db_str(),
            enabled: c.enabled,
            created: c.created_at,
            managed_by: c.write_source.managed_label(),
        })
        .collect();
    Ok(ChannelsPartial { channels }.into_response())
}

pub async fn new_form(org: Result<CurrentOrg, AppError>) -> Response {
    match resolve_org(org, "/settings/notifications/new") {
        Ok(_) => ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form: empty_create_form(),
        }
        .into_response(),
        Err(resp) => *resp,
    }
}

pub async fn edit_form(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
    Path(id): Path<Uuid>,
) -> WebResult<Response> {
    let org = match resolve_org(org, &format!("/settings/notifications/{id}/edit")) {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let channel = state
        .notification_channel_store
        .get(org, id)
        .await?
        .ok_or_else(|| {
            AppError::not_found("CHANNEL_NOT_FOUND", "notification channel not found")
        })?;
    Ok(ChannelFormPage {
        active_tab: TAB_NOTIFICATIONS,
        form: form_from_channel(channel),
    }
    .into_response())
}

fn empty_create_form() -> ChannelFormModel {
    ChannelFormModel {
        mode: "create",
        action: "/api/v1/notification-channels".into(),
        submit_method: "POST",
        name: String::new(),
        enabled: true,
        kind: "slack",
        config: ConfigFields::default(),
    }
}

fn form_from_channel(c: NotificationChannel) -> ChannelFormModel {
    // Prefill from the *redacted* config so non-secret routing (header names,
    // chat id) survives the edit while secrets stay masked.
    let mut redacted = c.config.clone();
    redacted.redact_in_place();
    let mut config = ConfigFields::default();
    match redacted {
        ChannelConfig::Slack { webhook_url } => config.slack_webhook_url = webhook_url,
        ChannelConfig::Webhook { url, headers } => {
            config.webhook_url = url;
            config.webhook_headers_json = json_pretty(&headers);
        }
        ChannelConfig::Telegram { bot_token, chat_id } => {
            config.telegram_bot_token = bot_token;
            config.telegram_chat_id = chat_id;
        }
    }
    ChannelFormModel {
        mode: "edit",
        action: format!("/api/v1/notification-channels/{}", c.id),
        submit_method: "PATCH",
        name: c.name,
        enabled: c.enabled,
        kind: c.kind.as_db_str(),
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_form_renders_empty_create() {
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form: empty_create_form(),
        }
        .render()
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("New notification channel"));
        assert!(html.contains(r#"data-action="/api/v1/notification-channels""#));
        assert!(html.contains(r#"data-method="POST""#));
        assert!(html.contains(r#"data-mode="create""#));
        // Create has no "replace config" toggle — config is always sent.
        assert!(!html.contains("Replace transport config"));
    }

    #[test]
    fn edit_form_prefills_redacted_config_and_replace_toggle() {
        use crate::domain::ChannelKind;
        use chrono::Utc;
        let ch = NotificationChannel {
            id: Uuid::nil(),
            name: "Ops".into(),
            kind: ChannelKind::Slack,
            config: ChannelConfig::Slack {
                webhook_url: "https://hooks.slack.com/services/T/B/zzUNIQUESECRETzz".into(),
            },
            enabled: true,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let form = form_from_channel(ch);
        assert_eq!(form.submit_method, "PATCH");
        assert_eq!(form.mode, "edit");
        // Secret comes back masked, never the real webhook.
        assert_eq!(form.config.slack_webhook_url, "***");
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains("Replace transport config"));
        assert!(html.contains(r#"data-method="PATCH""#));
        // The real webhook never reaches the browser — only the `***` mask.
        assert!(!html.contains("zzUNIQUESECRETzz"));
        assert!(html.contains(r#"value="***""#));
    }

    #[test]
    fn channels_partial_renders_rows_and_empty_state() {
        let empty = ChannelsPartial { channels: vec![] }.render().unwrap();
        assert!(empty.contains("No notification channels yet"));
        assert!(!empty.contains("<!doctype html>"));

        let html = ChannelsPartial {
            channels: vec![ChannelRow {
                id: "abc".into(),
                name: "Ops Slack".into(),
                kind: "slack",
                enabled: true,
                created: "2026-05-18T12:00:00Z".parse().unwrap(),
                managed_by: None,
            }],
        }
        .render()
        .unwrap();
        assert!(html.contains("Ops Slack"));
        assert!(html.contains(r#"href="/settings/notifications/abc/edit""#));
        assert!(html.contains(r#"hx-post="/api/v1/notification-channels/abc/test""#));
        assert!(html.contains(r#"hx-delete="/api/v1/notification-channels/abc""#));
    }
}
