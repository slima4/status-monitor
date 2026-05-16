use std::net::SocketAddr;

use anyhow::Context;
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::error::Result;

pub struct MetricsHandle;

pub fn init(bind: &str) -> Result<MetricsHandle> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("parsing metrics_bind '{bind}'"))?;

    PrometheusBuilder::new()
        .with_http_listener(addr)
        .install()
        .context("install prometheus exporter")?;

    register_descriptions();
    metrics::counter!("status_monitor_build_info", "version" => env!("CARGO_PKG_VERSION"))
        .absolute(1);
    tracing::info!(
        // SAFE: operator metrics bind address, not a peer/user IP
        addr = %addr,
        "metrics listening"
    );
    Ok(MetricsHandle)
}

fn register_descriptions() {
    use metrics::{describe_counter, describe_gauge, describe_histogram};

    describe_counter!(
        "status_monitor_checks_total",
        "Total checks completed, labelled by status"
    );
    describe_counter!(
        "status_monitor_checks_errors_total",
        "Total check errors, labelled by kind"
    );
    describe_counter!(
        "status_monitor_circuit_breaker_state_changes_total",
        "Circuit breaker state transitions"
    );
    describe_counter!(
        "status_monitor_storage_writes_total",
        "Storage writes, labelled by store and result"
    );
    describe_counter!(
        "status_monitor_storage_dropped_results_total",
        "Results dropped before storage, labelled by reason"
    );

    describe_histogram!(
        "status_monitor_check_duration_ms",
        "Total check duration in milliseconds"
    );
    describe_histogram!(
        "status_monitor_check_dns_ms",
        "DNS resolution latency in milliseconds"
    );
    describe_histogram!(
        "status_monitor_check_connect_ms",
        "TCP connect latency in milliseconds (recorded only when a new connection is established)"
    );
    describe_histogram!(
        "status_monitor_check_tls_ms",
        "TLS handshake latency in milliseconds (recorded only when a new HTTPS connection is established)"
    );
    describe_histogram!(
        "status_monitor_check_ttfb_ms",
        "HTTP time-to-first-byte in milliseconds"
    );
    describe_gauge!(
        "status_monitor_http_pool_idle_connections",
        "Connections held in the HTTP connection pool but not currently serving a request"
    );
    describe_gauge!(
        "status_monitor_http_pool_active_connections",
        "Connections currently serving an in-flight HTTP request"
    );
    describe_histogram!(
        "status_monitor_storage_batch_size",
        "Result batch size at flush time"
    );
    describe_histogram!(
        "status_monitor_storage_write_duration_ms",
        "Storage write duration in milliseconds"
    );

    describe_gauge!(
        "status_monitor_targets_total",
        "Total targets known to the registry"
    );
    describe_gauge!(
        "status_monitor_workers_in_flight",
        "Checks currently executing"
    );
    describe_gauge!(
        "status_monitor_result_queue_depth",
        "Current depth of the result channel buffer"
    );
    describe_gauge!(
        "status_monitor_circuit_breakers_open",
        "Number of circuit breakers currently in the Open state"
    );
    describe_counter!(
        "status_monitor_notifications_total",
        "Alert notifications dispatched, labelled by channel and kind"
    );
    describe_counter!(
        "status_monitor_notifications_failures_total",
        "Alert notification dispatches that returned an error, labelled by channel"
    );
    describe_counter!(
        "status_monitor_alerts_dropped_total",
        "Alert signals dropped before reaching the engine, labelled by reason"
    );
}

pub mod names {
    pub const CHECKS_TOTAL: &str = "status_monitor_checks_total";
    pub const CHECK_ERRORS: &str = "status_monitor_checks_errors_total";
    pub const BREAKER_STATE_CHANGES: &str = "status_monitor_circuit_breaker_state_changes_total";
    pub const STORAGE_WRITES: &str = "status_monitor_storage_writes_total";
    pub const STORAGE_DROPPED: &str = "status_monitor_storage_dropped_results_total";
    pub const CHECK_DURATION_MS: &str = "status_monitor_check_duration_ms";
    pub const CHECK_DNS_MS: &str = "status_monitor_check_dns_ms";
    pub const CHECK_CONNECT_MS: &str = "status_monitor_check_connect_ms";
    pub const CHECK_TLS_MS: &str = "status_monitor_check_tls_ms";
    pub const CHECK_TTFB_MS: &str = "status_monitor_check_ttfb_ms";
    pub const HTTP_POOL_IDLE: &str = "status_monitor_http_pool_idle_connections";
    pub const HTTP_POOL_ACTIVE: &str = "status_monitor_http_pool_active_connections";
    pub const STORAGE_BATCH_SIZE: &str = "status_monitor_storage_batch_size";
    pub const STORAGE_WRITE_DURATION_MS: &str = "status_monitor_storage_write_duration_ms";
    pub const TARGETS_TOTAL: &str = "status_monitor_targets_total";
    pub const WORKERS_IN_FLIGHT: &str = "status_monitor_workers_in_flight";
    pub const RESULT_QUEUE_DEPTH: &str = "status_monitor_result_queue_depth";
    pub const BREAKERS_OPEN: &str = "status_monitor_circuit_breakers_open";
    pub const NOTIFICATIONS_TOTAL: &str = "status_monitor_notifications_total";
    pub const NOTIFICATIONS_FAILURES: &str = "status_monitor_notifications_failures_total";
    pub const ALERTS_DROPPED: &str = "status_monitor_alerts_dropped_total";
}
