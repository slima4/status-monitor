# The overview dashboard, managed as code.
#
# The JSON is Grafana's "export for sharing" form: 41 panels reference
# the datasource as the template input "${DS_PROMETHEUS}", and the
# file carries the export-only top-level keys __inputs / __requires.
# Two transforms before upload:
#
#  1. Substitute "${DS_PROMETHEUS}" with the real Prometheus datasource
#     uid — the provider POSTs to /api/dashboards/db, which does NOT run
#     __inputs templating (only the UI import path does), so the
#     placeholder must be concrete or every panel loses its datasource.
#     `$${` escapes Terraform interpolation so the literal token is
#     matched (replace() is plain substring, not regex).
#  2. Drop __inputs / __requires. Grafana strips these server-side on
#     save; if uploaded, the provider would diff local-with vs
#     server-without on every plan and re-push on every apply
#     (overwrite = true). Removing them keeps plan clean and apply
#     idempotent.
#
# The JSON lives inside this module (terraform/dashboards/) because HCP
# Terraform remote-exec only uploads the terraform/ working dir — a
# file() reaching outside it fails on the remote runner. This is the
# single source: the metric-name drift gate
# (dashboards/grafana/check-metric-names.sh) reads it from here too.
locals {
  _dashboard_raw = jsondecode(replace(
    file("${path.module}/dashboards/status-monitor-overview.json"),
    "$${DS_PROMETHEUS}",
    data.grafana_data_source.prometheus.uid
  ))
  dashboard_json = jsonencode({
    for k, v in local._dashboard_raw : k => v
    if !contains(["__inputs", "__requires"], k)
  })
}

resource "grafana_dashboard" "overview" {
  folder      = grafana_folder.obs.uid
  overwrite   = true
  config_json = local.dashboard_json
}
