# Resolve Grafana Cloud datasource UIDs by name so resources stay
# portable (UIDs are stack-specific). These are read-only lookups —
# they never manage the datasource connection/auth (Grafana Cloud owns
# that), so no apply here can break the metrics/logs/traces pipelines.
data "grafana_data_source" "prometheus" {
  name = var.prometheus_datasource_name
}

# Loki + Tempo: used by the trace_id derived field (derived_field.tf).
# Reading loki.json_data_encoded lets that resource MERGE the derived
# field into Loki's existing jsonData instead of replacing it.
data "grafana_data_source" "loki" {
  name = var.loki_datasource_name
}

data "grafana_data_source" "tempo" {
  name = var.tempo_datasource_name
}
