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
        "status_monitor_check_redirects_total",
        "HTTP redirect hops, labelled by outcome \
         (followed | limit_exceeded | invalid_location | blocked_scheme)"
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
    describe_counter!(
        "status_monitor_rdap_singleflight_total",
        "RDAP singleflight outcomes per domain: hit (cached) or miss (fetched)"
    );
    describe_counter!(
        "status_monitor_domain_expiry_stale_served_total",
        "Times the domain_expiry executor served a cached last-good answer instead of a fresh probe, labelled by failure kind"
    );
    describe_counter!(
        "status_monitor_domain_expiry_state_write_failed_total",
        "Failures writing the last-good cache row after a successful probe — sustained values mean the sticky cache is going cold even though probes succeed"
    );
    describe_gauge!(
        "status_monitor_rdap_singleflight_slots",
        "Live entries in the RDAP singleflight cache. Bounded under normal load by the set of monitored domains"
    );
    describe_counter!(
        "status_monitor_scheduler_refresh_failed_total",
        "Registry refresh ticks that returned an error from Postgres — alert on a sustained rate above your normal noise floor"
    );
    describe_gauge!(
        "status_monitor_scheduler_consecutive_refresh_failures",
        "Consecutive registry refresh failures since the last success. Resets to 0 on recovery; primary alarm signal for a stuck scheduler"
    );
    describe_histogram!(
        "status_monitor_scheduler_refresh_duration_ms",
        "Wall-clock duration of one registry refresh tick (Postgres query + decode + DashMap diff). p99 climbing past a few hundred ms means the full-scan refresh is starting to strain at scale — switch to incremental sync"
    );
    describe_gauge!(
        "status_monitor_pg_pool_size",
        "Total connections currently held in the sqlx Postgres pool (idle + in-use). Bounded above by the configured max_connections"
    );
    describe_gauge!(
        "status_monitor_pg_pool_idle",
        "Connections sitting idle in the Postgres pool. A persistent idle = 0 alongside in_use at the max means saturation"
    );
    describe_gauge!(
        "status_monitor_pg_pool_in_use",
        "Connections checked out of the Postgres pool right now (size − idle). Alert when the ratio against pool_size stays high for several minutes"
    );
    describe_gauge!(
        "status_monitor_process_resident_bytes",
        "Resident set size of the status-monitor process in bytes (VmRSS on Linux). Early-warning signal for slow leaks and unbounded growth before the OOM killer triggers; not exposed on non-Linux platforms"
    );
    describe_gauge!(
        "status_monitor_clickhouse_max_part_count_for_partition",
        "ClickHouse MaxPartCountForPartition (sampled from system.asynchronous_metrics). Early warning for partition explosion — climbs toward parts_to_throw_insert (default 3000) if a high-cardinality column is ever added to PARTITION BY"
    );
    describe_counter!(
        "status_monitor_http_requests_total",
        "HTTP requests handled, labelled by method, MatchedPath route, and status class (2xx/3xx/4xx/5xx/other). The route label is the path-pattern with placeholders so cardinality is bounded by the router's static route table"
    );
    describe_histogram!(
        "status_monitor_http_request_duration_ms",
        "HTTP request latency in milliseconds, labelled by method and MatchedPath route. Primary SLO signal — query the {quantile=\"0.99\"} series for tail latency"
    );
    describe_gauge!(
        "status_monitor_http_responses_inflight",
        "HTTP requests currently being served. Climbing alongside flat throughput means handlers are blocking on something (usually a downstream pool); a release-valve signal for upstream contention"
    );
    describe_counter!(
        "status_monitor_ratelimit_drops_total",
        "Per-org and per-user rate-limit rejections (HTTP 429), labelled by `scope` (the same string carried in the error response — e.g. `per_org_api_writes`, `per_user_bulk_ops`). Abuse signal: a sudden rate growth on one scope is the first indicator of a single tenant hammering the API"
    );
}

pub mod names {
    pub const CHECKS_TOTAL: &str = "status_monitor_checks_total";
    pub const CHECK_ERRORS: &str = "status_monitor_checks_errors_total";
    pub const CHECK_REDIRECTS: &str = "status_monitor_check_redirects_total";
    pub const BREAKER_STATE_CHANGES: &str = "status_monitor_circuit_breaker_state_changes_total";
    pub const STORAGE_WRITES: &str = "status_monitor_storage_writes_total";
    pub const STORAGE_DROPPED: &str = "status_monitor_storage_dropped_results_total";
    pub const CHECK_DURATION_MS: &str = "status_monitor_check_duration_ms";
    pub const CHECK_DNS_MS: &str = "status_monitor_check_dns_ms";
    pub const CHECK_CONNECT_MS: &str = "status_monitor_check_connect_ms";
    pub const CHECK_TLS_MS: &str = "status_monitor_check_tls_ms";
    pub const CHECK_TTFB_MS: &str = "status_monitor_check_ttfb_ms";
    pub const STORAGE_BATCH_SIZE: &str = "status_monitor_storage_batch_size";
    pub const STORAGE_WRITE_DURATION_MS: &str = "status_monitor_storage_write_duration_ms";
    pub const TARGETS_TOTAL: &str = "status_monitor_targets_total";
    pub const WORKERS_IN_FLIGHT: &str = "status_monitor_workers_in_flight";
    pub const RESULT_QUEUE_DEPTH: &str = "status_monitor_result_queue_depth";
    pub const BREAKERS_OPEN: &str = "status_monitor_circuit_breakers_open";
    pub const NOTIFICATIONS_TOTAL: &str = "status_monitor_notifications_total";
    pub const NOTIFICATIONS_FAILURES: &str = "status_monitor_notifications_failures_total";
    pub const ALERTS_DROPPED: &str = "status_monitor_alerts_dropped_total";
    pub const HOST_THROTTLE_WAITS: &str = "status_monitor_host_throttle_waits_total";
    pub const HOST_THROTTLE_DROPS: &str = "status_monitor_host_throttle_drops_total";
    pub const RDAP_SINGLEFLIGHT: &str = "status_monitor_rdap_singleflight_total";
    pub const DOMAIN_EXPIRY_STALE_SERVED: &str = "status_monitor_domain_expiry_stale_served_total";
    pub const DOMAIN_EXPIRY_STATE_WRITE_FAILED: &str =
        "status_monitor_domain_expiry_state_write_failed_total";
    pub const RDAP_SINGLEFLIGHT_SLOTS: &str = "status_monitor_rdap_singleflight_slots";
    pub const SCHEDULER_REFRESH_FAILED: &str = "status_monitor_scheduler_refresh_failed_total";
    pub const SCHEDULER_CONSECUTIVE_REFRESH_FAILURES: &str =
        "status_monitor_scheduler_consecutive_refresh_failures";
    pub const SCHEDULER_REFRESH_DURATION_MS: &str = "status_monitor_scheduler_refresh_duration_ms";
    pub const PG_POOL_SIZE: &str = "status_monitor_pg_pool_size";
    pub const PG_POOL_IDLE: &str = "status_monitor_pg_pool_idle";
    pub const PG_POOL_IN_USE: &str = "status_monitor_pg_pool_in_use";
    pub const PROCESS_RESIDENT_BYTES: &str = "status_monitor_process_resident_bytes";
    pub const CLICKHOUSE_MAX_PART_COUNT: &str =
        "status_monitor_clickhouse_max_part_count_for_partition";
    pub const HTTP_REQUESTS_TOTAL: &str = "status_monitor_http_requests_total";
    pub const HTTP_REQUEST_DURATION_MS: &str = "status_monitor_http_request_duration_ms";
    pub const HTTP_RESPONSES_INFLIGHT: &str = "status_monitor_http_responses_inflight";
    pub const RATELIMIT_DROPS: &str = "status_monitor_ratelimit_drops_total";
}
