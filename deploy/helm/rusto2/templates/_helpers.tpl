{{/*
Expand the chart name.
*/}}
{{- define "rusto2.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a release-scoped name. Installing a release named "rusto2" yields the
bare name "rusto2", which keeps the Service names aligned with Compose:
rusto2-gw and rusto2-is.
*/}}
{{- define "rusto2.fullname" -}}
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

{{/*
Chart label value, safe for label values (build metadata uses "_").
*/}}
{{- define "rusto2.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels for every resource.
*/}}
{{- define "rusto2.labels" -}}
helm.sh/chart: {{ include "rusto2.chart" . }}
{{ include "rusto2.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: {{ include "rusto2.name" . }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end }}

{{/*
Selector labels shared by both workloads. Keep this set immutable.
*/}}
{{- define "rusto2.selectorLabels" -}}
app.kubernetes.io/name: {{ include "rusto2.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Selector labels for one component. Call with:
(dict "root" . "component" "gateway")
*/}}
{{- define "rusto2.componentSelectorLabels" -}}
{{ include "rusto2.selectorLabels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{/*
Labels, including the component marker. Call with:
(dict "root" . "component" "gateway")
*/}}
{{- define "rusto2.componentLabels" -}}
{{ include "rusto2.labels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{/*
Gateway and identity resource names.
*/}}
{{- define "rusto2.gateway.fullname" -}}
{{- printf "%s-gw" (include "rusto2.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "rusto2.identity.fullname" -}}
{{- printf "%s-is" (include "rusto2.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
ServiceAccount name.
*/}}
{{- define "rusto2.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "rusto2.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Resolve an image reference. Supports repository/tag/digest and falls back to
the chart appVersion when tag is empty. Call with:
(dict "root" . "image" .Values.gateway.image)
*/}}
{{- define "rusto2.image" -}}
{{- $image := .image -}}
{{- $repository := required "image.repository is required" $image.repository -}}
{{- if $image.digest -}}
{{- printf "%s@%s" $repository $image.digest -}}
{{- else -}}
{{- printf "%s:%s" $repository (default .root.Chart.AppVersion $image.tag) -}}
{{- end -}}
{{- end }}

{{/*
Public JWT issuer. Defaults to the in-cluster identity Service so a fresh
install can issue and validate tokens without DNS or Ingress setup.
*/}}
{{- define "rusto2.issuer" -}}
{{- if .Values.identity.issuer -}}
{{- .Values.identity.issuer | trimSuffix "/" -}}
{{- else -}}
{{- printf "http://%s:%v" (include "rusto2.identity.fullname" .) .Values.identity.service.port -}}
{{- end -}}
{{- end }}

{{/*
Internal JWKS URL used by the gateway. This always points at the identity
Service, even when the public issuer is an external hostname.
*/}}
{{- define "rusto2.identity.jwksURL" -}}
{{- printf "http://%s:%v/.well-known/jwks.json" (include "rusto2.identity.fullname" .) .Values.identity.service.port -}}
{{- end }}

{{/*
Client credentials Secret name: the existing Secret when configured, otherwise
the chart-managed "<fullname>-is-secrets".
*/}}
{{- define "rusto2.identity.secretName" -}}
{{- default (printf "%s-secrets" (include "rusto2.identity.fullname" .)) .Values.identity.existingSecret -}}
{{- end }}

{{/*
Signing-key PVC name: the existing claim when configured, otherwise the
chart-managed "<fullname>-is-data".
*/}}
{{- define "rusto2.identity.pvcName" -}}
{{- default (printf "%s-data" (include "rusto2.identity.fullname" .)) .Values.identity.persistence.existingClaim -}}
{{- end }}

{{/*
Render one base64-encoded Secret value. An explicit value wins; otherwise an
existing Secret value is preserved on upgrade and a random 32-character demo
secret is generated on first install. Call with:
(dict "ctx" . "values" .Values.identity.secrets "key" "demoClientSecret" "dataKey" "RUSTO2_DEMO_SECRET")
*/}}
{{- define "rusto2.identity.secretValue" -}}
{{- $value := "" -}}
{{- $configured := index .values .key -}}
{{- if $configured -}}
{{- $value = $configured | b64enc -}}
{{- else -}}
{{- $existing := lookup "v1" "Secret" .ctx.Release.Namespace (include "rusto2.identity.secretName" .ctx) -}}
{{- if $existing -}}
{{- $value = index ($existing.data | default dict) .dataKey -}}
{{- $value = default "" $value -}}
{{- end -}}
{{- if not $value -}}
{{- $value = randAlphaNum 32 | b64enc -}}
{{- end -}}
{{- end -}}
{{- $value -}}
{{- end }}
