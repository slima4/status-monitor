{{- define "uptimepage-agent.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "uptimepage-agent.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "uptimepage-agent.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "uptimepage-agent.labels" -}}
helm.sh/chart: {{ include "uptimepage-agent.chart" . }}
{{ include "uptimepage-agent.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/component: probe
app.kubernetes.io/part-of: uptimepage
uptimepage.dev/region: {{ .Values.region | quote }}
{{- end }}

{{- define "uptimepage-agent.selectorLabels" -}}
app.kubernetes.io/name: {{ include "uptimepage-agent.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "uptimepage-agent.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "uptimepage-agent.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "uptimepage-agent.image" -}}
{{- $repo := printf "%s/%s" .Values.image.registry .Values.image.repository }}
{{- if .Values.image.digest }}
{{- printf "%s@%s" $repo .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" $repo (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}

{{- define "uptimepage-agent.podSecurityContext" -}}
{{- $ctx := deepCopy .Values.podSecurityContext }}
{{- if .Values.ping.sysctl }}
{{- $_ := set $ctx "sysctls" (list (dict "name" "net.ipv4.ping_group_range" "value" (printf "%d 2147483647" (int .Values.podSecurityContext.runAsGroup)))) }}
{{- end }}
{{- toYaml $ctx }}
{{- end }}

{{- define "uptimepage-agent.containerSecurityContext" -}}
{{- $ctx := deepCopy .Values.containerSecurityContext }}
{{- if .Values.ping.addNetRaw }}
{{- $_ := set $ctx "capabilities" (dict "drop" (list "ALL") "add" (list "NET_RAW")) }}
{{- end }}
{{- toYaml $ctx }}
{{- end }}

{{- define "uptimepage-agent.validate" -}}
{{- if not .Values.controlPlaneUrl }}
{{- fail "uptimepage-agent: controlPlaneUrl is required" }}
{{- end }}
{{- if not .Values.region }}
{{- fail "uptimepage-agent: region is required, and it must already exist on the control plane" }}
{{- end }}
{{- if and (not .Values.token) (not .Values.existingSecret) }}
{{- fail "uptimepage-agent: set token or existingSecret - mint the token on the control plane first" }}
{{- end }}
{{- if gt (int .Values.replicaCount) 1 }}
{{- fail "uptimepage-agent: replicaCount cannot exceed 1 - two agents in one region run every check twice" }}
{{- end }}
{{- end }}
