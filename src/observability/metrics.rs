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
    metrics::counter!("uptimepage_build_info", "version" => env!("CARGO_PKG_VERSION")).absolute(1);
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
        "uptimepage_checks_total",
        "Total checks completed, labelled by status"
    );
    describe_counter!(
        "uptimepage_checks_errors_total",
        "Total check errors, labelled by kind"
    );
    describe_counter!(
        "uptimepage_check_redirects_total",
        "HTTP redirect hops, labelled by outcome \
         (followed | limit_exceeded | invalid_location | blocked_scheme)"
    );
    describe_counter!(
        "uptimepage_circuit_breaker_state_changes_total",
        "Circuit breaker state transitions"
    );
    describe_counter!(
        "uptimepage_storage_writes_total",
        "Storage writes, labelled by store and result"
    );
    describe_counter!(
        "uptimepage_storage_dropped_results_total",
        "Results dropped before storage, labelled by reason"
    );
    describe_counter!(
        "uptimepage_notifications_dead_lettered_total",
        "Incident pages that exhausted all retries without delivering, labelled by transport"
    );

    describe_histogram!(
        "uptimepage_check_duration_ms",
        "Total check duration in milliseconds"
    );
    describe_histogram!(
        "uptimepage_check_dns_ms",
        "DNS resolution latency in milliseconds"
    );
    describe_histogram!(
        "uptimepage_check_connect_ms",
        "TCP connect latency in milliseconds (recorded only when a new connection is established)"
    );
    describe_histogram!(
        "uptimepage_check_tls_ms",
        "TLS handshake latency in milliseconds (recorded only when a new HTTPS connection is established)"
    );
    describe_histogram!(
        "uptimepage_check_ttfb_ms",
        "HTTP time-to-first-byte in milliseconds"
    );
    describe_histogram!(
        "uptimepage_storage_batch_size",
        "Result batch size at flush time"
    );
    describe_histogram!(
        "uptimepage_storage_write_duration_ms",
        "Storage write duration in milliseconds"
    );

    describe_gauge!(
        "uptimepage_targets_total",
        "Targets in this process's scheduler registry. Non-zero only where in-process probing runs; a brain with agent-only probing reports 0 by design — use uptimepage_targets_enabled for the configured-monitor count"
    );
    describe_gauge!(
        "uptimepage_targets_enabled",
        "Configured enabled monitors counted from Postgres, labelled by kind. Inventory gauge refreshed on a slow cadence and scrape-cached, so request load never reaches Postgres. Source of truth for the dashboard monitor count regardless of where probing runs"
    );
    describe_gauge!(
        "uptimepage_users_active",
        "Non-deleted user accounts counted from Postgres. Slow-cadence inventory gauge, scrape-cached"
    );
    describe_gauge!("uptimepage_workers_in_flight", "Checks currently executing");
    describe_gauge!(
        "uptimepage_result_queue_depth",
        "Current depth of the result channel buffer"
    );
    describe_gauge!(
        "uptimepage_circuit_breakers_open",
        "Number of circuit breakers currently in the Open state"
    );
    describe_gauge!(
        "uptimepage_agent_last_seen_age_seconds",
        "Seconds since a regional agent last checked in, labelled by region and agent"
    );
    describe_gauge!(
        "uptimepage_agent_up",
        "1 if a regional agent checked in within the staleness window, else 0"
    );
    describe_counter!(
        "uptimepage_notifications_total",
        "Alert notifications dispatched, labelled by channel and kind"
    );
    describe_counter!(
        "uptimepage_notifications_failures_total",
        "Alert notification dispatches that returned an error, labelled by channel"
    );
    describe_counter!(
        "uptimepage_alerts_dropped_total",
        "Incident paging signals dropped before reaching the escalation engine, labelled by reason. The incident row stays in Postgres for the reconcile sweep to retry"
    );
    describe_gauge!(
        "uptimepage_monitors_unmonitored",
        "Monitors whose covering probes have all gone silent (no fresh results), sampled by the silence sweep. Distinct from down: these have no data at all"
    );
    describe_counter!(
        "uptimepage_telegram_send_deferred_total",
        "Telegram sends deferred by the per-bot/per-chat send budget rather than sent immediately. Sustained growth means the central bot is rate-limit bound"
    );
    describe_histogram!(
        "uptimepage_telegram_send_wait_ms",
        "Wait imposed on a Telegram send by the send budget before the slot opened, in milliseconds"
    );
    describe_counter!(
        "uptimepage_rdap_singleflight_total",
        "RDAP singleflight outcomes per domain: hit (cached) or miss (fetched)"
    );
    describe_counter!(
        "uptimepage_domain_expiry_stale_served_total",
        "Times the domain_expiry executor served a cached last-good answer instead of a fresh probe, labelled by failure kind"
    );
    describe_counter!(
        "uptimepage_domain_expiry_state_write_failed_total",
        "Failures writing the last-good cache row after a successful probe — sustained values mean the sticky cache is going cold even though probes succeed"
    );
    describe_gauge!(
        "uptimepage_rdap_singleflight_slots",
        "Live entries in the RDAP singleflight cache. Bounded under normal load by the set of monitored domains"
    );
    describe_counter!(
        "uptimepage_scheduler_refresh_failed_total",
        "Registry refresh ticks that returned an error from Postgres — alert on a sustained rate above your normal noise floor"
    );
    describe_gauge!(
        "uptimepage_scheduler_consecutive_refresh_failures",
        "Consecutive registry refresh failures since the last success. Resets to 0 on recovery; primary alarm signal for a stuck scheduler"
    );
    describe_histogram!(
        "uptimepage_scheduler_refresh_duration_ms",
        "Wall-clock duration of one registry refresh tick (Postgres query + decode + DashMap diff). p99 climbing past a few hundred ms means the full-scan refresh is starting to strain at scale — switch to incremental sync"
    );
    describe_gauge!(
        "uptimepage_pg_pool_size",
        "Total connections currently held in the sqlx Postgres pool (idle + in-use). Bounded above by the configured max_connections"
    );
    describe_gauge!(
        "uptimepage_pg_pool_idle",
        "Connections sitting idle in the Postgres pool. A persistent idle = 0 alongside in_use at the max means saturation"
    );
    describe_gauge!(
        "uptimepage_pg_pool_in_use",
        "Connections checked out of the Postgres pool right now (size − idle). Alert when the ratio against pool_size stays high for several minutes"
    );
    describe_gauge!(
        "uptimepage_process_resident_bytes",
        "Resident set size of the uptimepage process in bytes (VmRSS on Linux). Early-warning signal for slow leaks and unbounded growth before the OOM killer triggers; not exposed on non-Linux platforms"
    );
    describe_gauge!(
        "uptimepage_clickhouse_max_part_count_for_partition",
        "ClickHouse MaxPartCountForPartition (sampled from system.asynchronous_metrics). Early warning for partition explosion — climbs toward parts_to_throw_insert (default 3000) if a high-cardinality column is ever added to PARTITION BY"
    );
    describe_counter!(
        "uptimepage_http_requests_total",
        "HTTP requests handled, labelled by method, MatchedPath route, and status class (2xx/3xx/4xx/5xx/other). The route label is the path-pattern with placeholders so cardinality is bounded by the router's static route table"
    );
    describe_histogram!(
        "uptimepage_http_request_duration_ms",
        "HTTP request latency in milliseconds, labelled by method and MatchedPath route. Primary SLO signal — query the {quantile=\"0.99\"} series for tail latency"
    );
    describe_gauge!(
        "uptimepage_http_responses_inflight",
        "HTTP requests currently being served. Climbing alongside flat throughput means handlers are blocking on something (usually a downstream pool); a release-valve signal for upstream contention"
    );
    describe_counter!(
        "uptimepage_ratelimit_drops_total",
        "Per-org and per-user rate-limit rejections (HTTP 429), labelled by `scope` (the same string carried in the error response — e.g. `per_org_api_writes`, `per_user_bulk_ops`). Abuse signal: a sudden rate growth on one scope is the first indicator of a single tenant hammering the API"
    );
}

pub mod names {
    pub const CHECKS_TOTAL: &str = "uptimepage_checks_total";
    pub const CHECK_ERRORS: &str = "uptimepage_checks_errors_total";
    pub const CHECK_REDIRECTS: &str = "uptimepage_check_redirects_total";
    pub const BREAKER_STATE_CHANGES: &str = "uptimepage_circuit_breaker_state_changes_total";
    pub const STORAGE_WRITES: &str = "uptimepage_storage_writes_total";
    pub const STORAGE_DROPPED: &str = "uptimepage_storage_dropped_results_total";
    pub const NOTIFICATIONS_DEAD_LETTERED: &str = "uptimepage_notifications_dead_lettered_total";
    pub const MONITORS_UNMONITORED: &str = "uptimepage_monitors_unmonitored";
    pub const TELEGRAM_SEND_DEFERRED: &str = "uptimepage_telegram_send_deferred_total";
    pub const TELEGRAM_SEND_WAIT_MS: &str = "uptimepage_telegram_send_wait_ms";
    pub const CHECK_DURATION_MS: &str = "uptimepage_check_duration_ms";
    pub const CHECK_DNS_MS: &str = "uptimepage_check_dns_ms";
    pub const CHECK_CONNECT_MS: &str = "uptimepage_check_connect_ms";
    pub const CHECK_TLS_MS: &str = "uptimepage_check_tls_ms";
    pub const CHECK_TTFB_MS: &str = "uptimepage_check_ttfb_ms";
    pub const STORAGE_BATCH_SIZE: &str = "uptimepage_storage_batch_size";
    pub const STORAGE_WRITE_DURATION_MS: &str = "uptimepage_storage_write_duration_ms";
    pub const TARGETS_TOTAL: &str = "uptimepage_targets_total";
    pub const TARGETS_ENABLED: &str = "uptimepage_targets_enabled";
    pub const USERS_ACTIVE: &str = "uptimepage_users_active";
    pub const WORKERS_IN_FLIGHT: &str = "uptimepage_workers_in_flight";
    pub const RESULT_QUEUE_DEPTH: &str = "uptimepage_result_queue_depth";
    pub const BREAKERS_OPEN: &str = "uptimepage_circuit_breakers_open";
    pub const NOTIFICATIONS_TOTAL: &str = "uptimepage_notifications_total";
    pub const NOTIFICATIONS_FAILURES: &str = "uptimepage_notifications_failures_total";
    pub const ALERTS_DROPPED: &str = "uptimepage_alerts_dropped_total";
    pub const HOST_THROTTLE_WAITS: &str = "uptimepage_host_throttle_waits_total";
    pub const HOST_THROTTLE_DROPS: &str = "uptimepage_host_throttle_drops_total";
    pub const RDAP_SINGLEFLIGHT: &str = "uptimepage_rdap_singleflight_total";
    pub const DOMAIN_EXPIRY_STALE_SERVED: &str = "uptimepage_domain_expiry_stale_served_total";
    pub const DOMAIN_EXPIRY_STATE_WRITE_FAILED: &str =
        "uptimepage_domain_expiry_state_write_failed_total";
    pub const RDAP_SINGLEFLIGHT_SLOTS: &str = "uptimepage_rdap_singleflight_slots";
    pub const AGENT_LAST_SEEN_AGE: &str = "uptimepage_agent_last_seen_age_seconds";
    pub const AGENT_UP: &str = "uptimepage_agent_up";
    pub const SCHEDULER_REFRESH_FAILED: &str = "uptimepage_scheduler_refresh_failed_total";
    pub const SCHEDULER_CONSECUTIVE_REFRESH_FAILURES: &str =
        "uptimepage_scheduler_consecutive_refresh_failures";
    pub const SCHEDULER_REFRESH_DURATION_MS: &str = "uptimepage_scheduler_refresh_duration_ms";
    pub const PG_POOL_SIZE: &str = "uptimepage_pg_pool_size";
    pub const PG_POOL_IDLE: &str = "uptimepage_pg_pool_idle";
    pub const PG_POOL_IN_USE: &str = "uptimepage_pg_pool_in_use";
    pub const PROCESS_RESIDENT_BYTES: &str = "uptimepage_process_resident_bytes";
    pub const CLICKHOUSE_MAX_PART_COUNT: &str =
        "uptimepage_clickhouse_max_part_count_for_partition";
    pub const HTTP_REQUESTS_TOTAL: &str = "uptimepage_http_requests_total";
    pub const HTTP_REQUEST_DURATION_MS: &str = "uptimepage_http_request_duration_ms";
    pub const HTTP_RESPONSES_INFLIGHT: &str = "uptimepage_http_responses_inflight";
    pub const RATELIMIT_DROPS: &str = "uptimepage_ratelimit_drops_total";
}
