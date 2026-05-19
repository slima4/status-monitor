# Manage the repo dashboard declaratively. The exported JSON uses the
# ${DS_PROMETHEUS} input placeholder; substitute the resolved
# datasource UID so panels bind correctly. Additive + idempotent
# (overwrite = true); the import block adopts the already-live
# dashboard (uid embedded in the JSON) instead of duplicating it.
resource "grafana_dashboard" "overview" {
  folder    = grafana_folder.obs.uid
  overwrite = true

  config_json = replace(
    file("${path.module}/../dashboards/grafana/status-monitor-overview.json"),
    "$${DS_PROMETHEUS}",
    data.grafana_data_source.prometheus.uid
  )
}

# Adopt the existing live dashboard. If it was never uploaded (plan
# says "Import block ... not found"), delete this block — the resource
# then simply creates it. Provider 3.x dashboard import id is the
# dashboard uid; adjust to "<folderUID>/<uid>" only if plan asks.
import {
  to = grafana_dashboard.overview
  id = "status-monitor-overview"
}
