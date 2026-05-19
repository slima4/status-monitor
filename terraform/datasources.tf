# Resolve the Grafana Cloud Prometheus datasource UID by name so the
# alert rules stay portable (UIDs are stack-specific). Only the
# datasources referenced by active resources are looked up here; the
# Tempo lookup lives (commented) with the opt-in Loki block.
data "grafana_data_source" "prometheus" {
  name = var.prometheus_datasource_name
}
