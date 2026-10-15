{{- /* 9B helpers (the foundation _helpers.tpl stays untouched). */ -}}

{{- /* Full labels + component. Usage: include "sutraerp.componentLabels" (dict "root" $ "component" "api") */ -}}
{{- define "sutraerp.componentLabels" -}}
{{ include "sutraerp.labels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- /* Name of a component object: <fullname>-<component>. */ -}}
{{- define "sutraerp.componentName" -}}
{{- printf "%s-%s" (include "sutraerp.fullname" .root) .component | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- /* Migrate as a Helm hook? auto = hook with external PostgreSQL, plain revision Job with the
bundled StatefulSet (a pre-install hook would run before the bundled database exists). */ -}}
{{- define "sutraerp.migrateHook" -}}
{{- $h := toString .Values.migrations.hook -}}
{{- if or (eq $h "true") (and (eq $h "auto") (ne .Values.postgres.mode "bundled")) -}}true{{- end -}}
{{- end -}}

{{- define "sutraerp.migrateJobName" -}}
{{- if include "sutraerp.migrateHook" . -}}
{{- include "sutraerp.componentName" (dict "root" . "component" "migrate") -}}
{{- else -}}
{{- printf "%s-r%d" (include "sutraerp.componentName" (dict "root" . "component" "migrate")) (int .Release.Revision) -}}
{{- end -}}
{{- end -}}

{{- /* Pod-level image pull secrets. */ -}}
{{- define "sutraerp.imagePullSecrets" -}}
{{- with .Values.global.imagePullSecrets }}
imagePullSecrets:
{{- range . }}
  - name: {{ . }}
{{- end }}
{{- end }}
{{- end -}}

{{- /* NetworkPolicy building blocks (templates/core/networkpolicy.yaml). */ -}}
{{- define "sutraerp.np.peer" -}}
- podSelector:
    matchLabels:
      app.kubernetes.io/instance: {{ .inst }}
      app.kubernetes.io/component: {{ .component }}
{{- end -}}
{{- define "sutraerp.np.meta" -}}
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {{ include "sutraerp.fullname" .root }}-{{ .name }}
  labels:
    {{- include "sutraerp.labels" .root | nindent 4 }}
{{- end -}}
{{- define "sutraerp.np.select" -}}
podSelector:
  matchLabels:
    app.kubernetes.io/instance: {{ .inst }}
    app.kubernetes.io/component: {{ .component }}
{{- end -}}
