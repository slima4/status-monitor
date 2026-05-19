# Grafana provider pointed at the Grafana Cloud *stack* (instance-level
# resources: alert rules, contact points, notification policy,
# dashboard). Auth is a Grafana service-account token — least
# privilege is the **Editor** role plus alerting + datasource write;
# Admin is NOT required for anything this config manages. The token in
# TFC state = stack-level write, so keep the TFC workspace locked down.
#
# Both values are set as TFC workspace variables — never committed.
# grafana_auth is sensitive; grafana_url is the stack base
# (https://<stack>.grafana.net).
provider "grafana" {
  url  = var.grafana_url
  auth = var.grafana_auth
}
