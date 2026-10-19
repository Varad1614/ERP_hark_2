{{- /*
9C: helpers for templates/data/** (Flowable, OpenSearch, backup). Self-contained so the data
templates render on the foundation chart; names follow docs/batch9-contracts.md §10.1
(<fullname>-<component>, label app.kubernetes.io/component).
*/ -}}

{{- define "sutraerp.data.name" -}}
{{- printf "%s-%s" (include "sutraerp.fullname" .root) .component | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "sutraerp.data.labels" -}}
{{ include "sutraerp.labels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- /* ServiceAccount of the data workloads: the contract's `<fullname>-backup` (bound to the
OpenBao kubernetes-auth role `sutraerp`, §5; created by 9B's core/serviceaccounts.yaml). */ -}}
{{- define "sutraerp.data.serviceAccount" -}}
{{- default (include "sutraerp.data.name" (dict "root" . "component" "backup")) .Values.backup.serviceAccount -}}
{{- end -}}

{{- /* PostgreSQL host / port / admin user (bundled StatefulSet of 9B or the external server). */ -}}
{{- define "sutraerp.data.pgHost" -}}
{{- if eq .Values.postgres.mode "bundled" -}}
{{- include "sutraerp.data.name" (dict "root" . "component" "postgres") -}}
{{- else -}}
{{- required "postgres.external.host is required with postgres.mode=external" .Values.postgres.external.host -}}
{{- end -}}
{{- end -}}

{{- define "sutraerp.data.pgPort" -}}
{{- default 5432 .Values.postgres.external.port -}}
{{- end -}}

{{- define "sutraerp.data.pgAdminUser" -}}
{{- $b := default dict .Values.postgres.bundled -}}
{{- default "sutra" (default $b.user .Values.flowable.initDb.adminUser) -}}
{{- end -}}

{{- define "sutraerp.data.openbao" -}}
{{- if eq .Values.secrets.mode "openbao" -}}true{{- end -}}
{{- end -}}

{{- /* OpenBao agent annotations rendering `.file` from `.template` (init container only).
Usage: include "sutraerp.data.openbaoAnnotations" (dict "root" $ "file" "data.env" "group" "flowable" "template" $tpl) */ -}}
{{- define "sutraerp.data.openbaoAnnotations" -}}
{{- $o := .root.Values.secrets.openbao -}}
{{- $p := default "vault.hashicorp.com" $o.annotationPrefix -}}
{{ $p }}/agent-inject: "true"
{{ $p }}/role: {{ $o.role | quote }}
{{ $p }}/auth-path: {{ $o.authPath | quote }}
{{- if $o.address }}
{{ $p }}/service: {{ $o.address | quote }}
{{- end }}
{{ $p }}/agent-pre-populate-only: "true"
{{ $p }}/agent-inject-secret-{{ .file }}: {{ include "sutraerp.openbaoPath" (dict "root" .root "name" .group) | quote }}
{{ $p }}/agent-inject-template-{{ .file }}: {{ .template | quote }}
{{- end -}}

{{- /* Agent template: every key of the given OpenBao groups as KEY='value' lines (dotenv).
Usage: include "sutraerp.data.dotenvTemplate" (dict "root" $ "groups" (list "database" "flowable")) */ -}}
{{- define "sutraerp.data.dotenvTemplate" -}}
{{- $root := .root -}}
{{- range $g := .groups -}}
{{- $path := include "sutraerp.openbaoPath" (dict "root" $root "name" $g) -}}
{{ printf "{{ with secret %q }}{{ range $k, $v := .Data.data }}{{ $k }}='{{ $v }}'\n{{ end }}{{ end }}\n" $path }}
{{- end -}}
{{- end -}}

{{- /* Shell prologue that loads the rendered dotenv when present. */ -}}
{{- define "sutraerp.data.loadEnv" -}}
if [ -f /vault/secrets/data.env ]; then set -a; . /vault/secrets/data.env; set +a; fi
{{- end -}}

{{- define "sutraerp.data.podSecurity" -}}
runAsNonRoot: true
runAsUser: {{ .uid }}
runAsGroup: {{ .uid }}
fsGroup: {{ .uid }}
seccompProfile: { type: RuntimeDefault }
{{- end -}}

{{- define "sutraerp.data.containerSecurity" -}}
allowPrivilegeEscalation: false
capabilities: { drop: [ALL] }
{{- end -}}
