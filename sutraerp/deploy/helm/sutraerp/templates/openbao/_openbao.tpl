{{- /*
9B: OpenBao Agent Injector helpers (docs/batch9-contracts.md §5).

The OpenBao chart (openbao/openbao 0.30.x) ships the upstream `hashicorp/vault-k8s` injector with the
OpenBao agent image, so the pod annotations keep the `vault.hashicorp.com/*` prefix
(`secrets.openbao.annotationPrefix`). The agent runs as an init container only
(`agent-pre-populate-only`): KV v2 values are static, and a long-running sidecar would keep the
migrate Job from completing.
*/ -}}

{{- /* Agent template that renders every key of the configured groups into one dotenv file.
Usage: include "sutraerp.openbaoDotenv" (dict "root" $ "exclude" (list "KEY" …)) */ -}}
{{- define "sutraerp.openbaoDotenv" -}}
{{- $root := .root -}}
{{- $cond := "" -}}
{{- if .exclude -}}
{{- $parts := list -}}
{{- range .exclude }}{{ $parts = append $parts (printf "(eq $k %q)" .) }}{{ end -}}
{{- $cond = printf "(or %s false)" (join " " $parts) -}}
{{- end -}}
{{- range $g := $root.Values.secrets.openbao.groups -}}
{{- $path := include "sutraerp.openbaoPath" (dict "root" $root "name" $g) -}}
{{- if $cond -}}
{{ printf "{{ with secret %q }}{{ range $k, $v := .Data.data }}{{ if not %s }}{{ $k }}='{{ $v }}'\n{{ end }}{{ end }}{{ end }}\n" $path $cond }}
{{- else -}}
{{ printf "{{ with secret %q }}{{ range $k, $v := .Data.data }}{{ $k }}='{{ $v }}'\n{{ end }}{{ end }}\n" $path }}
{{- end -}}
{{- end -}}
{{- end -}}

{{- /* Pod annotations. Usage: include "sutraerp.openbaoAnnotations" (dict "root" $ "file" "sutra.env" "template" $tpl) */ -}}
{{- define "sutraerp.openbaoAnnotations" -}}
{{- $o := .root.Values.secrets.openbao -}}
{{- $p := $o.annotationPrefix -}}
{{ $p }}/agent-inject: "true"
{{ $p }}/role: {{ $o.role | quote }}
{{ $p }}/auth-path: {{ $o.authPath | quote }}
{{- if $o.address }}
{{ $p }}/service: {{ $o.address | quote }}
{{- end }}
{{ $p }}/agent-pre-populate-only: {{ $o.prePopulateOnly | toString | quote }}
{{ $p }}/agent-requests-cpu: {{ $o.agent.requestsCpu | quote }}
{{ $p }}/agent-requests-mem: {{ $o.agent.requestsMem | quote }}
{{ $p }}/agent-limits-mem: {{ $o.agent.limitsMem | quote }}
{{ $p }}/agent-limits-cpu: {{ $o.agent.limitsCpu | quote }}
{{ $p }}/agent-inject-secret-{{ .file }}: {{ include "sutraerp.openbaoPath" (dict "root" .root "name" (first $o.groups)) | quote }}
{{ $p }}/agent-inject-template-{{ .file }}: {{ .template | quote }}
{{- end -}}

{{- /* True when pods read their credentials from OpenBao. */ -}}
{{- define "sutraerp.openbaoEnabled" -}}
{{- if eq .Values.secrets.mode "openbao" -}}true{{- end -}}
{{- end -}}
