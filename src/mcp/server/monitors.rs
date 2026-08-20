//! Monitor write bodies: run a check now, pause/resume, create, retune.
//!
//! No audit here — the wrapper in [`super::tools_write`] records the outcome.

use rmcp::RoleServer;
use rmcp::handler::server::wrapper::Json;
use rmcp::service::RequestContext;

use crate::auth::scope::Scope;
use crate::domain::notification_channel::NotificationChannel;
use crate::domain::target::{NewTarget, TargetUpdate};
use crate::domain::{CheckSpec, TargetAlerts, WriteSource};
use crate::quotas::ratelimit::RateLimitCategory;
use crate::web::views::describe_check;

use crate::mcp::auth::McpAuth;
use crate::mcp::confirm::require_confirmation;
use crate::mcp::error::{McpToolError, codes, config_error, probe_dispatch_error};
use crate::mcp::schema::{
    CheckRunResult, CreateMonitorArgs, MonitorCreated, MonitorIdArg, MonitorStateResult,
    MonitorUpdateResult, ProbeOutcome, UpdateMonitorArgs,
};

use super::McpServer;
use super::args::{
    build_monitor_patch, default_interval_secs, fits_i32, new_check_spec, parse_region_policy,
    parse_uuid, resolve_bindings,
};
use super::text::{
    create_prompt_lines, field_label, present_error, sanitize_data, sanitize_prompt,
};
use super::view::{channel_names, check_diagnostic, check_timing};

impl McpServer {
    /// `run_check_now` body (no audit — the wrapper's `finish` records it).
    pub(super) async fn run_check_now_inner(
        &self,
        ctx: &RequestContext<RoleServer>,
        auth: &McpAuth,
        args: &MonitorIdArg,
    ) -> Result<Json<CheckRunResult>, McpToolError> {
        auth.require(Scope::TargetsExecute)?;
        self.enforce_rate_limit(auth.org, RateLimitCategory::CheckNow)
            .await?;
        let id = parse_uuid(&args.id, "monitor id")?;
        let target = self.load_target(auth.org, id).await?;
        require_confirmation(
            ctx,
            format!(
                "Run a check now on monitor \"{}\"? It probes the target immediately and \
                 records the result; a failure may trigger your alerts.",
                sanitize_prompt(&target.name)
            ),
        )
        .await?;

        // Same region-aware agent dispatch as REST check-now; the agent runs
        // the probe and persists the result.
        let result =
            crate::api::handlers::targets::check_now_via_dispatch(&self.state, auth.org, &target)
                .await
                .map_err(probe_dispatch_error)?;

        Ok(Json(CheckRunResult {
            id: target.id.to_string(),
            state: result.status.as_str().to_string(),
            checked_at: result.timestamp.to_rfc3339(),
            duration_ms: result.duration_ms,
            http_status: result.response_code,
            timing: check_timing(&result),
            response_size: result.response_size,
            error: result.error.as_deref().map(present_error),
            diagnostic: check_diagnostic(&result),
        }))
    }

    /// Shared pause/resume body. The wrapper's `finish` records the tool call;
    /// the store writes the org's own `target.paused`/`target.resumed` row, so
    /// the trail reads the same whichever surface stopped the monitor.
    pub(super) async fn set_enabled_inner(
        &self,
        ctx: &RequestContext<RoleServer>,
        auth: &McpAuth,
        args: &MonitorIdArg,
        enabled: bool,
    ) -> Result<Json<MonitorStateResult>, McpToolError> {
        auth.require(Scope::TargetsWrite)?;
        self.enforce_rate_limit(auth.org, RateLimitCategory::ApiWrites)
            .await?;
        let id = parse_uuid(&args.id, "monitor id")?;
        let target = self.load_writable_target(auth.org, id).await?;

        let (verb, effect) = if enabled {
            ("Resume", "Its checks will restart.")
        } else {
            ("Pause", "Its checks will stop until you resume it.")
        };
        require_confirmation(
            ctx,
            format!(
                "{verb} monitor \"{}\"? {effect}",
                sanitize_prompt(&target.name)
            ),
        )
        .await?;

        let updated = self
            .state
            .target_store
            .update(
                auth.org,
                id,
                TargetUpdate {
                    enabled: Some(enabled),
                    ..Default::default()
                },
                None,
                Some(auth.user_id),
            )
            .await
            .map_err(|e| McpToolError::internal(format!("set enabled: {e}")))?
            .ok_or_else(|| McpToolError::not_found("monitor not found"))?;

        Ok(Json(MonitorStateResult {
            id: id.to_string(),
            enabled: updated.enabled,
        }))
    }

    /// `create_monitor` body (no audit — the wrapper's `finish` records it).
    pub(super) async fn create_monitor_inner(
        &self,
        ctx: &RequestContext<RoleServer>,
        auth: &McpAuth,
        args: &CreateMonitorArgs,
    ) -> Result<Json<MonitorCreated>, McpToolError> {
        use crate::api::handlers::targets as rest;

        auth.require(Scope::TargetsWrite)?;
        // The trial run is a real probe against a caller-supplied address, so
        // this needs the scope that dispatching a probe needs, and it is metered
        // against the same probe budget the REST dry run spends.
        auth.require(Scope::TargetsExecute)?;
        self.enforce_rate_limit(auth.org, RateLimitCategory::ApiWrites)
            .await?;
        self.enforce_rate_limit(auth.org, RateLimitCategory::TestNow)
            .await?;

        let name = args.name.trim();
        if name.is_empty() {
            return Err(McpToolError::invalid_argument("name must not be blank"));
        }
        let check = new_check_spec(&args.check)?;
        let plan = self
            .state
            .quotas
            .limit_for_org(auth.org)
            .await
            .map_err(|e| McpToolError::internal(format!("plan: {e}")))?;

        let plan_floor = u64::try_from(plan.min_check_interval_secs).unwrap_or(60);
        // No interval can satisfy both bounds, so say which two disagree rather
        // than refuse an interval the caller never chose.
        if let Some(hb) = check.as_heartbeat() {
            let window = hb.period.as_secs().saturating_add(hb.grace.as_secs());
            if window < plan_floor {
                return Err(McpToolError::invalid_argument(format!(
                    "this plan checks no more often than every {plan_floor}s, so it cannot judge a \
                     heartbeat whose period and grace add up to {window}s; raise period_secs or \
                     grace_secs"
                )));
            }
        }
        let interval_secs = args
            .interval_secs
            .unwrap_or_else(|| default_interval_secs(&check, plan_floor));
        fits_i32(interval_secs, "interval_secs")?;
        if let Some(n) = args.alert_confirmations {
            fits_i32(u64::from(n), "alert_confirmations")?;
        }
        if let Some(n) = args.renotify_interval_secs {
            fits_i32(u64::from(n), "renotify_interval_secs")?;
        }

        // An empty list binds nothing, exactly like omitting the field, so it
        // does not demand the scope or spend the query.
        let wants_channels = args
            .channel_ids
            .as_deref()
            .is_some_and(|ids| !ids.is_empty());
        let channels = self.channels_for_binding(auth, wants_channels).await?;
        let alerts = match args.channel_ids.as_deref() {
            Some(ids) if wants_channels => resolve_bindings(ids, &channels)?,
            _ => TargetAlerts::default(),
        };
        let channel_summary = channel_names(
            &alerts,
            args.tags.as_deref().unwrap_or_default(),
            &channels,
            self.state.cfg.escalation.channel_failure_limit,
        );

        let mut new = NewTarget {
            name: name.to_string(),
            check,
            interval: std::time::Duration::from_secs(interval_secs),
            enabled: true,
            tags: args.tags.clone().unwrap_or_default(),
            alerts,
            region_policy: args
                .region_policy
                .as_ref()
                .map(parse_region_policy)
                .transpose()?,
            alert_confirmations: args.alert_confirmations.unwrap_or(2),
            notify_recovery: args.notify_recovery.unwrap_or(true),
            renotify_interval_secs: args.renotify_interval_secs.unwrap_or(3600),
            group_name: args.group_name.as_deref().map(str::trim).and_then(|g| {
                if g.is_empty() {
                    None
                } else {
                    Some(g.to_string())
                }
            }),
            owner_user_id: None,
        };
        rest::vet_new_target(&self.state, auth.org, &mut new, &plan)
            .await
            .map_err(config_error)?;

        // Ahead of the probe, not after it: a client that can never confirm must
        // not be able to spend probes at addresses it chooses. Behind argument
        // validation, which has no outward effect and is worth answering.
        if !crate::mcp::confirm::client_can_confirm(ctx) {
            return Err(McpToolError::new(
                codes::ELICITATION_UNSUPPORTED,
                "this MCP client cannot prompt for confirmation, and no monitor is \
                 created without one; create it in the Uptimepage app",
                false,
            ));
        }

        let address = describe_check(&new.check).1;
        let probe = if new.check.is_passive() {
            None
        } else {
            Some(self.trial_run(auth.org, &new.check).await?)
        };

        require_confirmation(
            ctx,
            format!(
                "Create monitor \"{}\"?\n\n{}\n{}",
                sanitize_prompt(&new.name),
                sanitize_prompt(&address),
                create_prompt_lines(&new, probe.as_ref(), channel_summary.as_deref()).join("\n"),
            ),
        )
        .await?;

        let created = rest::create_target(&self.state, auth.org, new, WriteSource::Api, &plan)
            .await
            .map_err(config_error)?;

        Ok(Json(MonitorCreated {
            id: created.id.to_string(),
            name: sanitize_data(&created.name),
            address: sanitize_data(&address),
            interval_secs: created.interval.as_secs(),
            probe,
            alerts: channel_summary.unwrap_or_else(|| "nobody".to_string()),
        }))
    }

    /// The org's channel inventory, read once so every diff and prompt names
    /// the same rows. Empty when the caller is not touching bindings.
    async fn channels_for_binding(
        &self,
        auth: &McpAuth,
        binding: bool,
    ) -> Result<Vec<NotificationChannel>, McpToolError> {
        if !binding {
            return Ok(Vec::new());
        }
        // Naming and validating a channel is reading the inventory, so the
        // caller needs the scope that reading it needs. Checked before any
        // budget is spent on a call that cannot succeed without it.
        auth.require(Scope::ChannelsRead)?;
        self.state
            .notification_channel_store
            .list(auth.org)
            .await
            .map_err(|e| McpToolError::internal(format!("list channels: {e}")))
    }

    /// Run the check once, unsaved, so the confirmation can show what it does.
    async fn trial_run(
        &self,
        org: crate::domain::OrgId,
        check: &CheckSpec,
    ) -> Result<ProbeOutcome, McpToolError> {
        let region = self
            .state
            .cfg
            .scheduler
            .effective_default_region()
            .to_string();
        let delivered = crate::api::handlers::targets::run_ad_hoc(
            &self.state,
            org,
            &region,
            crate::domain::agent_wire::DispatchKind::Test,
            None,
            check.clone(),
        )
        .await
        .map_err(probe_dispatch_error)?;
        let r = delivered.result;
        Ok(ProbeOutcome {
            state: r.status.as_str().to_string(),
            duration_ms: r.duration_ms,
            http_status: r.response_code,
            error: r.error.as_deref().map(present_error),
            diagnostic: check_diagnostic(&r),
        })
    }

    /// `update_monitor` body (no audit — the wrapper's `finish` records it).
    pub(super) async fn update_monitor_inner(
        &self,
        ctx: &RequestContext<RoleServer>,
        auth: &McpAuth,
        args: &UpdateMonitorArgs,
    ) -> Result<Json<MonitorUpdateResult>, McpToolError> {
        use crate::api::handlers::targets as rest;

        auth.require(Scope::TargetsWrite)?;
        self.enforce_rate_limit(auth.org, RateLimitCategory::ApiWrites)
            .await?;
        let id = parse_uuid(&args.id, "monitor id")?;
        let target = self.load_writable_target(auth.org, id).await?;

        // Read once and handed to every diff: the patch is rebuilt after the
        // confirmation and the two must agree field for field, so channels
        // cannot be diffed outside it.
        let channels = self
            .channels_for_binding(auth, args.channel_ids.is_some())
            .await?;

        let (update, changes) = build_monitor_patch(
            args,
            &target,
            &channels,
            self.state.cfg.escalation.channel_failure_limit,
        )?;
        if changes.is_empty() {
            return Ok(Json(MonitorUpdateResult {
                id: id.to_string(),
                changes,
            }));
        }

        rest::validate_alert_confirmations(update.alert_confirmations).map_err(config_error)?;
        rest::validate_renotify_interval(update.renotify_interval_secs).map_err(config_error)?;
        if let Some(Some(group)) = update.group_name.as_ref() {
            rest::validate_group_name(Some(group.as_str())).map_err(config_error)?;
        }
        if update.region_policy.is_some() {
            let available = self
                .state
                .target_store
                .available_regions()
                .await
                .map_err(|e| McpToolError::internal(format!("region catalog: {e}")))?;
            rest::validate_region_policy(update.region_policy, available.len())
                .map_err(config_error)?;
        }
        rest::validate_patch_interval(&self.state, auth.org, id, &update, Some(&target))
            .await
            .map_err(config_error)?;

        require_confirmation(
            ctx,
            format!(
                "Change monitor \"{}\"?\n\n{}",
                sanitize_prompt(&target.name),
                changes
                    .iter()
                    .map(|c| format!(
                        "{}: {} → {}",
                        field_label(&c.field),
                        sanitize_prompt(&c.from),
                        sanitize_prompt(&c.to)
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .await?;

        // The monitor can move while a human reads the prompt, and the approval
        // describes the diff as it stood then.
        let current = self.load_writable_target(auth.org, id).await?;
        let (update, still) = build_monitor_patch(
            args,
            &current,
            &channels,
            self.state.cfg.escalation.channel_failure_limit,
        )?;
        if still != changes {
            return Err(McpToolError::new(
                codes::CONFLICT,
                "monitor changed while the change was being confirmed; read it again and retry",
                true,
            ));
        }

        // `None`: not restamping `write_source` is what keeps a terraform marker.
        self.state
            .target_store
            .update(auth.org, id, update, None, Some(auth.user_id))
            .await
            .map_err(|e| McpToolError::internal(format!("update monitor: {e}")))?
            .ok_or_else(|| McpToolError::not_found("monitor not found"))?;

        Ok(Json(MonitorUpdateResult {
            id: id.to_string(),
            changes,
        }))
    }
}
