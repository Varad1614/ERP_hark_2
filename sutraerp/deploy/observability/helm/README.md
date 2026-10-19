# Upstream observability charts (production)

SutraERP does not vendor Prometheus, Loki, Tempo or Grafana; production clusters install the
upstream charts into namespace `observability` with these values (pin the chart versions in your
GitOps repo):

| Chart | Repo | Values |
|-------|------|--------|
| `prometheus-community/kube-prometheus-stack` | https://prometheus-community.github.io/helm-charts | `prometheus.values.yaml` (Prometheus Operator → the SutraERP chart's `ServiceMonitor` + `PrometheusRule`) |
| `grafana/loki` | https://grafana.github.io/helm-charts | `loki.values.yaml` (single binary, OTLP ingestion, 30 d retention) |
| `grafana/tempo` | https://grafana.github.io/helm-charts | `tempo.values.yaml` (OTLP gRPC receiver, 14 d retention) |
| `grafana/grafana` | https://grafana.github.io/helm-charts | `grafana.values.yaml` (datasources with the uids the dashboards expect, dashboard sidecar for `grafana_dashboard=1` ConfigMaps) |
| `open-telemetry/opentelemetry-collector` | https://open-telemetry.github.io/opentelemetry-helm-charts | `otel-collector.values.yaml` (embeds `../otel-collector.yaml`) |

Then enable in the SutraERP release: `observability.serviceMonitor.enabled`,
`observability.prometheusRule.enabled`, `observability.grafanaDashboards.enabled` and point
`observability.otlpEndpoint` at `http://otel-collector.observability.svc:4317`.

CI does not install these charts (no real cluster): the kind job uses the all-in-one
`grafana/otel-lgtm` (`observability.lgtm.enabled=true`) and the compose job uses
`deploy/observability/compose.yaml`; `helm template` renders the SutraERP side of both.
