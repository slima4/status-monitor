use utoipa::OpenApi;

use crate::api::error::{ApiError, ApiErrorBody};
use crate::api::handlers;
use crate::api::page::PageEnvelope;
use crate::api::types::{
    BulkAction, BulkActionFailure, BulkActionRequest, BulkActionResponse, DashboardSummary,
    Last24hSummary, StatusBreakdown, SystemSummary, TagCount, TargetsSummary, TestRequest,
    TestResponse,
};
use crate::domain::{
    AlertChannel, AlertChannelConfig, CheckResult, CheckSpec, CheckStatus, DomainExpiryCheck,
    ExpectedStatus, HttpCheck, HttpMethod, Incident, NewTarget, Target, TargetAlerts,
    TargetUpdate, TcpCheck, TlsCertCheck,
};
use crate::storage::UptimeStats;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "status-monitor",
        description = "HTTP / TCP / TLS-cert / domain-expiry health-check service. \
                       Schedules checks against configured targets, stores results, \
                       exposes a REST API.",
        license(name = "MIT"),
    ),
    servers((url = "/", description = "Current host")),
    paths(
        handlers::health::healthz,
        handlers::health::readyz,
        handlers::targets::create,
        handlers::targets::bulk_create,
        handlers::targets::list,
        handlers::targets::get,
        handlers::targets::update,
        handlers::targets::delete,
        handlers::targets::bulk_action,
        handlers::targets::test_check,
        handlers::targets::check_now,
        handlers::results::list_results,
        handlers::results::uptime,
        handlers::results::list_incidents,
        handlers::tags::list_tags,
        handlers::dashboard::dashboard_summary,
    ),
    components(
        schemas(
            ApiError,
            ApiErrorBody,
            handlers::health::HealthResponse,
            Target,
            NewTarget,
            TargetUpdate,
            CheckSpec,
            HttpCheck,
            TcpCheck,
            TlsCertCheck,
            DomainExpiryCheck,
            HttpMethod,
            ExpectedStatus,
            CheckResult,
            CheckStatus,
            UptimeStats,
            Incident,
            AlertChannel,
            AlertChannelConfig,
            TargetAlerts,
            TagCount,
            DashboardSummary,
            TargetsSummary,
            StatusBreakdown,
            Last24hSummary,
            SystemSummary,
            BulkActionRequest,
            BulkAction,
            BulkActionResponse,
            BulkActionFailure,
            TestRequest,
            TestResponse,
            PageEnvelope<Target>,
            PageEnvelope<CheckResult>,
            PageEnvelope<Incident>,
            PageEnvelope<TagCount>,
        ),
    ),
    tags(
        (name = "health",    description = "Liveness and readiness probes"),
        (name = "targets",   description = "Target CRUD and operations"),
        (name = "results",   description = "Check results, uptime, incidents"),
        (name = "tags",      description = "Tag inventory"),
        (name = "dashboard", description = "Fleet-wide aggregated views"),
    ),
)]
pub struct ApiDoc;
