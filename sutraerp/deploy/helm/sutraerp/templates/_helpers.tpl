{{/* Batch 9 chart helpers (foundation). docs/batch9-contracts.md §3. */}}
{{- define "sutraerp.name" -}}sutraerp{{- end -}}

{{- define "sutraerp.fullname" -}}
{{- if contains "sutraerp" .Release.Name -}}{{ .Release.Name | trunc 63 | trimSuffix "-" }}{{- else -}}{{ printf "%s-sutraerp" .Release.Name | trunc 63 | trimSuffix "-" }}{{- end -}}
{{- end -}}

{{- define "sutraerp.labels" -}}
app.kubernetes.io/name: {{ include "sutraerp.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version }}
{{- end -}}

{{- define "sutraerp.selectorLabels" -}}
app.kubernetes.io/name: {{ include "sutraerp.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/* Image reference: registry/repository@digest, else :tag, else :appVersion. Usage: include "sutraerp.image" (dict "root" . "img" .Values.image.api) */}}
{{- define "sutraerp.image" -}}
{{- $reg := .root.Values.global.imageRegistry -}}
{{- $ref := printf "%s/%s" $reg .img.repository -}}
{{- if .img.digest -}}{{ printf "%s@%s" $ref .img.digest }}{{- else -}}{{ printf "%s:%s" $ref (default .root.Chart.AppVersion .img.tag) }}{{- end -}}
{{- end -}}

{{/* OpenBao KV v2 data path of a secret group, e.g. include "sutraerp.openbaoPath" (dict "root" . "name" "app") */}}
{{- define "sutraerp.openbaoPath" -}}
{{- printf "%s/data/%s/%s/%s" .root.Values.secrets.openbao.kvMount .root.Values.secrets.openbao.pathPrefix .root.Values.global.environment .name -}}
{{- end -}}
