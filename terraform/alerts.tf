locals {
  # Every rule's condition C is the same fixed pass-through: each rule
  # puts its real comparison in the A expr (`... > N`), so C only fires
  # when A returns any series. Byte-identical across every rule —
  # hoisted here, referenced as local.threshold_c.
  threshold_c = jsonencode({
    refId      = "C"
    type       = "threshold"
    expression = "A"
    conditions = [{ evaluator = { type = "gt", params = [0] } }]
  })
}

resource "grafana_folder" "obs" {
  title = "uptimepage"
}

# Pipeline-health alerts. Each rule = a Prometheus query (ref A)
# feeding the shared server-side threshold expression (ref C =
# local.threshold_c); the rule fires when C is true. Datasource UID
# resolved by name (portable).
resource "grafana_rule_group" "pipeline" {
  name             = "uptimepage-pipeline"
  folder_uid       = grafana_folder.obs.uid
  interval_seconds = 60

  # Results permanently dropped: the batcher exhausted its retries and
  # gave up, so this is confirmed, irreversible loss (e.g. ClickHouse
  # unreachable long enough to drain the retry budget). Transient write
  # failures that retries recover are only a warning (StorageWriteFailing
  # below), so a critical here always means real data loss. no_data = OK:
  # absence means no writes attempted, which PipelineStalled covers.
  rule {
    name           = "UptimepageResultsLost"
    condition      = "C"
    for            = "2m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: check results permanently dropped"
      description = "results permanently dropped after retries exhausted (rate > 0 for 2m); confirmed data loss. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(rate(uptimepage_storage_dropped_results_total[5m])) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # App-up-but-stalled: targets configured but the check rate is 0 while
  # the process is still serving metrics. Full app-down is owned by
  # UptimepageMetricsPipelineDown (absent(build_info)), so no_data = OK
  # here — a dark pipeline must not double-page off this rule too.
  rule {
    name           = "UptimepagePipelineStalled"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: pipeline stalled"
      description = "targets configured but no checks executing for 10m (scheduler/worker stalled). Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      # `bool` + `or vector(0)` keep both factors a present 0/1 while the
      # app is up, so a healthy or idle process reports a clean 0 (Normal)
      # instead of an empty/NoData flap. checks_total is lazily created on
      # first increment; `or vector(0)` reads "no checks yet" as 0.
      # 20m query window = 2x the [10m] rate selector; for=10m rides
      # deploy restarts.
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "(sum(uptimepage_targets_total) > bool 0) * (sum(rate(uptimepage_checks_total[10m]) or vector(0)) == bool 0)"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Outbound alert delivery is failing (notification dispatch errors or
  # alert signals dropped before the engine). Critical: this is the
  # path that tells users their own monitored sites are down — a silent
  # failure here means incidents go unnoticed. `or` (not `+`): the two
  # counters are independent and each absent until first increment, so a
  # bare `+` matches to empty and would silently stay OK. no_data = OK:
  # nothing dispatched means
  # nothing to fail.
  rule {
    name           = "UptimepageNotificationDeliveryFailing"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: alert delivery failing"
      description = "notification dispatch errors or dropped alert signals > 0 for 5m — outbound incident alerts may not be reaching users. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(rate(uptimepage_notifications_failures_total[5m])) > 0 or sum(rate(uptimepage_alerts_dropped_total[5m])) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Circuit breakers stuck Open. Transient trips are normal and
  # self-heal, so for = 15m: alert only when breakers stay open (a
  # wedged upstream or a target permanently failing). Warning, not
  # critical — checks still run; this is degraded coverage, not data
  # loss.
  rule {
    name           = "UptimepageCircuitBreakersOpen"
    condition      = "C"
    for            = "15m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: circuit breakers stuck open"
      description = "one or more circuit breakers Open for 15m — a target/upstream is persistently failing and its checks are being skipped. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 300
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(uptimepage_circuit_breakers_open) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # One monitor owning a class the node could not run at all. family=internal
  # is exactly that — no dependence on what the target did, so unlike
  # transport and verdict errors it has no honest steady state. Pairing it
  # with a near-total share separates a defect concentrated in one monitor
  # from an outage spread across the fleet. Warning: monitoring is lying
  # about one monitor, nothing is lost.
  #
  # The floor is 10, not a fraction of the fleet: internal classes are rare
  # by construction, and a monitor on the 60s heartbeat floor in one region
  # only produces ~15 checks per 15m window. The freshness gate is load
  # bearing — every class gauge holds its last value when a sweep fails, so
  # without it a stalled sweep keeps this rule firing on stale data forever.
  # Confirm the numbers against real class volumes once there is history.
  rule {
    name           = "UptimepageProbeErrorConcentrated"
    condition      = "C"
    for            = "2h"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: one monitor owns a probe-side error class"
      description = "a single monitor accounts for ~all checks in a family=internal error class for 2h — the probe is failing on its own account, not because the target is down. Search the app log for the class to get the monitor id. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 900
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "uptimepage_check_error_class_top_monitor_share{family=\"internal\"} >= 0.9 and uptimepage_check_error_class_checks{family=\"internal\"} >= 10 and on() (uptimepage_check_error_class_sweep_age_seconds < 300)"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # The error-class sweep is stuck at its row cap. Raw error strings carry
  # hostnames and IPs, so their count grows with the fleet; past the cap a
  # low-volume class publishes as 0 and ProbeErrorConcentrated above simply
  # stops being able to fire. Without this the coverage loss is invisible,
  # because a blind alert and a quiet fleet look identical. for = 1h so a
  # transient burst of distinct errors does not page.
  rule {
    name           = "UptimepageErrorClassSweepTruncated"
    condition      = "C"
    for            = "1h"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: error-class sweep is hitting its row cap"
      description = "the sweep has been truncating for 1h, so low-volume error classes report 0 and the concentration alert cannot fire. Raise the cap or scope the query. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 900
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "uptimepage_check_error_class_truncated > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Result queue backing up. Brief depth is normal; sustained high
  # depth is backpressure that precedes dropped results (which
  # ResultsLost catches only once data is already lost). Warning,
  # for = 10m. Threshold 500 is a starting point — tune from the
  # result_queue_depth panel once a baseline is known.
  rule {
    name           = "UptimepageResultQueueBacklog"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: result queue backing up"
      description = "result queue depth > 500 for 10m — write path is not keeping up; dropped results may follow. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 300
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(uptimepage_result_queue_depth) > 500"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Scheduler stuck: the registry refresh has failed N consecutive times
  # under exponential backoff. The gauge resets to 0 on the first
  # successful refresh, so a sustained high value means new
  # customer-added targets aren't being picked up. Critical — every
  # minute past the streak is a minute of stale registry. no_data = OK:
  # absence of the gauge means the scheduler isn't even reporting
  # (PipelineStalled covers that case).
  rule {
    name           = "UptimepageRegistryRefreshStuck"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: scheduler refresh stuck"
      description = "registry refresh has failed 5+ consecutive times for 5m — newly-added customer targets are not being scheduled. Backoff is active (up to 10× refresh interval). Likely cause: Postgres outage, schema drift, or cipher misconfig. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 300
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(uptimepage_scheduler_consecutive_refresh_failures) > 5"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Registry refresh latency creeping up — the trigger for the deferred
  # incremental-sync work in the scaling-roadmap. 500ms p99 over 15m is
  # the threshold described in the metric describe text; tune as the
  # baseline shifts. Warning — checks still run; this signals "the
  # full-scan refresh is starting to strain" not "it's broken".
  # Exporter ships summaries (no _bucket), hence the {quantile} label
  # rather than histogram_quantile() — per docs/metrics.md.
  rule {
    name           = "UptimepageRegistryRefreshSlow"
    condition      = "C"
    for            = "15m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: registry refresh latency high"
      description = "registry refresh p99 > 500ms for 15m — the full-scan refresh is starting to strain at the current org count. Trigger to start the deferred incremental-sync work. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(uptimepage_scheduler_refresh_duration_ms{quantile=\"0.99\"}) > 500"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Postgres pool saturating. in_use / size > 85% for 5m means almost
  # every request is competing for the last few connections; the next
  # spike will start timing out. Critical: pool exhaustion takes the
  # whole app down (every handler awaits the pool). Tune the ratio if
  # we observe sustained healthy operation closer to the line.
  rule {
    name           = "UptimepagePgPoolSaturating"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: Postgres pool saturating"
      description = "Postgres pool in_use / size > 0.85 for 5m — pool exhaustion is imminent. Likely causes: slow queries holding connections, traffic spike, or pool max_connections too low for current load. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 300
        to   = 0
      }
      # max() handles the single-instance case correctly; / size avoids a
      # fixed threshold that would drift as the pool is resized.
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "(max(uptimepage_pg_pool_in_use) / max(uptimepage_pg_pool_size)) > 0.85"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Customer-visible 5xx error rate. Ratio (not absolute count) so the
  # threshold stays sensible across traffic volumes; for=10m rides
  # through deploy restarts and brief blips. 1% is a deliberate
  # SLO-grade threshold: 99% success leaves headroom against the 99.9%
  # we want to advertise. Critical — server-side errors are the most
  # direct signal that customers see broken pages. The `> 0.1` volume
  # gate stops a single 5xx in a near-idle minute from tripping the
  # ratio; tune the floor as steady-state traffic grows.
  rule {
    name           = "UptimepageHttp5xxRateHigh"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: HTTP 5xx error rate above 1%"
      description = "server-side errors are above 1% of total request volume for 10m — customers are hitting broken responses. Inspect `http_requests_total{status=\"5xx\"}` by route in the dashboard to localise. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "(sum(rate(uptimepage_http_requests_total{status=\"5xx\"}[5m])) / sum(rate(uptimepage_http_requests_total[5m]))) > 0.01 and sum(rate(uptimepage_http_requests_total[5m])) > 0.1"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # HTTP tail latency. Pick the worst route's p99 across the window —
  # `max(... {quantile="0.99"})` rather than histogram_quantile because
  # the exporter emits summaries (see docs/metrics.md). 1s for 15m is
  # the warning floor; tighten as steady-state baselines settle in.
  rule {
    name           = "UptimepageHttpLatencyHigh"
    condition      = "C"
    for            = "15m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: HTTP request p99 above 1s"
      description = "the slowest route's p99 > 1000ms for 15m — at least one endpoint is degrading. Use the Per-route p99 panel to identify which route, then `route` label drills further. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(uptimepage_http_request_duration_ms{quantile=\"0.99\"}) > 1000"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Storage write p99 latency high. histogram_quantile over the _bucket
  # series; p99 > 2s sustained for 10m means ClickHouse is degrading
  # before it starts dropping. rate over [10m] (not [5m]): on a
  # low-traffic instance a 5m bucket is sparse, so one slow write would
  # dominate p99 and false-fire. Warning — early signal ahead of
  # ResultsLost.
  rule {
    name           = "UptimepageStorageWriteLatencyHigh"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: storage write latency high"
      description = "storage write p99 > 2s for 10m — ClickHouse degrading; dropped results may follow. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "histogram_quantile(0.99, sum(rate(uptimepage_storage_write_duration_ms_bucket[10m])) by (le)) > 2000"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # ClickHouse partition-explosion early warning. MaxPartCountForPartition
  # (sampled app-side into this gauge) climbing toward parts_to_throw_insert
  # (default 3000) means inserts are about to be rejected fleet-wide. A healthy
  # day-partitioned schema sits in the low tens; 1500 is half the hard ceiling,
  # ample lead time to react (almost always: a high-cardinality column was added
  # to PARTITION BY, or merges fell behind). no_data = OK: the gauge is absent
  # when CH is unreachable, which ResultsLost / StorageWriteLatencyHigh cover.
  rule {
    name           = "UptimepageClickHousePartsHigh"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: ClickHouse partition count climbing"
      description = "MaxPartCountForPartition > 1500 (hard limit parts_to_throw_insert=3000) for 10m — likely a high-cardinality column added to PARTITION BY, or merges falling behind. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(uptimepage_clickhouse_max_part_count_for_partition) > 1500"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Ingest buffer overflowing — the bounded batcher buffer evicting oldest
  # results because sustained ingest exceeds what ClickHouse can absorb. Split
  # from the generic ResultsLost so the page points at CAPACITY (scale CH, raise
  # buffer_size, or the check-interval floor) instead of a CH outage, which
  # ResultsLost's description would otherwise misdiagnose. Warning, not critical:
  # ResultsLost already pages critical for the data loss itself; this rule's job
  # is the correct diagnosis. no_data = OK (no drops emitted = nothing wrong).
  rule {
    name           = "UptimepageIngestBufferOverflow"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: ingest buffer overflowing (capacity)"
      description = "results dropped with reason=buffer_overflow for 5m — sustained ingest exceeds ClickHouse write throughput and the batcher is evicting oldest results. Capacity action (scale CH / raise buffer_size / check the interval floor), not a CH outage. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 300
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(rate(uptimepage_storage_dropped_results_total{reason=\"buffer_overflow\"}[5m])) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Root disk filling. A full disk stalls ClickHouse inserts (Code 243
  # NOT_ENOUGH_SPACE) and crashes Postgres on ENOSPC, and the pipeline alerts
  # only fire once results are already lost. Warn at 80% used so there is time
  # to reclaim (prune images, cap system logs) before the wall. Used-fraction
  # (not free) so the value stays positive at 100% full and ref-C's `> 0` keeps
  # firing. Sourced from the node-exporter sidecar (metrics profile); no_data =
  # OK so a host without that profile never false-fires. Blind spot: if
  # node-exporter alone dies while the app keeps reporting, disk goes unwatched
  # until it returns.
  rule {
    name           = "UptimepageDiskSpaceLow"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: root disk over 80% full"
      description = "less than 20% root-disk space free for 10m. Reclaim before it fills: prune docker images, check ClickHouse system-log and data growth. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(1 - node_filesystem_avail_bytes{mountpoint=\"/\"} / node_filesystem_size_bytes{mountpoint=\"/\"}) > 0.80"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Root disk nearly full: ClickHouse NOT_ENOUGH_SPACE and a Postgres ENOSPC
  # crash are minutes away at this point. Critical, and a lower `for` than the
  # warning so it pages fast.
  rule {
    name           = "UptimepageDiskSpaceCritical"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: root disk over 90% full"
      description = "less than 10% root-disk space free for 5m. ClickHouse inserts and Postgres will start failing as it reaches 100%. Reclaim disk now. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(1 - node_filesystem_avail_bytes{mountpoint=\"/\"} / node_filesystem_size_bytes{mountpoint=\"/\"}) > 0.90"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Storage writes are failing but retries still recover them, so nothing
  # is permanently lost yet (that is ResultsLost, critical). Count failures
  # over the last hour instead of an instantaneous rate: sparse but
  # recurring failures (a flush failing every few minutes) never hold a
  # [5m] rate above zero, so a rate gate misses them. More than 3 in an
  # hour trips on a real recurring problem while a lone transient blip
  # stays quiet. A slow disk-fill is still caught sooner by the DiskSpaceLow
  # alerts. no_data = OK: no writes attempted.
  rule {
    name           = "UptimepageStorageWriteFailing"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: storage writes failing (retried)"
      description = "more than 3 storage write failures in the last hour while retries still recover them; degrading write path, no confirmed loss. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 7200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(increase(uptimepage_storage_writes_total{result=\"failure\"}[1h])) > 3"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Host memory nearly exhausted. MemAvailable is the kernel's own estimate of
  # allocatable memory without swapping; under 15% and the OOM killer is near,
  # which would take down Postgres or ClickHouse. Warning: the hard failure is
  # the process death that follows, this is the lead time to act.
  rule {
    name           = "UptimepageHostMemoryLow"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: host memory low"
      description = "less than 15% host memory available for 10m; the OOM killer is near and can kill Postgres or ClickHouse. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(1 - node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes) > 0.85"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Sustained host CPU saturation. 1 minus the mean idle rate across cores is
  # the busy fraction; above 90% for 15m the box is compute-bound and checks
  # queue and latency climbs. Warning: rarely a page alone, but the context
  # when a latency or queue alert fires with it.
  rule {
    name           = "UptimepageHostCpuHigh"
    condition      = "C"
    for            = "15m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: host CPU high"
      description = "host CPU above 90% busy for 15m; the box is compute-bound. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "1 - avg(rate(node_cpu_seconds_total{mode=\"idle\"}[5m])) > 0.90"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Root filesystem inodes running out. A filesystem can hit 100% inodes with
  # bytes still free (many tiny files) and writes fail exactly like a full disk.
  # Mirrors DiskSpaceLow at 80% used; same filesystem collector feeds both.
  rule {
    name           = "UptimepageInodesLow"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: root inodes over 80% used"
      description = "less than 20% root-filesystem inodes free for 10m; writes fail on inode exhaustion even with bytes free. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(1 - node_filesystem_files_free{mountpoint=\"/\"} / node_filesystem_files{mountpoint=\"/\"}) > 0.80"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Root inodes nearly exhausted at 90% used. At 100% every create and write
  # fails like a full disk, with bytes still free. Critical, mirrors
  # DiskSpaceCritical.
  rule {
    name           = "UptimepageInodesCritical"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: root inodes over 90% used"
      description = "less than 10% root-filesystem inodes free for 5m; approaching inode exhaustion, where writes fail with bytes still free. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "max(1 - node_filesystem_files_free{mountpoint=\"/\"} / node_filesystem_files{mountpoint=\"/\"}) > 0.90"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }
}

# Availability / dead-man alerts. Every rule above assumes data is flowing
# and goes quiet when it stops; these fire on the absence itself, so a dark
# pipeline can't masquerade as "all healthy".
resource "grafana_rule_group" "availability" {
  name             = "uptimepage-availability"
  folder_uid       = grafana_folder.obs.uid
  interval_seconds = 60

  # No metrics are reaching Grafana Cloud at all (control plane down, Alloy
  # down, or remote_write broken). build_info is emitted by every process on
  # each scrape, so its absence means the whole app -> Alloy -> remote_write
  # path is dead. no_data = OK: when metrics flow, absent() returns nothing,
  # which is the healthy state; for=10m rides a blue/green deploy (one color
  # always keeps build_info present).
  rule {
    name           = "UptimepageMetricsPipelineDown"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: metrics pipeline down"
      description = "no uptimepage metrics have reached Grafana Cloud for 10m (control plane, Alloy, or remote_write down). Monitoring is blind. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "absent(uptimepage_build_info)"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # Control-plane process is gone — the binary hosting the web app and
  # scheduler stopped reporting (host/container down), even if a probe agent
  # still is. build_info is startup liveness, not subsystem health, so a
  # scheduler stall with the process alive stays PipelineStalled's job.
  # MetricsPipelineDown above keys on any build_info series, so a surviving
  # agent masks a control-plane-only outage; the role label scopes this to
  # blue/green. no_data = OK: build_info present means healthy; for=10m rides
  # blue/green (one color stays up).
  rule {
    name           = "UptimepageControlPlaneDown"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: control plane down"
      description = "no control-plane metrics for 10m — the web app + scheduler process is not reporting (host or container down) even if a probe agent still is. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "absent(uptimepage_build_info{role=\"control-plane\"})"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # One or more enabled probe agents stopped checking in, so their regions'
  # monitors are no longer being probed. Sourced from agents.last_seen_at via
  # the control plane, so it covers remote agents too. The gauge is recomputed
  # every sweep (a recovered, disabled, or removed agent drops out), so it
  # can't latch the way a frozen per-agent series would. no_data = OK: no
  # agents, or the brain itself dark (the rule above owns that case).
  rule {
    name           = "UptimepageRegionalAgentDown"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: probe agent down"
      description = "one or more enabled probe agents have not checked in for over the staleness window, so their regions are unprobed. See /operator/agents for which. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "uptimepage_agents_enabled_down"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }
}

# Subscription-abuse alerts. Fires when confirm/verify email volume spikes
# above a threshold consistent with subscription stuffing (someone or a bot
# submitting the public /subscribe form at scale). The existing per-address
# and per-page caps bound the actual harm; this alert is the early-warning
# signal that lets the operator investigate before the caps exhaust.
resource "grafana_rule_group" "abuse" {
  name             = "uptimepage-abuse"
  folder_uid       = grafana_folder.obs.uid
  interval_seconds = 60

  # A confirm-email rate above 1/s over 5 minutes means more than 300
  # confirm mails sent from a single process in that window — well above
  # any organic subscribe wave (the per-address daily cap is 10, per-page
  # is 200). for = 10m avoids alerting on a legitimate traffic spike from
  # a blog post linking a status page; sustained elevated rate is the
  # signal. Warning: the caps bound the harm; this is an awareness
  # signal, not a production emergency.
  # no_data = OK: the counter is absent until the first confirm mail is sent.
  # Tune the threshold from the uptimepage_confirm_emails_total panel once
  # a real-traffic baseline is established.
  rule {
    name           = "UptimepageConfirmEmailSpike"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: confirm-email send rate spike"
      description = "confirm/verify email send rate (outcome=sent) > 1/s over 5m for 10m — possible subscription-stuffing attack on the public /subscribe endpoint or the alert-channel verification flow. Check uptimepage_confirm_emails_total{path,outcome} and the quota_events table for per-address abuse patterns. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 900
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(rate(uptimepage_confirm_emails_total{outcome=\"sent\"}[5m])) > 1"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }
}

# Churn alerts. Not health — nothing here means the product is broken. Every
# rule is a customer leaving while there is still time to ask why, so both fire
# at warning: a nudge to look, not a page.
resource "grafana_rule_group" "churn" {
  name             = "uptimepage-churn"
  folder_uid       = grafana_folder.obs.uid
  interval_seconds = 300

  # Reversible for the whole grace window, so this is the one churn signal
  # that arrives while the outcome can still change.
  rule {
    name           = "UptimepageAccountDeletionRequested"
    condition      = "C"
    for            = "0s"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: an account was scheduled for deletion"
      description = "a customer requested account deletion in the last hour. It is reversible until the grace window closes. Find them in org_audit_log (action = 'user.deletion_requested')."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 3600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "increase(uptimepage_account_deletions_requested_total[1h]) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }

  # A customer clearing the account by hand, which is what they resort to when
  # they cannot complete a deletion. The org then looks like a live tenant
  # forever.
  rule {
    name           = "UptimepageOrgEmptied"
    condition      = "C"
    for            = "0s"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "uptimepage"
    }
    annotations = {
      summary     = "uptimepage: an org deleted its last monitor"
      description = "an organisation went from having monitors to having none in the last hour. Find it in org_audit_log (action = 'target.deleted' / 'target.bulk_deleted')."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 3600
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "increase(uptimepage_orgs_emptied_total[1h]) > 0"
      })
    }
    data {
      ref_id         = "C"
      datasource_uid = "__expr__"
      relative_time_range {
        from = 0
        to   = 0
      }
      model = local.threshold_c
    }
  }
}

resource "grafana_contact_point" "default" {
  name = "uptimepage-default"

  email {
    addresses = [var.alert_email]
    subject   = "[{{ .Status | toUpper }}] {{ .CommonLabels.alertname }}"
  }

  # Renaming the contact point forces replace; the root policy references it
  # by name, so the new one must exist (and the policy re-point) before the
  # old is deleted — otherwise Grafana 409s the in-use delete.
  lifecycle {
    create_before_destroy = true
  }
}

# Critical path: email plus a Telegram push so a real outage reaches a
# phone, not just an inbox that can sit unread overnight. Warnings stay
# on the email-only default — degraded signals don't earn a push.
resource "grafana_contact_point" "critical" {
  name = "uptimepage-critical"

  email {
    addresses = [var.alert_email]
    subject   = "[{{ .Status | toUpper }}] {{ .CommonLabels.alertname }}"
  }

  telegram {
    token   = var.alert_telegram_token
    chat_id = var.alert_telegram_chat_id
  }

  lifecycle {
    create_before_destroy = true
  }
}

# NOTE: grafana_notification_policy manages the SINGLE root policy for
# the org — applying it REPLACES the stack's current root policy. This
# stack has no prior alerting, so this establishes it; if other
# alerting is ever added it must go through this resource too.
#
# Severity routing by channel and timing:
#   - root (critical falls through here): email + Telegram, page fast,
#     repeat often.
#   - child severity=warning: email-only default, batch longer, repeat
#     daily — degraded signals shouldn't have paging cadence. continue =
#     false: a matched warning stops here, never also hits the root path.
resource "grafana_notification_policy" "root" {
  contact_point   = grafana_contact_point.critical.name
  group_by        = ["alertname"]
  group_wait      = "30s"
  group_interval  = "5m"
  repeat_interval = "4h"

  policy {
    contact_point = grafana_contact_point.default.name
    group_by      = ["alertname"]
    continue      = false

    matcher {
      label = "severity"
      match = "="
      value = "warning"
    }

    group_wait     = "5m"
    group_interval = "30m"
    # "1d", not "24h": Grafana canonicalizes durations >= a day to the
    # day form server-side, so "24h" would diff back to "1d" on every
    # plan (perpetual no-op drift). Write the canonical form.
    repeat_interval = "1d"
  }
}
