use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::domain::{
    CheckSpec, ExpectedStatus, HttpMethod, OrgId, RegionIncidentPolicy, Target, TargetAlerts,
};
use crate::error::AppError;
use crate::web::error::WebResult;
use crate::web::filters;
use crate::web::{AuthedBrowser, CurrentOrg};

/// One HTTP header in the form's key/value row repeater.
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

pub struct AuthFieldState {
    pub has_basic: bool,
    pub has_bearer: bool,
}

impl AuthFieldState {
    pub fn basic_initial_mode(&self) -> &'static str {
        if self.has_basic { "redacted" } else { "create" }
    }

    pub fn bearer_initial_mode(&self) -> &'static str {
        if self.has_bearer {
            "redacted"
        } else {
            "create"
        }
    }
}

pub struct HttpFields {
    pub url: String,
    pub method: &'static str,
    pub timeout_ms: u64,
    pub follow_redirects: bool,
    pub max_redirects: u8,
    /// Single-input replacement for the old radio + 3 fields. Accepts
    /// "200", "200-299", "200, 201, 204". JS parses on submit.
    pub expected_status_input: String,
    pub expected_body_contains: String,
    pub headers: Vec<HeaderPair>,
    pub body: String,
    pub verify_tls: bool,
    pub auth: AuthFieldState,
}

impl Default for HttpFields {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: "GET",
            timeout_ms: 5_000,
            // Follow by default: most real targets (apex domains, http→https)
            // 301 to a canonical host, and a fresh monitor pointed at them
            // should report Up, not Down on the redirect.
            follow_redirects: true,
            max_redirects: 5,
            expected_status_input: "200".into(),
            expected_body_contains: String::new(),
            headers: Vec::new(),
            body: String::new(),
            verify_tls: true,
            auth: AuthFieldState {
                has_basic: false,
                has_bearer: false,
            },
        }
    }
}

pub struct TcpFields {
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

impl Default for TcpFields {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 0,
            timeout_ms: 3_000,
        }
    }
}

pub struct DnsFields {
    pub domain: String,
    pub record_type: &'static str,
    pub resolver: String,
    pub expected_contains: String,
    pub timeout_ms: u64,
}

impl Default for DnsFields {
    fn default() -> Self {
        Self {
            domain: String::new(),
            record_type: "A",
            resolver: String::new(),
            expected_contains: String::new(),
            timeout_ms: 3_000,
        }
    }
}

pub struct TlsCertFields {
    pub host: String,
    pub port: u16,
    pub server_name: String,
    pub warn_days: u32,
    pub critical_days: u32,
    pub timeout_ms: u64,
}

impl Default for TlsCertFields {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 443,
            server_name: String::new(),
            warn_days: 30,
            critical_days: 7,
            timeout_ms: 5_000,
        }
    }
}

pub struct DomainExpiryFields {
    pub domain: String,
    pub warn_days: u32,
    pub critical_days: u32,
    pub timeout_ms: u64,
}

impl Default for DomainExpiryFields {
    fn default() -> Self {
        Self {
            domain: String::new(),
            warn_days: 30,
            critical_days: 7,
            timeout_ms: 5_000,
        }
    }
}

/// One row in the monitor form's Alerts section: an org channel plus whether
/// this monitor binds to it. Channels are pure delivery targets — the firing
/// policy (confirmations, recovery) is monitor-level.
pub struct ChannelChoice {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub selected: bool,
}

pub struct FormModel {
    pub mode: &'static str,
    pub id: String,
    pub action: String,
    pub submit_method: &'static str,
    pub name: String,
    pub interval_s: u64,
    /// The org plan's `min_check_interval_secs`, surfaced so the form's
    /// `min=`/JS guard mirror the same floor the API enforces (no magic 60).
    pub min_interval_s: u64,
    pub enabled: bool,
    pub tags: Vec<String>,
    /// Free-text operator group label (drives Monitors-page bucketing).
    pub group_name: String,
    /// Selected owner user-id (or empty string for "unowned"); the form
    /// renders a `<select>` populated from `owner_options`.
    pub owner_user_id: String,
    /// Org members available as owner candidates.
    pub owner_options: Vec<OwnerChoice>,
    pub check_type: &'static str,
    pub http: HttpFields,
    pub tcp: TcpFields,
    pub dns: DnsFields,
    pub tls_cert: TlsCertFields,
    pub domain_expiry: DomainExpiryFields,
    /// The org's notification channels, with this monitor's bindings prefilled.
    pub channels: Vec<ChannelChoice>,
    /// Consecutive failing checks before this monitor alerts (monitor-level).
    pub alert_confirmations: u32,
    /// Whether a recovery is announced to the bound channels (monitor-level).
    pub notify_recovery: bool,
    /// Seconds between outage reminders while unacknowledged; 0 = off.
    pub renotify_interval_secs: u32,
    /// Escalation-policy choices for the monitor's binding selector (edit only;
    /// a not-yet-created monitor has no id to bind). Own binding marked selected.
    pub escalation_choices: Vec<crate::web::views::escalation::Choice>,
    /// What an unbound monitor escalates through — shown while inheriting.
    pub escalation_hint: String,
    /// Whether the escalation-policy section renders at all (off when the
    /// team-paging feature is disabled for the deployment).
    pub show_escalation: bool,
    /// The enabled region catalog with this monitor's assignments prefilled
    /// (edit only). Empty / single-region deployments still render one row.
    pub region_choices: Vec<RegionChoice>,
    /// Detection-threshold dropdown options with the monitor's current one
    /// selected: `any` / `majority` / `all` plus a fixed count up to the catalog.
    pub region_threshold_options: Vec<ThresholdChoice>,
    /// Whether the region assignment + policy section renders (edit only — a
    /// not-yet-created monitor has no id to assign regions to).
    pub show_regions: bool,
}

/// One option in the detection-threshold dropdown. `value` is what the form
/// submits (`any`/`majority`/`all` or a plain integer).
pub struct ThresholdChoice {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// Build the threshold dropdown for `selected`, offering the symbolic presets
/// plus a fixed count for each region up to `max`.
fn region_threshold_choices(selected: RegionIncidentPolicy, max: usize) -> Vec<ThresholdChoice> {
    let current = match selected {
        RegionIncidentPolicy::Any => "any".to_string(),
        RegionIncidentPolicy::Majority => "majority".to_string(),
        RegionIncidentPolicy::All => "all".to_string(),
        RegionIncidentPolicy::Count(n) => n.to_string(),
    };
    let mut out = vec![
        ("any", "Any region down".to_string()),
        ("majority", "Majority of regions".to_string()),
        ("all", "All regions".to_string()),
    ]
    .into_iter()
    .map(|(v, label)| ThresholdChoice {
        selected: current == v,
        value: v.to_string(),
        label,
    })
    .collect::<Vec<_>>();
    for n in 1..=max {
        let v = n.to_string();
        out.push(ThresholdChoice {
            selected: current == v,
            value: v.clone(),
            label: format!("{n} region{}", if n == 1 { "" } else { "s" }),
        });
    }
    out
}

/// One preset in the outage-reminder dropdown. `secs` is submitted verbatim.
pub struct RenotifyChoice {
    pub secs: u32,
    pub label: String,
    pub selected: bool,
}

pub struct ConfirmationChoice {
    pub value: u32,
    pub label: String,
    pub selected: bool,
}

/// One preset in the check-interval rail. `secs` is submitted verbatim.
pub struct IntervalChoice {
    pub secs: u64,
    pub label: String,
    pub selected: bool,
}

impl FormModel {
    /// Confirmation-count presets; an off-preset stored value is preserved as its own option.
    pub fn confirmation_options(&self) -> Vec<ConfirmationChoice> {
        let mut values = vec![1, 2, 3, 5];
        if !values.contains(&self.alert_confirmations) {
            values.push(self.alert_confirmations);
            values.sort_unstable();
        }
        values
            .into_iter()
            .map(|value| ConfirmationChoice {
                value,
                label: if value == 1 {
                    "1 fail".to_string()
                } else {
                    format!("{value} fails")
                },
                selected: value == self.alert_confirmations,
            })
            .collect()
    }

    /// Whether the selected kind sits on the slow cadence group (the API
    /// enforces an hourly floor for certificate / registration expiry).
    pub fn slow_kind(&self) -> bool {
        crate::domain::min_interval_secs_for_kind(self.check_type) >= 3_600
    }

    /// Per-kind interval floors as a JSON object, mirrored to the client so
    /// the form validates against the same numbers the API enforces.
    pub fn kind_floors_json(&self) -> String {
        let floors: serde_json::Map<String, serde_json::Value> =
            ["http", "tcp", "dns", "tls_cert", "domain_expiry"]
                .iter()
                .map(|k| {
                    (
                        (*k).to_string(),
                        crate::domain::min_interval_secs_for_kind(k).into(),
                    )
                })
                .collect();
        serde_json::Value::Object(floors).to_string()
    }

    /// Check-interval presets for http/tcp/dns, filtered by the plan floor.
    pub fn interval_options_fast(&self) -> Vec<IntervalChoice> {
        self.interval_group(
            &[30, 60, 120, 300, 600, 900, 1_800, 3_600],
            !self.slow_kind(),
        )
    }

    /// Check-interval presets for tls_cert/domain_expiry.
    pub fn interval_options_slow(&self) -> Vec<IntervalChoice> {
        self.interval_group(&[3_600, 21_600, 43_200, 86_400], self.slow_kind())
    }

    /// An off-preset stored value is preserved as its own option in the
    /// active group, so editing an API-created monitor keeps its cadence.
    fn interval_group(&self, presets: &[u64], active: bool) -> Vec<IntervalChoice> {
        let mut values: Vec<u64> = presets
            .iter()
            .copied()
            .filter(|s| *s >= self.min_interval_s)
            .collect();
        if active && !values.contains(&self.interval_s) {
            values.push(self.interval_s);
            values.sort_unstable();
        }
        values
            .into_iter()
            .map(|secs| IntervalChoice {
                secs,
                label: super::exact_duration(secs),
                selected: active && secs == self.interval_s,
            })
            .collect()
    }

    /// Reminder-cadence presets with the monitor's current interval selected;
    /// an off-preset stored value is preserved as its own option.
    pub fn renotify_options(&self) -> Vec<RenotifyChoice> {
        let mut values: Vec<u32> = vec![0, 900, 1_800, 3_600, 7_200, 21_600];
        if !values.contains(&self.renotify_interval_secs) {
            values.push(self.renotify_interval_secs);
            values.sort_unstable();
        }
        values
            .into_iter()
            .map(|secs| RenotifyChoice {
                secs,
                label: if secs == 0 {
                    "off".to_string()
                } else {
                    super::exact_duration(u64::from(secs))
                },
                selected: secs == self.renotify_interval_secs,
            })
            .collect()
    }
}

/// One option in the form's "Owner" select.
pub struct OwnerChoice {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

/// One region in the monitor form's assignment checkboxes.
pub struct RegionChoice {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Template, WebTemplate)]
#[template(path = "targets/form.html")]
pub struct FormPage {
    pub active_tab: &'static str,
    pub form: FormModel,
}

#[derive(Debug, Default, Deserialize)]
pub struct NewParams {
    /// When set, prefill the create form from an existing monitor (the
    /// "Copy" action on the list) so similar monitors can be added fast.
    #[serde(default)]
    pub from: Option<Uuid>,
}

/// Whether `form_from_target` produces an edit form (PATCH the same monitor)
/// or a copy (POST a new monitor seeded from an existing one).
enum FormKind {
    Edit,
    Copy,
}

fn empty_create_form() -> FormModel {
    FormModel {
        mode: "create",
        id: String::new(),
        action: "/api/v1/targets".into(),
        submit_method: "POST",
        name: String::new(),
        interval_s: 60,
        min_interval_s: 60,
        enabled: true,
        tags: Vec::new(),
        group_name: String::new(),
        owner_user_id: String::new(),
        owner_options: Vec::new(),
        check_type: "http",
        http: HttpFields::default(),
        tcp: TcpFields::default(),
        dns: DnsFields::default(),
        tls_cert: TlsCertFields::default(),
        domain_expiry: DomainExpiryFields::default(),
        channels: Vec::new(),
        alert_confirmations: 2,
        notify_recovery: true,
        renotify_interval_secs: 3600,
        escalation_choices: Vec::new(),
        escalation_hint: String::new(),
        show_escalation: false,
        region_choices: Vec::new(),
        region_threshold_options: Vec::new(),
        show_regions: false,
    }
}

/// The org's channels with `alerts` prefilled as the selected bindings.
/// Unbound channels default to a sensible new-binding policy.
async fn channel_choices(
    state: &AppState,
    org: OrgId,
    alerts: &TargetAlerts,
) -> Result<Vec<ChannelChoice>, AppError> {
    let bound: std::collections::HashSet<Uuid> = alerts.iter().map(|b| b.channel_id).collect();
    let channels = state.notification_channel_store.list(org).await?;
    Ok(channels
        .into_iter()
        .map(|c| ChannelChoice {
            selected: bound.contains(&c.id),
            id: c.id.to_string(),
            name: c.name,
            kind: super::channel_kind_label(c.kind),
        })
        .collect())
}

/// The org plan's check-interval floor, as the form needs it (u64 seconds).
/// Same value the API enforces via `min_check_interval`, so the client
/// `min=`/guard never disagree with the server.
async fn plan_min_interval(state: &AppState, org: OrgId) -> Result<u64, AppError> {
    let plan = state.quotas.limit_for_org(org).await?;
    Ok(u64::try_from(plan.min_check_interval_secs)
        .unwrap_or(60)
        .max(1))
}

/// Org members rendered as `<select>` options for the owner field.
/// Empty option ("unowned") is added template-side.
async fn owner_choices(
    state: &AppState,
    org: OrgId,
    selected: &str,
) -> Result<Vec<OwnerChoice>, AppError> {
    let members = match state.db.as_ref() {
        Some(pool) => crate::storage::orgs::list_members(pool, org).await?,
        None => return Ok(Vec::new()),
    };
    let mut out: Vec<OwnerChoice> = members
        .into_iter()
        .map(|m| {
            let id = m.membership.user_id.0.to_string();
            OwnerChoice {
                selected: id == selected,
                id,
                label: m.email,
            }
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(out)
}

pub async fn new_form(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Query(params): Query<NewParams>,
) -> WebResult<FormPage> {
    let (mut form, alerts) = match params.from {
        Some(id) => {
            let target = state
                .target_store
                .get(org, id)
                .await?
                .ok_or_else(|| AppError::not_found("TARGET_NOT_FOUND", "monitor not found"))?;
            let alerts = target.alerts.clone();
            (form_from_target(target, FormKind::Copy)?, alerts)
        }
        None => (empty_create_form(), TargetAlerts::default()),
    };
    form.channels = channel_choices(&state, org, &alerts).await?;
    form.owner_options = owner_choices(&state, org, &form.owner_user_id).await?;
    form.show_escalation = state.cfg.escalation.enabled;
    form.min_interval_s = plan_min_interval(&state, org).await?;
    // A new monitor is prefilled with 60s; raise it if the plan floor is
    // higher so the default the user sees would actually be accepted.
    form.interval_s = form.interval_s.max(form.min_interval_s);
    // Prefill the default coverage (all regions capped at the plan, checked).
    let available = state.target_store.available_regions_detailed().await?;
    let max_regions = state.quotas.limit_for_org(org).await?.max_regions;
    if available.len() > 1 && max_regions > 1 {
        let default_region = state.cfg.scheduler.effective_default_region().to_string();
        let ids: Vec<String> = available.iter().map(|r| r.id.clone()).collect();
        let default_set =
            crate::api::handlers::targets::default_region_set(ids, max_regions, &default_region);
        let chosen: std::collections::HashSet<String> = default_set.into_iter().collect();
        let cap = available.len().min(max_regions.max(1) as usize);
        form.region_choices = available
            .into_iter()
            .map(|r| RegionChoice {
                selected: chosen.contains(&r.id),
                label: crate::web::views::region_display::region_label(&r),
                id: r.id,
            })
            .collect();
        form.region_threshold_options =
            region_threshold_choices(RegionIncidentPolicy::default(), cap);
        form.show_regions = true;
    }
    Ok(FormPage {
        active_tab: "targets",
        form,
    })
}

pub async fn edit_form(
    _auth: AuthedBrowser,
    CurrentOrg(org): CurrentOrg,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> WebResult<FormPage> {
    let target = state
        .target_store
        .get(org, id)
        .await?
        .ok_or_else(|| AppError::not_found("TARGET_NOT_FOUND", "monitor not found"))?;
    let alerts = target.alerts.clone();
    let region_policy = target.region_policy;
    let mut form = form_from_target(target, FormKind::Edit)?;
    form.channels = channel_choices(&state, org, &alerts).await?;
    form.owner_options = owner_choices(&state, org, &form.owner_user_id).await?;
    form.show_escalation = state.cfg.escalation.enabled;
    if form.show_escalation {
        (form.escalation_choices, form.escalation_hint) =
            crate::web::views::escalation::monitor_binding(&state, org, id).await?;
    }
    // Meaningless unless the deployment has >1 region and the plan allows >1.
    let available = state.target_store.available_regions_detailed().await?;
    let max_regions = state.quotas.limit_for_org(org).await?.max_regions;
    if available.len() > 1 && max_regions > 1 {
        let assigned: std::collections::HashSet<String> = state
            .target_store
            .regions_for_target(org, id)
            .await?
            .unwrap_or_default()
            .into_iter()
            .collect();
        let cap = available.len().min(max_regions.max(1) as usize);
        form.region_choices = available
            .into_iter()
            .map(|r| RegionChoice {
                selected: assigned.contains(&r.id),
                label: crate::web::views::region_display::region_label(&r),
                id: r.id,
            })
            .collect();
        form.region_threshold_options = region_threshold_choices(region_policy, cap);
        form.show_regions = true;
    }
    // Edit keeps the saved interval as-is; if a plan floor rose past it the
    // save will surface the API error rather than silently rewriting it.
    form.min_interval_s = plan_min_interval(&state, org).await?;
    Ok(FormPage {
        active_tab: "targets",
        form,
    })
}

fn form_from_target(t: Target, kind: FormKind) -> Result<FormModel, AppError> {
    let tags = t.tags;
    let group_name = t.group_name.unwrap_or_default();
    let owner_user_id = t.owner_user_id.map(|id| id.to_string()).unwrap_or_default();

    let mut http = HttpFields::default();
    let mut tcp = TcpFields::default();
    let mut dns = DnsFields::default();
    let mut tls_cert = TlsCertFields::default();
    let mut domain_expiry = DomainExpiryFields::default();
    let check_type: &'static str = match t.check {
        CheckSpec::Http(h) => {
            http = http_fields_from(h);
            "http"
        }
        CheckSpec::Tcp(c) => {
            tcp = TcpFields {
                host: c.host,
                port: c.port,
                timeout_ms: c.timeout.as_millis() as u64,
            };
            "tcp"
        }
        CheckSpec::Dns(d) => {
            dns = DnsFields {
                domain: d.domain,
                record_type: d.record_type.as_str(),
                resolver: d.resolver.unwrap_or_default(),
                expected_contains: d.expected_contains.unwrap_or_default(),
                timeout_ms: d.timeout.as_millis() as u64,
            };
            "dns"
        }
        CheckSpec::TlsCert(c) => {
            tls_cert = TlsCertFields {
                host: c.host,
                port: c.port,
                server_name: c.server_name.unwrap_or_default(),
                warn_days: c.warn_days,
                critical_days: c.critical_days,
                timeout_ms: c.timeout.as_millis() as u64,
            };
            "tls_cert"
        }
        CheckSpec::DomainExpiry(d) => {
            domain_expiry = DomainExpiryFields {
                domain: d.domain,
                warn_days: d.warn_days,
                critical_days: d.critical_days,
                timeout_ms: d.timeout.as_millis() as u64,
            };
            "domain_expiry"
        }
    };

    let (mode, id, action, submit_method, name) = match kind {
        FormKind::Edit => (
            "edit",
            t.id.to_string(),
            format!("/api/v1/targets/{}", t.id),
            "PATCH",
            t.name,
        ),
        FormKind::Copy => (
            "create",
            String::new(),
            "/api/v1/targets".into(),
            "POST",
            format!("{} (copy)", t.name),
        ),
    };

    let alert_confirmations = t.alert_confirmations;
    let notify_recovery = t.notify_recovery;
    let renotify_interval_secs = t.renotify_interval_secs;

    Ok(FormModel {
        mode,
        id,
        action,
        submit_method,
        name,
        interval_s: t.interval.as_secs(),
        // Overwritten by the handler with the org plan's real floor.
        min_interval_s: 60,
        enabled: t.enabled,
        tags,
        group_name,
        owner_user_id,
        // Populated by the handler from `orgs::list_members`.
        owner_options: Vec::new(),
        check_type,
        http,
        tcp,
        dns,
        tls_cert,
        domain_expiry,
        channels: Vec::new(),
        alert_confirmations,
        notify_recovery,
        renotify_interval_secs,
        escalation_choices: Vec::new(),
        escalation_hint: String::new(),
        show_escalation: false,
        region_choices: Vec::new(),
        region_threshold_options: Vec::new(),
        show_regions: false,
    })
}

fn http_fields_from(h: crate::domain::HttpCheck) -> HttpFields {
    let has_basic = h.basic_auth.is_some();
    let has_bearer = h.bearer_token.is_some();
    let expected_status_input = match h.expected_status {
        ExpectedStatus::Exact(c) => c.to_string(),
        ExpectedStatus::Range { min, max } => format!("{min}-{max}"),
        ExpectedStatus::OneOf(v) => v.iter().map(u16::to_string).collect::<Vec<_>>().join(", "),
    };
    let mut headers: Vec<HeaderPair> = h
        .headers
        .into_iter()
        .map(|(name, value)| HeaderPair { name, value })
        .collect();
    headers.sort_by(|a, b| a.name.cmp(&b.name));
    HttpFields {
        url: h.url.to_string(),
        method: http_method_str(h.method),
        timeout_ms: h.timeout.as_millis() as u64,
        follow_redirects: h.follow_redirects,
        max_redirects: h.max_redirects,
        expected_status_input,
        expected_body_contains: h.expected_body_contains.unwrap_or_default(),
        headers,
        body: h.body.unwrap_or_default(),
        verify_tls: h.verify_tls,
        auth: AuthFieldState {
            has_basic,
            has_bearer,
        },
    }
}

fn http_method_str(m: HttpMethod) -> &'static str {
    match m {
        HttpMethod::Get => "GET",
        HttpMethod::Head => "HEAD",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Options => "OPTIONS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::redaction::REDACTED;

    #[test]
    fn new_form_renders_empty_create() {
        let page = FormPage {
            active_tab: "targets",
            form: empty_create_form(),
        };
        let html = page.render().unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("New monitor"));
        assert!(html.contains(r#"data-action="/api/v1/targets""#));
        assert!(html.contains(r#"data-method="POST""#));
        assert!(html.contains(r#"data-mode="create""#));
        assert!(html.contains(r#"name="check_type" value="http""#));
    }

    #[test]
    fn renders_all_five_check_type_cards() {
        let html = FormPage {
            active_tab: "targets",
            form: empty_create_form(),
        }
        .render()
        .unwrap();
        assert_eq!(html.matches("data-check-card").count(), 5);
        for v in ["http", "tcp", "dns", "tls_cert", "domain_expiry"] {
            assert!(
                html.contains(&format!(r#"value="{v}""#)),
                "missing card for variant {v}"
            );
        }
        // Default selection lands on HTTP.
        assert!(html.contains(r#"name="check_type" value="http" checked"#));
        // Each protocol panel is rendered (non-active panels are .hidden).
        for v in ["http", "tcp", "dns", "tls_cert", "domain_expiry"] {
            assert!(
                html.contains(&format!(r#"data-variant="{v}""#)),
                "missing panel for variant {v}"
            );
        }
    }

    #[test]
    fn smart_expected_status_round_trips_all_three_shapes() {
        for (status, want) in [
            (ExpectedStatus::Exact(204), "204"),
            (ExpectedStatus::Range { min: 200, max: 299 }, "200-299"),
            (ExpectedStatus::OneOf(vec![200, 201, 204]), "200, 201, 204"),
        ] {
            use crate::domain::HttpCheck;
            use std::collections::HashMap;
            use std::time::Duration;
            use url::Url;
            let h = HttpCheck {
                url: Url::parse("https://example.com").unwrap(),
                method: HttpMethod::Get,
                timeout: Duration::from_millis(5_000),
                follow_redirects: true,
                max_redirects: 5,
                expected_status: status,
                expected_body_contains: None,
                headers: HashMap::new(),
                body: None,
                verify_tls: true,
                basic_auth: None,
                bearer_token: None,
            };
            assert_eq!(http_fields_from(h).expected_status_input, want);
        }
    }

    #[test]
    fn headers_render_as_row_inputs() {
        use crate::domain::HttpCheck;
        use std::collections::HashMap;
        use std::time::Duration;
        use url::Url;
        let mut headers = HashMap::new();
        headers.insert("Accept".to_string(), "application/json".to_string());
        headers.insert("X-Request-Id".to_string(), "abc".to_string());
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "api".into(),
            check: CheckSpec::Http(HttpCheck {
                url: Url::parse("https://example.com").unwrap(),
                method: HttpMethod::Get,
                timeout: Duration::from_millis(5_000),
                follow_redirects: true,
                max_redirects: 5,
                expected_status: ExpectedStatus::Exact(200),
                expected_body_contains: None,
                headers,
                body: None,
                verify_tls: true,
                basic_auth: None,
                bearer_token: None,
            }),
            interval: Duration::from_secs(60),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert_eq!(form.http.headers.len(), 2);
        // Sorted alphabetically by name for stable rendering.
        assert_eq!(form.http.headers[0].name, "Accept");
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        // Container has `data-header-rows` (plural); each row has `data-header-row`
        // (no value, followed by class). Count the row attr with trailing space
        // to avoid matching the container.
        assert_eq!(html.matches("data-header-row ").count(), 2);
        assert!(html.contains(r#"name="http_header_key""#));
        assert!(html.contains(r#"name="http_header_value""#));
        assert!(html.contains(r#"value="Accept""#));
        assert!(html.contains(r#"value="application/json""#));
    }

    #[test]
    fn tags_render_as_chips() {
        let mut form = empty_create_form();
        form.tags = vec!["prod".into(), "api".into()];
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        assert_eq!(html.matches(r#"class="tag-chip""#).count(), 2);
        assert!(html.contains(r#"data-tag-value="prod""#));
        assert!(html.contains(r#"data-tag-value="api""#));
        assert!(html.contains(r#"data-tag-input"#));
    }

    #[test]
    fn form_surfaces_plan_interval_floor_and_presets() {
        let mut form = empty_create_form();
        form.min_interval_s = 60;
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        // Client mirror of the API floor (no hardcoded 60 in the markup).
        assert!(html.contains(r#"data-min-interval="60""#));
        // The plan floor drops the 30s preset from the fast rail.
        assert!(!html.contains(r#"name="interval_s" value="30""#));
        assert!(html.contains(r#"name="interval_s" value="60" class="sr-only" checked"#));
        // Both cadence rails render; the slow one starts hidden on http.
        assert!(html.contains(r#"data-interval-rail="fast""#));
        assert!(html.contains(r#"data-interval-rail="slow" hidden"#));
        assert!(html.contains(r#"name="interval_s" value="86400""#));
    }

    #[test]
    fn off_preset_interval_is_preserved_as_an_option() {
        let mut form = empty_create_form();
        form.interval_s = 90;
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"name="interval_s" value="90" class="sr-only" checked"#));
        assert!(html.contains("90s"));
    }

    #[test]
    fn edit_form_renders_redacted_state_for_existing_auth() {
        use crate::domain::HttpCheck;
        use std::collections::HashMap;
        use std::time::Duration;
        use url::Url;

        let t = Target {
            id: uuid::Uuid::nil(),
            name: "api".into(),
            check: CheckSpec::Http(HttpCheck {
                url: Url::parse("https://example.com").unwrap(),
                method: HttpMethod::Get,
                timeout: Duration::from_millis(5_000),
                follow_redirects: false,
                max_redirects: 0,
                expected_status: ExpectedStatus::Exact(200),
                expected_body_contains: None,
                headers: HashMap::new(),
                body: None,
                verify_tls: true,
                basic_auth: Some((REDACTED.into(), REDACTED.into())),
                bearer_token: Some(REDACTED.into()),
            }),
            interval: Duration::from_secs(60),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert!(form.http.auth.has_basic);
        assert!(form.http.auth.has_bearer);
        assert_eq!(form.submit_method, "PATCH");
        let page = FormPage {
            active_tab: "targets",
            form,
        };
        let html = page.render().unwrap();
        assert!(html.contains(r#"data-initial-mode="redacted""#));
        assert!(html.contains("replace credentials"));
        assert!(html.contains("replace token"));
    }

    #[test]
    fn edit_form_maps_tcp_target_fields() {
        use crate::domain::TcpCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "db".into(),
            check: CheckSpec::Tcp(TcpCheck {
                host: "db.example.com".into(),
                port: 5432,
                timeout: Duration::from_millis(2_500),
            }),
            interval: Duration::from_secs(30),
            enabled: false,
            tags: vec!["prod".into(), "db".into()],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert_eq!(form.check_type, "tcp");
        assert_eq!(form.tcp.host, "db.example.com");
        assert_eq!(form.tcp.port, 5432);
        assert_eq!(form.tcp.timeout_ms, 2_500);
        assert_eq!(form.interval_s, 30);
        assert!(!form.enabled);
        assert_eq!(form.tags, vec!["prod".to_string(), "db".to_string()]);
        assert_eq!(form.submit_method, "PATCH");
    }

    #[test]
    fn edit_form_maps_tls_cert_target_fields() {
        use crate::domain::TlsCertCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "tls".into(),
            check: CheckSpec::TlsCert(TlsCertCheck {
                host: "example.com".into(),
                port: 8443,
                server_name: Some("vhost.example.com".into()),
                warn_days: 14,
                critical_days: 3,
                timeout: Duration::from_millis(4_500),
            }),
            interval: Duration::from_secs(3_600),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert_eq!(form.check_type, "tls_cert");
        assert_eq!(form.tls_cert.host, "example.com");
        assert_eq!(form.tls_cert.port, 8443);
        assert_eq!(form.tls_cert.server_name, "vhost.example.com");
        assert_eq!(form.tls_cert.warn_days, 14);
        assert_eq!(form.tls_cert.critical_days, 3);
        assert_eq!(form.tls_cert.timeout_ms, 4_500);
    }

    #[test]
    fn edit_form_maps_domain_expiry_target_fields() {
        use crate::domain::DomainExpiryCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "dom".into(),
            check: CheckSpec::DomainExpiry(DomainExpiryCheck {
                domain: "example.com".into(),
                warn_days: 60,
                critical_days: 14,
                timeout: Duration::from_secs(10),
            }),
            interval: Duration::from_secs(86_400),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert_eq!(form.check_type, "domain_expiry");
        assert_eq!(form.domain_expiry.domain, "example.com");
        assert_eq!(form.domain_expiry.warn_days, 60);
        assert_eq!(form.domain_expiry.critical_days, 14);
        assert_eq!(form.domain_expiry.timeout_ms, 10_000);
    }

    #[test]
    fn edit_form_maps_dns_target_fields() {
        use crate::domain::{DnsCheck, DnsRecordType};
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "dns".into(),
            check: CheckSpec::Dns(DnsCheck {
                domain: "api.example.com".into(),
                record_type: DnsRecordType::Cname,
                resolver: Some("1.1.1.1".into()),
                expected_contains: Some("edge.cdn".into()),
                timeout: Duration::from_millis(2_500),
            }),
            interval: Duration::from_secs(60),
            enabled: true,
            tags: vec![],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Edit).unwrap();
        assert_eq!(form.check_type, "dns");
        assert_eq!(form.dns.domain, "api.example.com");
        assert_eq!(form.dns.record_type, "CNAME");
        assert_eq!(form.dns.resolver, "1.1.1.1");
        assert_eq!(form.dns.expected_contains, "edge.cdn");
        assert_eq!(form.dns.timeout_ms, 2_500);
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"name="check_type" value="dns""#));
        assert!(html.contains(r#"value="CNAME" selected"#));
        assert!(html.contains(r#"value="api.example.com""#));
        assert!(html.contains(r#"value="edge.cdn""#));
        assert!(html.contains(r#"value="1.1.1.1""#));
    }

    #[test]
    fn escalation_selector_is_edit_only() {
        // Create has no monitor id to bind yet → a prompt, no selector.
        let mut create_form = empty_create_form();
        create_form.show_escalation = true;
        let create = FormPage {
            active_tab: "targets",
            form: create_form,
        }
        .render()
        .unwrap();
        assert!(create.contains("Save the monitor first"));
        assert!(!create.contains("data-monitor-policy-select"));

        // Edit renders the binding selector with the inherit option.
        let mut form = empty_create_form();
        form.mode = "edit";
        form.show_escalation = true;
        form.id = "00000000-0000-0000-0000-000000000001".into();
        form.escalation_choices = vec![crate::web::views::escalation::Choice {
            id: "p1".into(),
            name: "Primary".into(),
            selected: true,
        }];
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        assert!(html.contains("data-monitor-policy-select"));
        assert!(html.contains(r#"data-target-id="00000000-0000-0000-0000-000000000001""#));
        assert!(html.contains("inherit org default"));
        assert!(html.contains(r#"value="p1" selected"#));
    }

    #[test]
    fn escalation_section_hidden_when_disabled() {
        // show_escalation defaults off → the whole policy block is absent.
        let form = empty_create_form();
        assert!(!form.show_escalation);
        let html = FormPage {
            active_tab: "targets",
            form,
        }
        .render()
        .unwrap();
        assert!(!html.contains("Escalation policy"));
        assert!(!html.contains("Save the monitor first"));
    }

    #[test]
    fn copy_form_seeds_create_from_existing() {
        use crate::domain::TcpCheck;
        use std::time::Duration;
        let t = Target {
            id: uuid::Uuid::nil(),
            name: "db".into(),
            check: CheckSpec::Tcp(TcpCheck {
                host: "db.example.com".into(),
                port: 5432,
                timeout: Duration::from_millis(2_500),
            }),
            interval: Duration::from_secs(30),
            enabled: true,
            tags: vec!["prod".into()],
            alerts: Default::default(),
            region_policy: Default::default(),
            alert_confirmations: 2,
            notify_recovery: true,
            renotify_interval_secs: 3600,
            group_name: None,
            owner_user_id: None,
            write_source: crate::domain::WriteSource::Ui,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let form = form_from_target(t, FormKind::Copy).unwrap();
        assert_eq!(form.mode, "create");
        assert_eq!(form.submit_method, "POST");
        assert_eq!(form.action, "/api/v1/targets");
        assert!(form.id.is_empty());
        assert_eq!(form.name, "db (copy)");
        // Check config carried over so the copy is a real duplicate.
        assert_eq!(form.check_type, "tcp");
        assert_eq!(form.tcp.host, "db.example.com");
        assert_eq!(form.tcp.port, 5432);
        assert_eq!(form.tags, vec!["prod".to_string()]);
    }
}
