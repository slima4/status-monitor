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
use crate::domain::{ChannelConfig, NotificationChannel, Target};
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
    pub telegram_app_chat_id: String,
    pub telegram_app_chat_title: String,
    pub whatsapp_access_token: String,
    pub whatsapp_phone_number_id: String,
    pub whatsapp_to: String,
    pub whatsapp_template_name: String,
    pub whatsapp_language_code: String,
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
            telegram_app_chat_id: String::new(),
            telegram_app_chat_title: String::new(),
            whatsapp_access_token: String::new(),
            whatsapp_phone_number_id: String::new(),
            whatsapp_to: String::new(),
            whatsapp_template_name: String::new(),
            whatsapp_language_code: String::new(),
        }
    }
}

/// A monitor card on the channel edit page — either already bound to the
/// channel (used-by grid) or offered by the "+ add monitor" picker.
pub struct MonitorCard {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub addr: String,
    pub enabled: bool,
    /// `terraform`/`api` chip — a UI bind may be overwritten on the next
    /// apply, so the card says who manages the monitor.
    pub managed_by: Option<&'static str>,
    /// Space-joined tags, feeding the picker's client-side search.
    pub tags: String,
}

pub struct ChannelFormModel {
    /// `"create"` or `"edit"`. Drives the heading, submit verb, and (when not
    /// `"create"`) the "Replace transport config" toggle.
    pub mode: &'static str,
    /// Channel id; empty on create. The bind picker PATCHes it into a
    /// monitor's alert bindings.
    pub channel_id: String,
    pub action: String,
    pub submit_method: &'static str,
    pub name: String,
    pub enabled: bool,
    pub kind: &'static str,
    pub config: ConfigFields,
    /// Monitors bound to this channel; always empty on create.
    pub used_by: Vec<MonitorCard>,
    /// Monitors NOT bound to this channel, offered by the bind picker.
    pub bindable: Vec<MonitorCard>,
    /// Whether this deployment runs the central Telegram bot. Gates the
    /// one-tap "telegram" type card; the BYO "telegram bot" card is always
    /// offered.
    pub central_telegram: bool,
}

impl ChannelFormModel {
    /// The one-tap card shows on create when the bot is configured, and on
    /// edit only for an already-linked channel (informational — a linked
    /// channel stays viewable even if the operator later removes the bot).
    pub fn offers_telegram_app(&self) -> bool {
        (self.central_telegram && self.mode == "create") || self.kind == "telegram_app"
    }
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

    /// The org has no monitors at all — the picker can only offer creating
    /// one.
    pub fn has_no_monitors(&self) -> bool {
        self.used_by.is_empty() && self.bindable.is_empty()
    }

    /// Every monitor is already bound to this channel.
    pub fn all_bound(&self) -> bool {
        self.bindable.is_empty() && !self.used_by.is_empty()
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
            kind: super::channel_kind_label(c.kind),
            enabled: c.enabled,
            created: c.created_at,
            managed_by: c.write_source.managed_label(),
        })
        .collect();
    Ok(ChannelsPartial { channels }.into_response())
}

pub async fn new_form(
    State(state): State<AppState>,
    org: Result<CurrentOrg, AppError>,
) -> WebResult<Response> {
    let org = match resolve_org(org, "/settings/notifications/new") {
        Ok(o) => o,
        Err(resp) => return Ok(*resp),
    };
    let mut form = empty_create_form();
    form.central_telegram = state.cfg.telegram.enabled();
    (_, form.bindable) = org_monitor_cards(&state, org, None).await?;
    Ok(ChannelFormPage {
        active_tab: TAB_NOTIFICATIONS,
        form,
    }
    .into_response())
}

/// All org monitors as cards, split by alert binding to `channel`:
/// `(used_by, bindable)`. With no channel everything is bindable.
async fn org_monitor_cards(
    state: &AppState,
    org: crate::domain::OrgId,
    channel: Option<Uuid>,
) -> Result<(Vec<MonitorCard>, Vec<MonitorCard>), AppError> {
    // The default filter caps at 100, which would silently hide monitors
    // past that on large orgs — fetch the full set.
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
    let card = |t: Target| {
        let (kind, addr) = describe_check(&t.check);
        MonitorCard {
            id: t.id.to_string(),
            name: t.name,
            kind,
            addr,
            enabled: t.enabled,
            managed_by: t.write_source.managed_label(),
            tags: t.tags.join(" "),
        }
    };
    let (used_by, bindable): (Vec<_>, Vec<_>) = targets
        .into_iter()
        .partition(|t| channel.is_some_and(|id| t.alerts.iter().any(|b| b.channel_id == id)));
    let cards = |v: Vec<Target>| {
        let mut cards: Vec<MonitorCard> = v.into_iter().map(&card).collect();
        // Alphabetical keeps the picker's search + pagination predictable.
        cards.sort_by_cached_key(|c| c.name.to_lowercase());
        cards
    };
    Ok((cards(used_by), cards(bindable)))
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
    let mut form = form_from_channel(channel);
    form.central_telegram = state.cfg.telegram.enabled();
    (form.used_by, form.bindable) = org_monitor_cards(&state, org, Some(id)).await?;
    Ok(ChannelFormPage {
        active_tab: TAB_NOTIFICATIONS,
        form,
    }
    .into_response())
}

fn empty_create_form() -> ChannelFormModel {
    ChannelFormModel {
        mode: "create",
        channel_id: String::new(),
        action: "/api/v1/notification-channels".into(),
        submit_method: "POST",
        name: String::new(),
        enabled: true,
        kind: "slack",
        config: ConfigFields::default(),
        used_by: Vec::new(),
        bindable: Vec::new(),
        central_telegram: false,
    }
}

fn form_from_channel(c: NotificationChannel) -> ChannelFormModel {
    // Prefill from the *redacted* config so non-secret routing (header names,
    // chat id) survives the edit while secrets stay masked.
    let mut redacted = c.config.clone();
    redacted.redact_in_place();
    let mut config = ConfigFields::default();
    match redacted {
        ChannelConfig::Slack(c) => config.slack_webhook_url = c.webhook_url,
        ChannelConfig::Webhook(c) => {
            config.webhook_url = c.url;
            config.webhook_headers_json = json_pretty(&c.headers);
            config.webhook_secret = c.secret.unwrap_or_default();
        }
        ChannelConfig::Telegram(c) => {
            config.telegram_bot_token = c.bot_token;
            config.telegram_chat_id = c.chat_id;
        }
        // Linked via the central bot; display-only — the API rejects this
        // kind in request bodies, so the panel renders info, not inputs.
        ChannelConfig::TelegramApp(c) => {
            config.telegram_app_chat_id = c.chat_id;
            config.telegram_app_chat_title = c.chat_title.unwrap_or_default();
        }
        ChannelConfig::WhatsApp(c) => {
            config.whatsapp_access_token = c.access_token;
            config.whatsapp_phone_number_id = c.phone_number_id;
            config.whatsapp_to = c.to;
            config.whatsapp_template_name = c.template_name;
            config.whatsapp_language_code = c.language_code.unwrap_or_default();
        }
    }
    ChannelFormModel {
        mode: "edit",
        channel_id: c.id.to_string(),
        action: format!("/api/v1/notification-channels/{}", c.id),
        submit_method: "PATCH",
        name: c.name,
        enabled: c.enabled,
        kind: c.kind.as_db_str(),
        config,
        used_by: Vec::new(),
        bindable: Vec::new(),
        central_telegram: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SlackConfig;

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
        // Every transport offers a type card + config panel.
        for kind in ["slack", "webhook", "telegram", "whatsapp"] {
            assert!(html.contains(&format!(r#"value="{kind}""#)), "{kind} card");
            assert!(
                html.contains(&format!(r#"data-variant="{kind}""#)),
                "{kind} panel"
            );
        }
        // Telegram setup helper: bot QR + chat-id probe.
        assert!(html.contains("data-tg-qr"));
        assert!(html.contains("data-tg-detect"));
        // Fingerprinted URL proves the vendored lib is actually embedded —
        // a missing file silently falls back to the bare path and 404s.
        assert!(
            crate::web::assets::url("js/qrcode.min.js").contains("?v="),
            "qrcode.min.js must be embedded"
        );
        // Create has no "replace config" toggle — config is always sent.
        assert!(!html.contains("Replace transport config"));
        // "Test now" works pre-save (ad-hoc config test); delete needs a
        // saved channel and stays edit-only.
        assert!(html.contains("data-send-test"));
        assert!(html.contains("test now"));
        assert!(!html.contains("delete channel"));
        // Create offers the same + card picker; picks are bound after create.
        assert!(html.contains("data-add-monitor"));
        assert!(html.contains("# none picked yet"));
        assert!(html.contains("# no monitors yet"));
    }

    #[test]
    fn create_form_one_tap_telegram_gated_on_central_bot() {
        // Without the central bot (self-host) the one-tap card is absent and
        // only the BYO "telegram bot" card is offered.
        let off = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form: empty_create_form(),
        }
        .render()
        .unwrap();
        assert!(!off.contains(r#"value="telegram_app""#));
        assert!(off.contains("telegram bot"));

        let mut form = empty_create_form();
        form.central_telegram = true;
        let on = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(on.contains(r#"value="telegram_app""#));
        assert!(on.contains("one-tap chat link"));
        assert!(on.contains("data-tga-connect"));
        assert!(on.contains("data-tga-qr-box"));
    }

    #[test]
    fn edit_form_linked_telegram_shows_chat_info_not_inputs() {
        use chrono::Utc;
        let ch = NotificationChannel {
            id: Uuid::nil(),
            name: "Ops Telegram".into(),
            kind: crate::domain::ChannelKind::TelegramApp,
            config: ChannelConfig::TelegramApp(crate::domain::TelegramAppConfig {
                chat_id: "-100123".into(),
                chat_title: Some("Ops".into()),
            }),
            enabled: true,
            write_source: crate::domain::WriteSource::Ui,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let form = form_from_channel(ch);
        assert_eq!(form.kind, "telegram_app");
        assert_eq!(form.config.telegram_app_chat_id, "-100123");
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        // The one-tap card stays visible for an already-linked channel even
        // with central_telegram=false, so the edit page renders coherently.
        assert!(html.contains(r#"value="telegram_app""#));
        assert!(html.contains("# linked via the bot"));
        assert!(html.contains("-100123"));
        assert!(html.contains("Ops"));
        // Display-only: no connect button on edit.
        assert!(!html.contains("data-tga-connect"));
    }

    #[test]
    fn create_form_offers_monitor_binding() {
        let mut form = empty_create_form();
        form.bindable = vec![MonitorCard {
            id: "t1".into(),
            name: "api-prod".into(),
            kind: "HTTP",
            addr: "https://api.example.com/health".into(),
            enabled: true,
            managed_by: None,
            tags: String::new(),
        }];
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains("data-add-monitor"));
        assert!(html.contains(r#"data-bind-monitor data-target-id="t1""#));
        assert!(html.contains("api-prod"));
        // Search + show-disabled + pager controls ride along for big orgs.
        assert!(html.contains("data-picker-search"));
        assert!(html.contains("data-picker-show-disabled"));
        assert!(html.contains("data-picker-pager"));
    }

    fn slack_channel(webhook_url: &str) -> NotificationChannel {
        use chrono::Utc;
        NotificationChannel {
            id: Uuid::nil(),
            name: "Ops".into(),
            kind: crate::domain::ChannelKind::Slack,
            config: ChannelConfig::Slack(SlackConfig {
                webhook_url: webhook_url.into(),
            }),
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
            MonitorCard {
                id: "t1".into(),
                name: "api-prod".into(),
                kind: "HTTP",
                addr: "https://api.example.com/health".into(),
                enabled: true,
                managed_by: None,
                tags: String::new(),
            },
            MonitorCard {
                id: "t2".into(),
                name: "old-worker".into(),
                kind: "TCP",
                addr: "10.0.0.5:9000".into(),
                enabled: false,
                managed_by: None,
                tags: String::new(),
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
        // Each bound card carries its × unbind affordance.
        assert!(html.contains(r#"data-unbind-monitor data-target-id="t1""#));
        assert!(html.contains(r#"aria-label="unbind api-prod""#));
        // The empty-state note renders hidden so the JS can re-show it after
        // the last unbind.
        assert!(html.contains(r#"data-used-by-note class="font-mono text-xs text-quiet hidden""#));
    }

    #[test]
    fn edit_form_renders_bind_picker() {
        let mut form = form_from_channel(slack_channel("https://hooks.slack.com/services/T/B/x"));
        form.bindable = vec![
            MonitorCard {
                id: "t3".into(),
                name: "staging-api".into(),
                kind: "HTTP",
                addr: "https://staging.example.com".into(),
                enabled: true,
                managed_by: None,
                tags: String::new(),
            },
            MonitorCard {
                id: "t4".into(),
                name: "tf-api".into(),
                kind: "HTTP",
                addr: "https://tf.example.com".into(),
                enabled: true,
                managed_by: Some("terraform"),
                tags: String::new(),
            },
        ];
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains("data-add-monitor"));
        assert!(html.contains(r#"data-channel-id="00000000-0000-0000-0000-000000000000""#));
        assert!(html.contains(r#"data-bind-monitor data-target-id="t3""#));
        assert!(html.contains("staging-api"));
        // Externally-managed monitors carry their chip — a UI bind may be
        // overwritten on the next apply.
        assert!(html.contains(r#"<span class="cli-brackets font-normal" title="managed externally — changes made here may be overwritten on the next apply">terraform</span>"#));
        // Monitors are available, so the all-bound note starts hidden.
        assert!(
            html.contains(r#"data-picker-allbound class="font-mono text-xs text-quiet hidden""#)
        );
    }

    #[test]
    fn edit_form_bind_picker_empty_states() {
        // Every state renders (the JS toggles them after in-place moves);
        // the server decides which starts visible.
        const NONE_VISIBLE: &str = r#"data-picker-none class="font-mono text-xs text-quiet""#;
        const NONE_HIDDEN: &str = r#"data-picker-none class="font-mono text-xs text-quiet hidden""#;
        const ALLBOUND_VISIBLE: &str =
            r#"data-picker-allbound class="font-mono text-xs text-quiet""#;
        const ALLBOUND_HIDDEN: &str =
            r#"data-picker-allbound class="font-mono text-xs text-quiet hidden""#;

        // No monitors at all → invite to create one.
        let form = form_from_channel(slack_channel("https://hooks.slack.com/services/T/B/x"));
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains("data-add-monitor"));
        assert!(html.contains(NONE_VISIBLE));
        assert!(html.contains(ALLBOUND_HIDDEN));
        assert!(html.contains(r#"href="/targets/new""#));

        // Every monitor already bound → say so instead of an empty grid.
        let mut form = form_from_channel(slack_channel("https://hooks.slack.com/services/T/B/x"));
        form.used_by = vec![MonitorCard {
            id: "t1".into(),
            name: "api-prod".into(),
            kind: "HTTP",
            addr: "https://api.example.com/health".into(),
            enabled: true,
            managed_by: None,
            tags: String::new(),
        }];
        let html = ChannelFormPage {
            active_tab: TAB_NOTIFICATIONS,
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains(ALLBOUND_VISIBLE));
        assert!(html.contains(NONE_HIDDEN));
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
