locals {
  # Every rule's condition C is the same fixed pass-through: each rule
  # puts its real comparison in the A expr (`... > N`), so C only fires
  # when A returns any series. Byte-identical across all six rules —
  # hoisted here, referenced as local.threshold_c.
  threshold_c = jsonencode({
    refId      = "C"
    type       = "threshold"
    expression = "A"
    conditions = [{ evaluator = { type = "gt", params = [0] } }]
  })
}

resource "grafana_folder" "obs" {
  title = "status-monitor"
}

# Six pipeline-health alerts. Each rule = a Prometheus query (ref A)
# feeding the shared server-side threshold expression (ref C =
# local.threshold_c); the rule fires when C is true. Datasource UID
# resolved by name (portable).
resource "grafana_rule_group" "pipeline" {
  name             = "status-monitor-pipeline"
  folder_uid       = grafana_folder.obs.uid
  interval_seconds = 60

  # Checks run but results are not persisting (storage write failures
  # or dropped results) — the silent data-loss class (e.g. ClickHouse
  # auth/availability). no_data = OK: absence of the metric means no
  # writes attempted, which PipelineStalled covers instead.
  rule {
    name           = "StatusMonitorResultsLost"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: check results being dropped"
      description = "storage write failures or dropped results > 0 for 5m — checks run but results are not persisting. Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      relative_time_range {
        from = 600
        to   = 0
      }
      # `or` (not `+`): the two counters are independent and each is
      # absent until its first increment. `a + b` vector-matches and
      # is empty unless BOTH exist — so a single-class failure (e.g.
      # only writes{result="failure"} on a ClickHouse outage) would
      # yield no data and silently stay OK. `> 0 or > 0` fires on
      # either.
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "sum(rate(status_monitor_storage_dropped_results_total[5m])) > 0 or sum(rate(status_monitor_storage_writes_total{result=\"failure\"}[5m])) > 0"
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

  # Targets configured but zero checks executing for 10m. no_data =
  # Alerting: a vanished metric means the app/scheduler is down, which
  # is exactly the condition; for=10m rides through deploy restarts.
  rule {
    name           = "StatusMonitorPipelineStalled"
    condition      = "C"
    for            = "10m"
    no_data_state  = "Alerting"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: pipeline stalled"
      description = "targets configured but no checks executing for 10m (scheduler/worker/app down). Runbook: runbooks/grafana-cloud.md."
    }
    data {
      ref_id         = "A"
      datasource_uid = data.grafana_data_source.prometheus.uid
      # 20m window vs the [10m] rate selector (2x headroom, parity
      # with the 5m rule). Stalled-but-up → expr returns sum(targets)
      # (>0) → C fires; app fully down → metrics vanish → no_data →
      # no_data_state=Alerting (for=10m debounces deploy restarts,
      # which keep /metrics down <2m).
      relative_time_range {
        from = 1200
        to   = 0
      }
      model = jsonencode({
        refId   = "A"
        instant = true
        expr    = "(sum(status_monitor_targets_total) > 0) and (sum(rate(status_monitor_checks_total[10m])) == 0)"
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
  # failure here means incidents go unnoticed. `or` for the same reason
  # as ResultsLost: the two counters are independent and each absent
  # until first increment. no_data = OK: nothing dispatched means
  # nothing to fail.
  rule {
    name           = "StatusMonitorNotificationDeliveryFailing"
    condition      = "C"
    for            = "5m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "critical"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: alert delivery failing"
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
        expr    = "sum(rate(status_monitor_notifications_failures_total[5m])) > 0 or sum(rate(status_monitor_alerts_dropped_total[5m])) > 0"
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
    name           = "StatusMonitorCircuitBreakersOpen"
    condition      = "C"
    for            = "15m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: circuit breakers stuck open"
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
        expr    = "sum(status_monitor_circuit_breakers_open) > 0"
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
    name           = "StatusMonitorResultQueueBacklog"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: result queue backing up"
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
        expr    = "max(status_monitor_result_queue_depth) > 500"
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
    name           = "StatusMonitorStorageWriteLatencyHigh"
    condition      = "C"
    for            = "10m"
    no_data_state  = "OK"
    exec_err_state = "Error"
    labels = {
      severity = "warning"
      service  = "status-monitor"
    }
    annotations = {
      summary     = "status-monitor: storage write latency high"
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
        expr    = "histogram_quantile(0.99, sum(rate(status_monitor_storage_write_duration_ms_bucket[10m])) by (le)) > 2000"
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
  name = "status-monitor-default"

  email {
    addresses = [var.alert_email]
    subject   = "[{{ .Status | toUpper }}] {{ .CommonLabels.alertname }}"
  }
}

# NOTE: grafana_notification_policy manages the SINGLE root policy for
# the org — applying it REPLACES the stack's current root policy. This
# stack has no prior alerting, so this establishes it; if other
# alerting is ever added it must go through this resource too.
#
# Severity routing, one email contact, differentiated by timing:
#   - root (critical falls through here): page fast, repeat often.
#   - child severity=warning: batch longer, repeat daily — degraded
#     signals shouldn't have paging cadence. continue = false: a
#     matched warning stops here, never also hits the root cadence.
resource "grafana_notification_policy" "root" {
  contact_point   = grafana_contact_point.default.name
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
