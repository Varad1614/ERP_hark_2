# Observability (slice 9A)

Contract: `docs/batch9-contracts.md` §3 (telemetry) and §4 (audit chain). Runbook:
`docs/runbooks/observability.md`.

| Path | What |
|------|------|
| `compose.yaml` | CI stack: PostgreSQL 18 + `grafana/otel-lgtm` (collector, Prometheus, Loki, Tempo, Grafana). The API runs on the host and exports OTLP to `localhost:4317`; Prometheus scrapes `host.docker.internal:9464/metrics`. |
| `lgtm/prometheus.yaml` | Prometheus config mounted into the lgtm container (scrape + alert rules). |
| `otel-collector.yaml` | Production collector: OTLP in → Tempo (traces) + Loki (logs), scrubbing + tail sampling. |
| `helm/*.values.yaml` | Values for the upstream kube-prometheus-stack, Loki, Tempo, Grafana and collector charts. |
| `dashboards/sutra-*.json` | Grafana dashboards `sutra-api-red`, `sutra-database`, `sutra-outbox-jobs`, `sutra-audit-chain`, `sutra-workflow-search` (datasource variables `DS_PROMETHEUS` / `DS_LOKI` / `DS_TEMPO`). |
| `alerts/sutra.rules.yaml` | Prometheus rules, group `sutra` (7 alerts, `severity` + `runbook_url`). |
| `alerts/tests/*.yaml` | `promtool test rules` fixtures (one firing + one quiet case per alert). |
| `gen.py` | Generates the rules, their tests and the dashboards — edit there, then `python3 gen.py`. |

Local run (needs Docker): `docker compose -f deploy/observability/compose.yaml up -d --wait`, then
`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 cargo run -p sutra-api -- serve` and
`SUTRA_CI_MODE=compose bash deploy/ci/check-telemetry.sh`. Grafana: http://localhost:3000.
