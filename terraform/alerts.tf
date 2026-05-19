resource "grafana_folder" "obs" {
  title = "status-monitor"
}

# Two pipeline-health alerts. Each rule = a Prometheus query (ref A)
# feeding a server-side threshold expression (ref C); the rule fires
# when C is true. Datasource UID resolved by name (portable).
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
      model = jsonencode({
        refId      = "C"
        type       = "threshold"
        expression = "A"
        conditions = [{ evaluator = { type = "gt", params = [0] } }]
      })
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
      model = jsonencode({
        refId      = "C"
        type       = "threshold"
        expression = "A"
        conditions = [{ evaluator = { type = "gt", params = [0] } }]
      })
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
resource "grafana_notification_policy" "root" {
  contact_point = grafana_contact_point.default.name
  group_by      = ["alertname"]
}
