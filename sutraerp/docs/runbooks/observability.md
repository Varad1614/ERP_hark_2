# Runbook — observability and the audit chain (Batch 9A)

Contract: `docs/batch9-contracts.md` §3 (telemetry) and §4 (audit chain). Stack and dashboards:
`deploy/observability/` (README there). Every Prometheus alert of group `sutra` links here through
its `runbook_url` (`…/observability.md#<AlertName>`).

## Signals at a glance

| Signal | Where | Notes |
|--------|-------|-------|
| Traces | Tempo (OTLP from the API → collector) | Server span `"{METHOD} {route}"` with `http.route`, `http.response.status_code`, `sutra.request_id`; inbound `traceparent` is honoured. Internal spans `sutra.job`, `sutra.audit.seal`. |
| Logs | stdout JSON (always) + Loki via OTLP (when `OTEL_EXPORTER_OTLP_ENDPOINT` is set) | Every line inside a request carries `trace_id` / `span_id`; Loki keeps them as structured metadata: `{service_name="sutra-api"} \| trace_id="<id>"`. |
| Metrics | `GET :9464/metrics` (`SUTRA_METRICS_ADDR`, never the public port) | Names in §3.3; labels never carry ids, tenants or raw paths. |
| Dashboards | Grafana folder *SutraERP* | `sutra-api-red`, `sutra-database`, `sutra-outbox-jobs`, `sutra-audit-chain`, `sutra-workflow-search`. |

Configuration (`Config::hardening.telemetry` / `.audit_chain`): `OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` | `http/protobuf`), `OTEL_SERVICE_NAME`,
`OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER_ARG` (parent-based ratio), `SUTRA_METRICS`,
`SUTRA_METRICS_ADDR`, `SUTRA_GIT_SHA`, `SUTRA_AUDIT_CHAIN`, `SUTRA_AUDIT_SEAL_SECS`,
`SUTRA_AUDIT_SEAL_BATCH`, `SUTRA_AUDIT_ANCHOR_SECS`, `SUTRA_AUDIT_ANCHOR_KEY` (OpenBao
`kv/sutraerp/<env>/audit`), `SUTRA_AUDIT_ANCHOR_KEY_ID`. Production refuses to start with the
chain on and no anchor key.

First moves for any alert: open the linked dashboard, pick the time window of the alert, jump
from a slow / failing request to its trace (Tempo) and from the trace to its logs (Loki,
`trace_id`).

---

<a id="SutraApiHighErrorRate"></a>
## SutraApiHighErrorRate

**Meaning:** more than 2 % of API requests answered 5xx for 10 minutes (critical).

1. Dashboard *SutraERP · API (RED)* → *Errors by route*: one route or all of them?
2. All routes: check `/health/ready` of each pod (database, NATS, Valkey readiness), recent
   deploys (`sutra_build_info`, *Restarts*), and the *Database pool* dashboard (pool exhaustion
   surfaces as 5xx / 504).
3. One route: open a failing trace (Tempo, `{ span.http.route = "<route>" && status = error }`),
   read the `ERROR` log lines of that `trace_id` in Loki.
4. Roll back the release (`helm rollback sutraerp`) if the errors started with a deploy.

<a id="SutraApiLatencyP95"></a>
## SutraApiLatencyP95

**Meaning:** the 95th percentile of request latency stayed above 1 s for 10 minutes (warning;
the performance gate budget is 300 ms reads / 800 ms writes, `docs/batch9-contracts.md` §9).

1. *Slowest routes (p95)* table → the route; *Slow traces (> 1 s)* → a trace.
2. Long gaps without child spans usually mean waiting for a pool connection
   (`SutraDbPoolSaturated`) or a slow statement (PostgreSQL `pg_stat_statements`).
3. Scale out (`api.autoscaling`) only after ruling out a single slow query or lock.

<a id="SutraOutboxBacklog"></a>
## SutraOutboxBacklog

**Meaning:** more than 1000 outbox events wait to be relayed for 15 minutes (warning). Domain
writes still succeed; consumers (inbox, notifications, projections) fall behind.

1. *Outbox & jobs* dashboard: is `published` flat (relay stopped) or below the write rate?
2. Relay stopped: NATS readiness (`/health/ready` → `nats` check), logs `outbox publisher not
   ready` / `outbox relay pass failed`. Restore NATS; the relay drains by itself.
3. Many `failed`: rows retrying with backoff or dead-lettered — `SELECT event_type, count(*),
   max(last_error) FROM platform.outbox WHERE dead_lettered_at IS NOT NULL GROUP BY 1`.
   Dead-lettered rows are never purged; replay them by clearing `dead_lettered_at` after the fix.

<a id="SutraAuditUnsealed"></a>
## SutraAuditUnsealed

**Meaning:** audit rows have waited for a chain entry for 30 minutes (warning). Evidence is
still written; it is just not yet tamper-evident.

1. Is the sealer running? `sutra_job_runs_total{job="audit_seal"}` on *Audit chain* — no `ok`
   runs means `SUTRA_AUDIT_CHAIN=false` or no API replica with the scheduler.
2. `outcome="error"` runs: logs `sweeper run failed` with `job=audit_seal` (privileges after a
   restore? re-run `sutra-api migrate`, which re-grants the runtime role).
3. Backlog larger than `SUTRA_AUDIT_SEAL_BATCH` per `SUTRA_AUDIT_SEAL_SECS`: raise the batch
   (default 500 rows per tenant per tick).
4. `GET /api/v1/platform/audit/chain` (administrator) shows `unsealed` for the caller's tenant.

<a id="SutraAuditChainBroken"></a>
## SutraAuditChainBroken

**Meaning:** a verification found a broken chain (critical, **security incident**). See
[Audit chain break response](#audit-chain-break-response).

<a id="SutraJobFailing"></a>
## SutraJobFailing

**Meaning:** a scheduled job failed at least 3 times in 30 minutes (warning; label `job`).

1. Logs: `{service_name="sutra-api"} |= "sweeper run failed"` and the `job` field.
2. Statement timeouts (60 s per run) on purges: the backlog is too large — lower
   `SUTRA_SWEEP_BATCH_LIMIT` temporarily or run the purge in off-hours.
3. Permission errors after a restore: `sutra-api migrate` (re-grants), then watch the next run.

<a id="SutraDbPoolSaturated"></a>
## SutraDbPoolSaturated

**Meaning:** an API instance used more than 90 % of its PostgreSQL pool for 10 minutes (warning).

1. *Database pool* dashboard: one instance or all? All → load or slow queries; one → a stuck
   request holding connections (look for long traces on that `service.instance.id`).
2. Check PostgreSQL `max_connections` against `replicas × SUTRA_DB_MAX_CONNECTIONS` before
   raising the pool size.
3. Long-running transactions: `SELECT pid, now() - xact_start, query FROM pg_stat_activity
   WHERE state <> 'idle' ORDER BY 2 DESC`.

---

## Audit chain break response

The chain makes tampering **detectable**, not impossible. A break means a sealed audit row, a
chain entry or an anchor changed outside the application (every one of them is append-only by
trigger and by privilege; the runtime role cannot UPDATE / DELETE them).

1. **Freeze:** do not "repair" anything. Snapshot the database (`deploy/backup`, keep the dump
   immutable) and export `platform.audit_log`, `platform.audit_chain_entry`,
   `platform.audit_chain_head`, `platform.audit_anchor` for the tenant.
2. **Locate:** `POST /api/v1/platform/audit/chain/verify` (body `{}` or a `fromSeq` / `toSeq`
   range) as an administrator of the tenant returns `firstBrokenSeq` and `reason`:
   - `hash_mismatch` — the audit row sealed at that `seq` (or its entry hash) was edited;
     compare the row with the last backup that still verifies.
   - `link_broken` — an entry is missing (deleted) or its `prev_hash` / the head was rewritten.
   - `missing_row` — the audit row of an entry no longer exists.
   - `anchor_mismatch` — an anchor's MAC or head hash differs: either the anchor or the chain
     before it was rewritten, or `SUTRA_AUDIT_ANCHOR_KEY` / `_KEY_ID` is not the key that signed it.
3. **Scope:** the database role and time window that could write the tables (PostgreSQL logs,
   `pg_stat_activity`, OpenBao audit log for who read the owner credentials). The anchors bound
   the window: entries up to the last good anchor are proven unchanged.
4. **Report:** treat as a security incident (`docs/runbooks/security.md`), inform the
   institution's data-protection contact; the chain stays broken as evidence — new rows keep
   being sealed after the break and verify from the next `seq` (`fromSeq`).
5. **Key rotation:** set a new `SUTRA_AUDIT_ANCHOR_KEY_ID` + key in OpenBao; old anchors stay
   verifiable with the old key (verification checks the anchors of the configured key id only).

Hash and MAC bytes are frozen in `sutra_kernel::audit_chain` (`sutra.audit.v1`, test vector in
the contract), so an auditor can recompute a chain with an independent script.

## CI proof

`.github/workflows/sutraerp-observability.yml` (job *Observability (OTel → Prometheus · Loki ·
Tempo)*) runs the API against `deploy/observability/compose.yaml` and
`deploy/ci/check-telemetry.sh` (`SUTRA_CI_MODE=compose`); the kind job of
`sutraerp-deploy.yml` runs the same script with `SUTRA_CI_MODE=kind` against the chart's
`sutraerp-lgtm`. `promtool test rules deploy/observability/alerts/tests/sutra.rules.test.yaml`
covers every alert above; `apps/api/tests/db_b9a.rs` covers sealing, verification, tampering and
anchors against PostgreSQL 18.
