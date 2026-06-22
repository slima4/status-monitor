# Dashboards managed as code, one JSON file per board under dashboards/.
#
# Each JSON is Grafana's "export for sharing" form: panels reference the
# datasource as the template input "${DS_PROMETHEUS}". Two transforms before
# upload:
#
#  1. Substitute "${DS_PROMETHEUS}" with the real Prometheus datasource uid —
#     the provider POSTs to /api/dashboards/db, which does NOT run __inputs
#     templating (only the UI import path does), so the placeholder must be
#     concrete or every panel loses its datasource. `$${` escapes Terraform
#     interpolation so the literal token is matched (replace() is substring).
#  2. Drop __inputs / __requires. Grafana strips these server-side on save;
#     uploading them makes the provider diff local-with vs server-without and
#     re-push on every apply.
#
# Files live in this module (terraform/dashboards/) because HCP Terraform
# remote-exec only uploads the terraform/ working dir — a file() reaching
# outside it fails on the remote runner. These are the single source: the
# metric-name drift gate (dashboards/grafana/check-metric-names.sh) reads them
# from here too. Adding a board is just dropping a new *.json in that dir.
locals {
  dashboard_files = fileset("${path.module}/dashboards", "*.json")
  dashboards = {
    for f in local.dashboard_files : f => jsonencode({
      for k, v in jsondecode(replace(
        file("${path.module}/dashboards/${f}"),
        "$${DS_PROMETHEUS}",
        data.grafana_data_source.prometheus.uid
      )) : k => v
      if !contains(["__inputs", "__requires"], k)
    })
  }
}

resource "grafana_dashboard" "board" {
  for_each    = local.dashboards
  folder      = grafana_folder.obs.uid
  overwrite   = true
  config_json = each.value
}
