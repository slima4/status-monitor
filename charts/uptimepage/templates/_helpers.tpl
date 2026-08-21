{{- define "uptimepage.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "uptimepage.fullname" -}}
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

{{- define "uptimepage.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "uptimepage.labels" -}}
helm.sh/chart: {{ include "uptimepage.chart" . }}
{{ include "uptimepage.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: uptimepage
{{- end }}

{{- define "uptimepage.selectorLabels" -}}
app.kubernetes.io/name: {{ include "uptimepage.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "uptimepage.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "uptimepage.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* Digest wins over tag. */}}
{{- define "uptimepage.image" -}}
{{- $repo := printf "%s/%s" .Values.image.registry .Values.image.repository }}
{{- if .Values.image.digest }}
{{- printf "%s@%s" $repo .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" $repo (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}

{{- define "uptimepage.secretName" -}}
{{- default (printf "%s-env" (include "uptimepage.fullname" .)) .Values.secrets.existingSecret }}
{{- end }}

{{- define "uptimepage.publicBaseUrl" -}}
{{- if .Values.publicBaseUrl }}
{{- .Values.publicBaseUrl | trimSuffix "/" }}
{{- else }}
{{- printf "https://%s" .Values.domain }}
{{- end }}
{{- end }}

{{- define "uptimepage.podSecurityContext" -}}
{{- $ctx := deepCopy .Values.podSecurityContext }}
{{- if .Values.ping.sysctl }}
{{- $_ := set $ctx "sysctls" (list (dict "name" "net.ipv4.ping_group_range" "value" (printf "%d 2147483647" (int .Values.podSecurityContext.runAsGroup)))) }}
{{- end }}
{{- toYaml $ctx }}
{{- end }}

{{- define "uptimepage.containerSecurityContext" -}}
{{- $ctx := deepCopy .Values.containerSecurityContext }}
{{- if .Values.ping.addNetRaw }}
{{- $_ := set $ctx "capabilities" (dict "drop" (list "ALL") "add" (list "NET_RAW")) }}
{{- end }}
{{- toYaml $ctx }}
{{- end }}

{{- define "uptimepage.commonEnv" -}}
- name: UPTIMEPAGE_OBSERVABILITY__LOG_FORMAT
  value: {{ .Values.observability.logFormat | quote }}
- name: UPTIMEPAGE_OBSERVABILITY__LOG_LEVEL
  value: {{ .Values.observability.logLevel | quote }}
- name: RUST_LOG
  value: {{ .Values.observability.rustLog | quote }}
- name: UPTIMEPAGE_SERVER__METRICS_BIND
  value: "0.0.0.0:{{ .Values.service.metricsPort }}"
{{- end }}

{{/* Everything here would otherwise fail at runtime instead of at render. */}}
{{- define "uptimepage.validate" -}}
{{- if not .Values.domain }}
{{- fail "uptimepage: .Values.domain is required" }}
{{- end }}
{{- if not (has .Values.mode (list "allInOne" "split")) }}
{{- fail (printf "uptimepage: .Values.mode must be allInOne or split, got %q" .Values.mode) }}
{{- end }}
{{- if gt (int .Values.app.replicaCount) 1 }}
{{- fail "uptimepage: app.replicaCount cannot exceed 1 - the control plane has no leader election, so a second pod duplicates alerts and notifications" }}
{{- end }}
{{- if and (eq .Values.mode "split") (not .Values.probes) }}
{{- fail "uptimepage: mode=split needs at least one entry in .Values.probes, otherwise nothing probes anything" }}
{{- end }}
{{- if and (eq .Values.mode "allInOne") .Values.probes }}
{{- fail "uptimepage: .Values.probes is set but mode=allInOne - the control plane and the agents would both probe the same region" }}
{{- end }}
{{- if and (not .Values.postgresql.url) (not .Values.postgresql.existingSecret) }}
{{- fail "uptimepage: set postgresql.url or postgresql.existingSecret" }}
{{- end }}
{{- if not .Values.clickhouse.url }}
{{- fail "uptimepage: clickhouse.url is required" }}
{{- end }}
{{- /* The app refuses to boot on the published credentials. Catching it here
       turns a crash loop into a message you can act on. */}}
{{- if and .Values.postgresql.url (regexMatch "^postgres(ql)?://monitor:monitor@" .Values.postgresql.url) }}
{{- fail "uptimepage: postgresql.url carries the published monitor:monitor credentials, which the app refuses to boot on. Use a real password." }}
{{- end }}
{{- if not .Values.clickhouse.existingSecret }}
{{- if or (not .Values.clickhouse.password) (eq .Values.clickhouse.password "monitor") }}
{{- fail "uptimepage: clickhouse.password must be set and must not be the published value - the app rejects an empty or default ClickHouse password at boot" }}
{{- end }}
{{- end }}
{{- if and (not .Values.secrets.existingSecret) (not .Values.secrets.fingerprintSalt) }}
{{- fail "uptimepage: secrets.fingerprintSalt is required - the app refuses to boot with an empty salt. Generate one with: openssl rand -base64 32" }}
{{- end }}
{{- /* The KEK must decode to exactly 32 bytes; anything else crash-loops on the
       first boot rather than failing here. */}}
{{- if .Values.secrets.credentialsKekBase64 }}
{{- $kek := len (b64dec .Values.secrets.credentialsKekBase64) }}
{{- if ne $kek 32 }}
{{- fail (printf "uptimepage: secrets.credentialsKekBase64 must decode to 32 bytes, got %d. Generate one with: openssl rand -base64 32" $kek) }}
{{- end }}
{{- end }}
{{- if .Values.secrets.existingSecret }}
{{- if or .Values.auth.github.clientSecret .Values.auth.google.clientSecret .Values.auth.microsoft.clientSecret .Values.email.resend.apiKey }}
{{- fail "uptimepage: secrets.existingSecret owns every core key - put github-client-secret / google-client-secret / microsoft-client-secret / resend-api-key in that Secret rather than in values" }}
{{- end }}
{{- else }}
{{- if and .Values.auth.github.clientId (not .Values.auth.github.clientSecret) }}
{{- fail "uptimepage: auth.github.clientId is set without auth.github.clientSecret" }}
{{- end }}
{{- if and .Values.auth.google.clientId (not .Values.auth.google.clientSecret) }}
{{- fail "uptimepage: auth.google.clientId is set without auth.google.clientSecret" }}
{{- end }}
{{- if and .Values.auth.microsoft.clientId (not .Values.auth.microsoft.clientSecret) }}
{{- fail "uptimepage: auth.microsoft.clientId is set without auth.microsoft.clientSecret" }}
{{- end }}
{{- if and (eq .Values.email.provider "resend") (not .Values.email.resend.apiKey) }}
{{- fail "uptimepage: email.provider=resend needs email.resend.apiKey" }}
{{- end }}
{{- end }}
{{- if and (eq .Values.email.provider "resend") (not .Values.email.fromAddress) }}
{{- fail "uptimepage: email.provider=resend needs email.fromAddress" }}
{{- end }}
{{- if and .Values.tenancy.subdomainPublicRoutes .Values.ingress.enabled (not .Values.ingress.wildcard.enabled) }}
{{- fail "uptimepage: tenancy.subdomainPublicRoutes needs ingress.wildcard.enabled, or per-org status pages have no route" }}
{{- end }}
{{- if and .Values.mcp.oauthEnabled (not (hasPrefix "https://" (include "uptimepage.publicBaseUrl" .))) }}
{{- fail "uptimepage: mcp.oauthEnabled requires an https publicBaseUrl - the app refuses to boot otherwise" }}
{{- end }}
{{- range $i, $p := .Values.probes }}
{{- if not $p.name }}{{ fail (printf "uptimepage: probes[%d].name is required" $i) }}{{ end }}
{{- if not $p.region }}{{ fail (printf "uptimepage: probes[%d].region is required" $i) }}{{ end }}
{{- if and (not $p.token) (not $p.existingSecret) }}
{{- fail (printf "uptimepage: probes[%d] needs token or existingSecret - mint one on the control plane first" $i) }}
{{- end }}
{{- if gt (int (default 1 $p.replicaCount)) 1 }}
{{- fail (printf "uptimepage: probes[%d].replicaCount cannot exceed 1 - two agents in one region run every check twice" $i) }}
{{- end }}
{{- end }}
{{- end }}
