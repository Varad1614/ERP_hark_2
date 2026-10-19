# kind (slice 9B)

`cluster.yaml` — one control-plane node (kind v0.31.0, `kindest/node:v1.31.14`, pinned by digest in
`.github/workflows/sutraerp-deploy.yml`) used by the `Kind (install · migrate · smoke · telemetry · DR)`
job through `up.sh`. The default CNI (kindnet) enforces NetworkPolicy, so the chart's default-deny
policies are real. The stages reach the Services through `kubectl port-forward`:

| Service | Local | Stage env |
|---------|-------|-----------|
| `sutraerp-api:8080` | `localhost:8080` | `SUTRA_CI_API_URL` |
| `sutraerp-web:8080` | `localhost:8081` | `SUTRA_CI_WEB_URL` |
| `sutraerp-lgtm` (9A, when present) | 3000 Grafana, 9090 Prometheus, 3100 Loki, 3200 Tempo, 4318 OTLP/HTTP | `SUTRA_CI_GRAFANA_URL`, `SUTRA_CI_PROMETHEUS_URL`, `SUTRA_CI_LOKI_URL`, `SUTRA_CI_TEMPO_URL` |

Also exported to every stage: `SUTRA_CI_MODE=kind`, `SUTRA_CI_NS=sutraerp`, `KUBECONFIG`.

Run the same thing on a workstation (docker, kind, kubectl, helm):

```bash
cd sutraerp
docker build -f deploy/docker/api.Dockerfile -t ghcr.io/varad1614/sutraerp-api:ci .
docker build -f deploy/docker/web.Dockerfile -t ghcr.io/varad1614/sutraerp-web:ci .
bash deploy/kind/up.sh          # cluster + OpenBao + seed + helm install/upgrade, then the four stages
```
