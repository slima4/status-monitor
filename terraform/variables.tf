# All values are set as HCP Terraform workspace variables (TFC UI →
# workspace → Variables). No default here is a secret; nothing is
# committed.

variable "grafana_url" {
  type        = string
  description = "Grafana Cloud stack base URL, e.g. https://<stack>.grafana.net"
}

variable "grafana_auth" {
  type        = string
  description = "Grafana service-account token for the stack (Editor + alerting/datasource write; see providers.tf)"
  sensitive   = true
}

# Grafana Cloud auto-provisions these datasources; names follow
# grafanacloud-<stack-slug>-{prom,logs,traces}. Defaults match this
# stack (slug 'slima4'); override per workspace if the slug differs.
variable "prometheus_datasource_name" {
  type        = string
  description = "Name of the Grafana Cloud Prometheus datasource"
  default     = "grafanacloud-slima4-prom"
}

variable "loki_datasource_name" {
  type        = string
  description = "Name of the Grafana Cloud Loki datasource (carries the trace_id derived field)"
  default     = "grafanacloud-slima4-logs"
}

variable "tempo_datasource_name" {
  type        = string
  description = "Name of the Grafana Cloud Tempo datasource (derived-field link target)"
  default     = "grafanacloud-slima4-traces"
}

variable "alert_email" {
  type        = string
  description = "Destination address for the default contact point"
}
