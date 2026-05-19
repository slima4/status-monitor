# Loki → Tempo pivot. The app logs one INFO "http access" line per
# request carrying "trace_id":"<32 hex>" (access_log middleware, commit
# 436e439). This derived field turns that token into a click-through to
# the matching Tempo trace.
#
# grafana_data_source_config manages ONLY jsonData/secureJsonData of an
# already-existing datasource — it does NOT own the datasource's
# url/access/basic-auth/connection. Grafana Cloud auto-provisions and
# owns the Loki connection; this only layers a jsonData key onto it, so
# the safety invariant holds: no apply here can break the logs
# pipeline.
#
# Idempotency: json_data_encoded is a full replace of jsonData, so we
# read Loki's CURRENT jsonData back via the data source and merge() the
# derived field in — never a hand-replicated blob. After apply Loki's
# jsonData = {whatever Cloud set..., derivedFields:[ours]}; the next
# plan reads exactly that, merge() overlays the identical derivedFields,
# the encoded JSON is unchanged → zero drift. try()/{} guards the case
# where Loki has no jsonData yet (Cloud Loki ds is typically minimal).
#
# Caveat: the grafana_data_source DATA source strips httpHeaderName*
# from json_data_encoded on read. If the Loki ds ever auth'd via
# custom HTTP headers, the write-back would drop those names and break
# auth. Grafana Cloud Loki uses basic-auth (no jsonData headers), so
# this is safe here — README has a one-time precheck before first
# apply that verifies it.
locals {
  loki_json_data = try(jsondecode(data.grafana_data_source.loki.json_data_encoded), {})
}

resource "grafana_data_source_config" "loki" {
  uid = data.grafana_data_source.loki.uid

  json_data_encoded = jsonencode(merge(local.loki_json_data, {
    derivedFields = [
      {
        name        = "trace_id"
        matcherType = "regex"
        # Matches the access-log JSON field. The doubled backslashes
        # survive HCL → JSON encoding to the literal regex
        # "trace_id":"([0-9a-f]{32})".
        matcherRegex = "\"trace_id\":\"([0-9a-f]{32})\""
        # $${__value.raw} → Grafana's ${__value.raw} (the captured id);
        # with datasourceUid set, Grafana makes this an internal link
        # that runs a trace lookup against Tempo.
        url             = "$${__value.raw}"
        datasourceUid   = data.grafana_data_source.tempo.uid
        urlDisplayLabel = "View trace in Tempo"
      }
    ]
  }))
}
