//! The write tools: scope-gated, elicitation-confirmed and audited.
//!
//! Each is a thin wrapper: build the audit args, run the inner body, then
//! [`McpServer::finish`] writes exactly one audit row for the outcome — so
//! EVERY path (insufficient scope, declined, bad input, not-found, error,
//! success) is recorded, not just the happy path. The bodies themselves live
//! in [`super::monitors`] and [`super::incidents`].

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};
use serde_json::json;

use crate::mcp::auth::McpAuth;
use crate::mcp::error::McpToolError;
use crate::mcp::schema::{
    CheckRunResult, CreateMonitorArgs, IncidentActionArgs, IncidentActionResult, IncidentIdArg,
    IncidentUpdatePosted, IncidentVisibilityResult, MonitorCreated, MonitorIdArg,
    MonitorStateResult, MonitorUpdateResult, PostIncidentUpdateArgs, PublishIncidentArgs,
    UpdateMonitorArgs,
};

use super::McpServer;
use super::args::requested_fields;

#[tool_router(router = write_router, vis = "pub(super)")]
impl McpServer {
    #[tool(
        description = "Run a check on a monitor immediately and record the result. Requires user confirmation; a down result may fire the org's normal alerts. Heartbeat monitors cannot be probed (they wait for your systems to ping them). Not read-only.",
        title = "Run check now",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn run_check_now(
        &self,
        Parameters(args): Parameters<MonitorIdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<CheckRunResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.run_check_now_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "run_check_now", args_json, result)
            .await
    }

    #[tool(
        description = "Pause a monitor (stop its checks until resumed). Requires user confirmation. Not read-only; idempotent.",
        title = "Pause monitor",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn pause_monitor(
        &self,
        Parameters(args): Parameters<MonitorIdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<MonitorStateResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.set_enabled_inner(&ctx, &auth, &args, false).await;
        self.finish(pool, &auth, "pause_monitor", args_json, result)
            .await
    }

    /// Create a monitor. The check runs once first and its result is shown in
    /// the confirmation, so a misconfigured check is visible before it exists.
    #[tool(
        description = "Create a monitor for an http, tcp, ping, dns, tls_cert, domain_expiry or heartbeat check. The check is run once before anything is saved and the result is shown to the user along with every setting it would apply; nothing is created unless they approve. Pass channel_ids from list_notification_channels to have it alert those channels (this needs the channels:read scope); without them the monitor alerts nobody. Request headers, request bodies and credentials cannot be set here, a URL carrying a username or password is refused, and browser flows cannot be created here — add those in the app. Not read-only.",
        title = "Create monitor",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn create_monitor(
        &self,
        Parameters(args): Parameters<CreateMonitorArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<MonitorCreated>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let result = self.create_monitor_inner(&ctx, &auth, &args).await;
        let args_json = match &result {
            Ok(Json(created)) => json!({
                "id": created.id,
                "name": created.name,
                "address": created.address,
                "interval_secs": created.interval_secs,
                // Who this pages is the part an incident review asks about.
                "alerts": created.alerts,
            }),
            Err(_) => json!({ "name": args.name }),
        };
        self.finish(pool, &auth, "create_monitor", args_json, result)
            .await
    }

    /// Retune an existing monitor's alerting and cadence. Read it first with
    /// `get_monitor`: tags and channel bindings are replaced whole, not merged.
    #[tool(
        description = "Change how loudly a monitor is watched: check interval, alert confirmations, recovery notices, reminder interval, tags, group, the multi-region detection quorum, and which notification channels it alerts (channel_ids replaces the whole set, and needs the channels:read scope). It cannot change what the check watches — name, address, assertions, expected status, headers, body, probe regions and owner are refused. A monitor managed by Terraform is refused outright. Shows the old and new value of every field before it runs, and requires confirmation. Not read-only.",
        title = "Retune monitor",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn update_monitor(
        &self,
        Parameters(args): Parameters<UpdateMonitorArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<MonitorUpdateResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let result = self.update_monitor_inner(&ctx, &auth, &args).await;
        // This path leaves `write_source` alone, so the audit row is the only
        // record of what an MCP client changed.
        let args_json = match &result {
            Ok(Json(applied)) => json!({ "id": args.id, "changes": applied.changes }),
            Err(_) => json!({ "id": args.id, "requested": requested_fields(&args) }),
        };
        self.finish(pool, &auth, "update_monitor", args_json, result)
            .await
    }

    #[tool(
        description = "Resume a paused monitor (restart its checks). Requires user confirmation. Not read-only; idempotent.",
        title = "Resume monitor",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn resume_monitor(
        &self,
        Parameters(args): Parameters<MonitorIdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<MonitorStateResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.set_enabled_inner(&ctx, &auth, &args, true).await;
        self.finish(pool, &auth, "resume_monitor", args_json, result)
            .await
    }

    #[tool(
        description = "Acknowledge an incident: take ownership and halt escalation. Internal/operational only — does NOT post anything to the public status page. Use post_incident_update for customer-facing updates. Requires confirmation. Not read-only.",
        title = "Acknowledge incident",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn acknowledge_incident(
        &self,
        Parameters(args): Parameters<IncidentActionArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<IncidentActionResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.acknowledge_incident_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "acknowledge_incident", args_json, result)
            .await
    }

    #[tool(
        description = "Resolve an incident (mark the operational state resolved). Internal only — does not post to the public status page. Requires confirmation. Not read-only.",
        title = "Resolve incident",
        // Closing a live incident ends the escalation that would page someone.
        annotations(read_only_hint = false, destructive_hint = true, idempotent_hint = true)
    )]
    async fn resolve_incident(
        &self,
        Parameters(args): Parameters<IncidentActionArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<IncidentActionResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.resolve_incident_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "resolve_incident", args_json, result)
            .await
    }

    #[tool(
        description = "Post a public, customer-facing update to an incident's status-page timeline (phase + message). This is what your subscribers and status-page visitors see. Requires confirmation. Not read-only.",
        title = "Post incident update",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn post_incident_update(
        &self,
        Parameters(args): Parameters<PostIncidentUpdateArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<IncidentUpdatePosted>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id, "phase": args.phase });
        let result = self.post_incident_update_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "post_incident_update", args_json, result)
            .await
    }

    #[tool(
        description = "Publish an incident so it appears on every status page carrying the affected monitor, optionally seeding the public title and description. Status-page subscribers may be notified. Requires confirmation. Not read-only; idempotent.",
        title = "Publish incident",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn publish_incident(
        &self,
        Parameters(args): Parameters<PublishIncidentArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<IncidentVisibilityResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.publish_incident_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "publish_incident", args_json, result)
            .await
    }

    #[tool(
        description = "Hide a published incident from the public status pages again. Its operator timeline is untouched. Requires confirmation. Not read-only; idempotent.",
        title = "Unpublish incident",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn unpublish_incident(
        &self,
        Parameters(args): Parameters<IncidentIdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<IncidentVisibilityResult>, McpToolError> {
        let auth = McpAuth::from_ctx(&ctx)?;
        let pool = self.require_pool()?;
        let args_json = json!({ "id": args.id });
        let result = self.unpublish_incident_inner(&ctx, &auth, &args).await;
        self.finish(pool, &auth, "unpublish_incident", args_json, result)
            .await
    }
}
