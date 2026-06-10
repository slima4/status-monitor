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
use crate::storage::traits::TargetFilter;
use crate::web::CurrentOrg;
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::views::{describe_check, json_pretty, resolve_org};

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
    pub webhook_secret: String,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
}

impl Default for ConfigFields {
    fn default() -> Self {
        Self {
            slack_webhook_url: String::new(),
            webhook_url: String::new(),
            webhook_headers_json: "{}".into(),
            webhook_secret: String::new(),
            telegram_bot_token: String::new(),
            telegram_chat_id: String::new(),
        }
    }
}

/// A monitor whose alert bindings include the channel being edited.
pub struct UsedByMonitor {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub addr: String,
    pub enabled: bool,
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
    /// Monitors bound to this channel; always empty on create.
    pub used_by: Vec<UsedByMonitor>,
}

impl ChannelFormModel {
    /// `"1 monitor"` / `"3 monitors"` — one pluralization for the header
    /// count and the delete warning.
    pub fn used_by_label(&self) -> String {
        let n = self.used_by.len();
        if n == 1 {
            "1 monitor".into()
        } else {
            format!("{n} monitors")
        }
    }
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
    // The default filter caps at 100, which would silently hide bound
    // monitors past that on large orgs — fetch the full set.
    let targets = state
        .target_store
        .list(
            org,
            TargetFilter {
                limit: Some(10_000),
                ..Default::default()
            },
        )
        .await?;
    let used_by = targets
        .into_iter()
        .filter(|t| t.alerts.iter().any(|b| b.channel_id == id))
        .map(|t| {
            let (kind, addr) = describe_check(&t.check);
            UsedByMonitor {
                id: t.id.to_string(),
                name: t.name,
                kind,
                addr,
                enabled: t.enabled,
            }
        })
        .collect();
    let mut form = form_from_channel(channel);
    form.used_by = used_by;
    Ok(ChannelFormPage {
        active_tab: TAB_NOTIFICATIONS,
        form,
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
        used_by: Vec::new(),
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
        ChannelConfig::Webhook {
            url,
            headers,
            secret,
        } => {
            config.webhook_url = url;
            config.webhook_headers_json = json_pretty(&headers);
            config.webhook_secret = secret.unwrap_or_default();
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
        used_by: Vec::new(),
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
        // "Test now" works pre-save (ad-hoc config test); delete needs a
        // saved channel and stays edit-only.
        assert!(html.contains("data-send-test"));
        assert!(html.contains("test now"));
        assert!(!html.contains("delete channel"));
    }

    fn slack_channel(webhook_url: &str) -> NotificationChannel {
        use chrono::Utc;
        NotificationChannel {
            id: Uuid::nil(),
            name: "Ops".into(),
            kind: crate::domain::ChannelKind::Slack,
            config: ChannelConfig::Slack {
                webhook_url: webhook_url.into(),
            },
            enabled: true,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn edit_form_prefills_redacted_config_and_replace_toggle() {
        let ch = slack_channel("https://hooks.slack.com/services/T/B/zzUNIQUESECRETzz");
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
        assert!(html.contains("data-send-test"));
        assert!(html.contains(
            r#"hx-delete="/api/v1/notification-channels/00000000-0000-0000-0000-000000000000""#
        ));
        // Empty used_by: quiet placeholders in the header and the card.
        assert!(html.contains("# not bound to any monitor"));
        assert!(html.contains("# not bound to any monitor yet"));
        // The real webhook never reaches the browser — only the `***` mask.
        assert!(!html.contains("zzUNIQUESECRETzz"));
        assert!(html.contains(r#"value="***""#));
    }

    #[test]
    fn edit_form_lists_bound_monitors() {
        let mut form = form_from_channel(slack_channel("https://hooks.slack.com/services/T/B/x"));
        form.used_by = vec![
            UsedByMonitor {
                id: "t1".into(),
                name: "api-prod".into(),
                kind: "HTTP",
                addr: "https://api.example.com/health".into(),
                enabled: true,
            },
            UsedByMonitor {
                id: "t2".into(),
                name: "old-worker".into(),
                kind: "TCP",
                addr: "10.0.0.5:9000".into(),
                enabled: false,
            },
        ];
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"href="/targets/t1/edit""#));
        assert!(html.contains("api-prod"));
        assert!(html.contains("http · https://api.example.com/health"));
        assert!(html.contains("old-worker"));
        assert!(html.contains("tcp · 10.0.0.5:9000"));
        assert!(html.contains(r#"<span class="cli-brackets font-normal">disabled</span>"#));
        assert!(!html.contains("# not bound to any monitor yet"));
    }

    #[test]
    fn channels_partial_renders_rows_and_empty_state() {
        let empty = ChannelsPartial { channels: vec![] }.render().unwrap();
        assert!(empty.contains("# no channels configured"));
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
