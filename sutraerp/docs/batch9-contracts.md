# Batch 9 — shared contracts (Production hardening + Domain G00 / H09 carry-overs)

**Status:** frozen at `batch9/foundation`. This file is the single source of truth for the five
slices **9A**, **9B**, **9C**, **9D** and **9E**. If a slice needs to change anything in §2–§13, it
says so in its report ("contract change: …") instead of silently editing a shared file; the
integrator applies it on top of the merge.

Plan line (Batch 9): *OpenTelemetry exporter plus Prometheus, Loki, Tempo and Grafana; tamper-evident
audit chaining. Kubernetes manifests and Helm, OpenBao secrets, GHCR push and Cosign signing.
Flowable production deployment, OpenSearch projections, performance and capacity gates, backup and
DR drills, security review. Pilot go-live with one institution.*

**User decisions (2026-10-14):** no real cluster or cloud — everything is **proved in GitHub
Actions** (throwaway kind cluster, containers on the runner). GHCR `ghcr.io/varad1614/*` push +
Cosign keyless signing **only on `erp_merge` and `v*` tags**; build-only elsewhere. Pilot go-live =
runbook + checklist + seeded pilot tenant script.

Architecture inputs: ADR-L5-0012 (audit evidence, chaining / anchoring), -0023 / -0042 (Flowable
isolated JVM runtime behind `WorkflowPort`), -0029 (observability), -0030 (image supply chain),
-0005 / -0036 (runtime DB roles), -0004 (scope); L2/L3/L4 section-g G00 (G-001…015) and section-h
H09 (H-061…071).

**Lessons from Batches 3–8 applied:** every cross-slice dependency is fixed up front — env names
(`apps/api/src/config_b9.rs`, already parsed), the audit hash bytes (`sutra_kernel::audit_chain`,
with a test vector), Helm values layout (`deploy/helm/sutraerp/values.yaml`), one workflow file
and one CI stage script per slice, pre-mounted routers — so no slice waits for another. **CI-only**:
no local cargo / pnpm build / docker / chromium (§13).

---

## 1. Slices and scope (one wave: 9A … 9E in parallel)

| Slice | Scope |
|-------|-------|
| **9F** | this contract, `config_b9.rs`, `sutra_kernel::audit_chain` (hash contract), migration `20261120000000_platform_audit_chain.sql`, permissions §6.1, mount points, chart skeleton, stub workflows + stage scripts, `SutraERP lockfile` workflow |
| **9A** | OTLP trace + log export, Prometheus `/metrics` listener, metric / span / log conventions §3, Grafana dashboards, Prometheus alert rules (+ `promtool test`), Loki / Tempo wiring, audit sealer + verify API + anchors §4, `db_b9a` |
| **9B** | Helm chart (api, web, migrate hook Job, bundled postgres / nats / valkey, NetworkPolicy, PDB, HPA, ingress, OpenBao injector annotations), OpenBao dev install + policy + seed §5, GHCR push + Cosign keyless + SBOM attestation + verify §10.2, kind job running every stage §10.4, compose `--profile app` boot test (closes "Docker artefacts never run") |
| **9C** | Flowable deployment + REST `WorkflowPort` adapter + BPMN for every approval key + decision sync §7, OpenSearch client + projector + 3 projections + search API §8, backup / restore scripts + CronJob + DR drill §11 |
| **9D** | Performance + capacity gates §9 (k6), security review: cargo-deny + cargo-audit, `pnpm audit`, gitleaks, Trivy + Grype, ZAP baseline, `docs/security/threat-model.md` + `findings.md`, fixes (small marked patches) |
| **9E** | G00 campus governance (G-001…015) + H09 accreditation (H-061…071) §12, home KPIs from real APIs, person pickers instead of raw record ids, approval-state normalisation §6.4, pilot seed script + runbook + go-live checklist |

**Deferred (recorded in `docs/batches.md` by the integrator):** real cloud / DNS / TLS / IdP
federation for the pilot (operator steps in the runbook only); PG streaming replication, WAL-archive
PITR, multi-region; OpenSearch projections beyond students / staff / library catalogue and a global
search UI; Flowable as the default approval engine (adapter is selectable, **no-op stays default**
until the user approves after the pilot); every Batch 8 deferral (H01, H03…H06, H10, G06…G09,
J09 / J10, F-046…050, F-067…075, F-077…080, F-083); OLED theme and customisable home widgets unless
9E has time.

## 2. Where the code lives: file ownership

### 2.1 Ownership map (create / edit freely inside your own paths)

| Slice | Paths |
|-------|-------|
| 9A | `apps/api/src/telemetry.rs`, new `apps/api/src/metrics.rs`, `crates/persistence/src/audit_chain.rs`, `modules/platform/src/audit/**`, `deploy/observability/**`, `deploy/helm/sutraerp/templates/observability/**`, `deploy/ci/check-telemetry.sh`, `.github/workflows/sutraerp-observability.yml`, migrations `20261120000010`–`019` (`_platform_`), `apps/api/tests/db_b9a.rs`, `docs/runbooks/observability.md` |
| 9B | `deploy/helm/sutraerp/**` except `templates/{observability,data}/**`, `deploy/{kind,openbao}/**`, `deploy/docker/**`, `deploy/ci/smoke.sh`, `.github/workflows/sutraerp-deploy.yml`, the `images` job of `.github/workflows/sutraerp-ci.yml` (rename to `Container images (build · push · sign)`), a new `compose-boot` job in `sutraerp-deploy.yml`, `docs/runbooks/deploy.md` |
| 9C | `modules/platform/src/{workflow_flowable.rs,search/**}`, `crates/infra/src/opensearch.rs`, `crates/contracts/src/platform/search.rs` (new), new files `modules/{student,people,campus}/src/search_source.rs` (+ one `pub mod search_source; // 9C` line each), `deploy/{flowable,opensearch,backup}/**`, `deploy/helm/sutraerp/templates/data/**`, `deploy/ci/dr-drill.sh`, `.github/workflows/sutraerp-data.yml`, migrations `…020`–`029` (`_platform_`), `apps/api/tests/{db_b9c.rs,live_flowable.rs,live_opensearch.rs}`, `docs/runbooks/backup-dr.md` |
| 9D | `deploy/perf/**`, `deny.toml`, `.gitleaks.toml`, `.zap/**`, `.trivyignore`, `docs/security/**`, `.github/workflows/sutraerp-quality.yml`, `docs/runbooks/security.md`; fixes elsewhere only as small patches marked `// 9D: security` and listed in the report |
| 9E | `modules/campus/src/governance/**`, `modules/services/src/accreditation/**`, `crates/contracts/src/campus/events/governance.rs`, `crates/contracts/src/services/events/accreditation.rs` (new, add `pub mod` with `// 9E`), inbox rules `modules/platform/src/inbox/rules/{campus_9e,services_9e}.rs`, migrations `…030`–`049`, `apps/api/tests/db_b9e.rs`, web `apps/web/app/features/campus/governance/**`, `features/reports/accreditation/**`, `features/home/**`, `apps/web/app/lib/roles.ts` (KPI fixtures only), the person-id fields in `features/campus/library/index.tsx` and `features/campus/residence/index.tsx`, approval-state fields per §6.4, `deploy/pilot/**`, `deploy/ci/seed-pilot.sh`, `.github/workflows/sutraerp-pilot.yml`, `docs/runbooks/{pilot-go-live,go-live-checklist}.md` |

### 2.2 Shared files: foundation-owned, do not edit

`apps/api/src/{config.rs,config_b9.rs,main.rs,lib.rs}`, `apps/api/src/app.rs` (**exception:** 9A
wires the metrics listener + sealer jobs, 9C wires the Flowable adapter + projector — each inside
one block marked `// ---- 9A ----` / `// ---- 9C ----`, appended after the existing job wiring),
`crates/kernel/src/{audit_chain.rs,auth.rs}`, `migrations/20261120000000_*`, every `lib.rs` mount
list, `apps/api/tests/architecture.rs`, `deploy/helm/sutraerp/{Chart.yaml,values.yaml,templates/_helpers.tpl,templates/core/configmap.yaml}`
(slices append keys **only inside their own values section**, marked `# 9X:`; 9B may restructure
`templates/core/**`), `.github/workflows/sutraerp-lockfile.yml`, this file. Unavoidable edits:
append at the end of the block with a `// 9X:` marker and report them.

Workspace `Cargo.toml` `[workspace.dependencies]`: append new crates at the end of the list under a
`# 9X:` comment; never bump an existing version (9D may, as a marked security fix).

### 2.3 Generated files

`openapi/openapi.json` + `packages/api-client/src/schema.d.ts`: only 9A (audit routes), 9C (search
route) and 9E change them; each regenerates from its own CI artifact `openapi-regenerated`.
`Cargo.lock`: never resolved locally — download artifact `cargo-lock` of the `SutraERP lockfile`
workflow run for your push and commit it (§13). The integrator regenerates both after each merge.

## 3. Telemetry (9A)

### 3.1 Environment (parsed in `config_b9.rs` → `Config::hardening.telemetry`)

| Variable | Default | Meaning |
|----------|---------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | unset (no export) | collector, e.g. `http://otel-collector.observability.svc:4317` |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | `grpc` \| `http/protobuf` |
| `OTEL_SERVICE_NAME` | `sutra-api` | `service.name` |
| `OTEL_RESOURCE_ATTRIBUTES` | unset | extra resource attrs; 9A always adds `service.version` (crate version), `deployment.environment` (`SUTRA_ENV`), `service.instance.id` (hostname) |
| `OTEL_TRACES_SAMPLER_ARG` | `1.0` | parent-based trace-id ratio |
| `SUTRA_METRICS` | `true` (not in `test`) | serve Prometheus text format |
| `SUTRA_METRICS_ADDR` | `127.0.0.1:9464` (audit A3 M6; set `0.0.0.0:9464` where Prometheus scrapes over the network) | separate listener, path `/metrics`; never routed through the web / ingress |

The existing `Config::otel_endpoint` stays (same variable). Logs stay JSON on stdout (collected by
Loki through the collector / promtail); with an endpoint set, 9A also exports logs via OTLP.

### 3.2 Spans

- HTTP server span name `"{METHOD} {route}"` (OTel HTTP semconv), attributes `http.request.method`,
  `http.route` (matched path, never the raw path), `http.response.status_code`, `url.scheme`,
  `sutra.request_id`, `sutra.tenant_id` (when authenticated; **never** user names / e-mails). The
  inbound `traceparent` is honoured (W3C propagation); outbound reqwest calls (Flowable, OpenSearch,
  OIDC) propagate it.
- Internal spans: `db.query` is **not** created per statement (too noisy); use `sutra.job`
  (`sutra.job.name` = `jobs::Job::name()`), `sutra.outbox.publish`, `sutra.audit.seal`,
  `sutra.workflow.<op>` (9C), `sutra.search.<op>` (9C).
- Every JSON log line inside a span carries `trace_id` and `span_id` (hex) fields.

### 3.3 Metrics (Prometheus names; prefix `sutra_`)

| Metric | Type | Labels |
|--------|------|--------|
| `sutra_http_requests_total` | counter | `method`, `route`, `status` (class `2xx`…`5xx`) |
| `sutra_http_request_duration_seconds` | histogram (buckets .005 … 10) | `method`, `route` |
| `sutra_http_requests_in_flight` | gauge | — |
| `sutra_db_pool_connections` | gauge | `state` = `idle` \| `used` \| `max` |
| `sutra_outbox_pending` | gauge | — (rows not yet relayed) |
| `sutra_outbox_published_total` / `sutra_outbox_failed_total` | counter | — |
| `sutra_job_runs_total` | counter | `job`, `outcome` = `ok` \| `error` \| `skipped` |
| `sutra_audit_unsealed` | gauge | — (rows without a chain entry) |
| `sutra_audit_chain_verify_failures_total` | counter | — |
| `sutra_workflow_calls_total` (9C) | counter | `op`, `outcome` |
| `sutra_search_projection_lag_seconds` (9C) | gauge | `projection` |
| `sutra_build_info` | gauge = 1 | `version`, `git_sha` (env `SUTRA_GIT_SHA`, set by the image build) |

Label cardinality: never ids, tenants or raw paths as labels.

### 3.4 Dashboards and alerts

Dashboards `deploy/observability/dashboards/sutra-{api-red,database,outbox-jobs,audit-chain,workflow-search}.json`
(uid `sutra-<name>`, datasource variables `${DS_PROMETHEUS}` / `${DS_LOKI}` / `${DS_TEMPO}`). Alert
rules `deploy/observability/alerts/sutra.rules.yaml`, group `sutra`, alerts:
`SutraApiHighErrorRate` (5xx > 2 % for 10 m), `SutraApiLatencyP95` (> 1 s for 10 m),
`SutraOutboxBacklog` (pending > 1000 for 15 m), `SutraAuditUnsealed` (> 0 for 30 m),
`SutraAuditChainBroken` (verify failures increase), `SutraJobFailing`, `SutraDbPoolSaturated`
(used / max > 0.9 for 10 m); each with `severity` and `runbook_url` (`docs/runbooks/observability.md#<alert>`)
and a `promtool test rules` fixture in `alerts/tests/`.

## 4. Tamper-evident audit chain (9F schema + hashing, 9A sealer / API)

### 4.1 Tables (migration `20261120000000_platform_audit_chain.sql`, already in the branch)

`platform.audit_chain_entry (tenant_id, seq bigint ≥ 1, audit_id uuid UNIQUE → audit_log.id,
prev_hash bytea(32), entry_hash bytea(32), sealed_at)` PK `(tenant_id, seq)`, append-only (trigger +
privileges); `platform.audit_chain_head (tenant_id PK, seq, head_hash, updated_at)` (the only row that
moves); `platform.audit_anchor (id, tenant_id, seq, head_hash, key_id, mac bytea(32), anchored_at)`
append-only, `UNIQUE (tenant_id, seq, key_id)`. `platform.audit_log` itself is unchanged and every
module keeps inserting through `sutra_persistence::audit::insert`.

### 4.2 Hash (pure, `sutra_kernel::audit_chain`)

`entry_hash = SHA-256("sutra.audit.v1\n" + hex(prev_hash) + "\n" + canonical_json(row))`, genesis
`prev_hash` = 32 zero bytes. `canonical_json`: object of all 12 `audit_log` columns as camelCase keys
(`action, actorId, actorKind, correlationId, details, id, institutionId, occurredAt, outcome,
targetId, targetType, tenantId`), keys sorted at every level, compact, `occurredAt` =
`%Y-%m-%dT%H:%M:%S%.6fZ` UTC, absent = `null`. Anchor MAC = HMAC-SHA-256(key,
`"sutra.audit.anchor.v1\n{tenant}\n{seq}\n{hex(head_hash)}"`). Test vector (sample row in the unit
test): `890495a8bd29552c520acb3b573f2823ed646c54e8029aaa6dfa52c7f59a5067`. Never change these bytes;
a new format is `sutra.audit.v2` with a migration.

### 4.3 Sealer rules (9A, `sutra_persistence::audit_chain`)

- One sealer at a time: `pg_try_advisory_lock(hashtext('sutra.audit.chain'))` (skip the tick if held);
  runs as a `jobs::Job` named `audit_seal` every `SUTRA_AUDIT_SEAL_SECS` when `SUTRA_AUDIT_CHAIN=true`.
- Per tenant: `SELECT … FROM audit_chain_head WHERE tenant_id = $1 FOR UPDATE` (insert at first
  seal), unsealed rows `audit_log a LEFT JOIN audit_chain_entry e ON e.audit_id = a.id WHERE e IS NULL
  AND a.tenant_id = $1 ORDER BY a.id LIMIT SUTRA_AUDIT_SEAL_BATCH`; seq continues from the head; one
  transaction per tenant batch. Seal order is chain order (a late-committing row is simply sealed in a
  later tick — `occurred_at` order is not required).
- Anchors: job `audit_anchor` every `SUTRA_AUDIT_ANCHOR_SECS` (0 = off) writes one anchor per tenant
  whose head moved, with `SUTRA_AUDIT_ANCHOR_KEY` / `SUTRA_AUDIT_ANCHOR_KEY_ID`; no key → job logs once
  and skips. Production: 9A adds a `validate()` rule "chain on ⇒ anchor key set" **with** the config
  test changes.
- `verify(tenant, from_seq, to_seq)` recomputes from `audit_log` and the previous entry, checks
  `prev_hash` linkage, head equality and each anchor MAC; returns the first broken `seq` + reason
  (`hash_mismatch` | `link_broken` | `missing_row` | `anchor_mismatch`).

### 4.4 API (9A; tag `platform-audit`, operation ids `platform_audit_*`, permission `platform.audit.verify`)

| Method + path | Response |
|---------------|----------|
| `GET /api/v1/platform/audit/chain` (`platform_audit_chain_status`) | `{tenantId, headSeq, headHash (hex), sealed, unsealed, lastSealedAt, lastAnchor {seq, keyId, anchoredAt} \| null}` |
| `POST /api/v1/platform/audit/chain/verify` (`platform_audit_chain_verify`), body `{fromSeq?, toSeq?}` | `{ok, checked, firstBrokenSeq \| null, reason \| null, headSeq, headHash}` (200 even when broken; the verify itself is audited) |
| `GET /api/v1/platform/audit/anchors?limit=` (`platform_audit_anchor_list`) | `[{seq, headHash, keyId, mac, anchoredAt}]` |

Tenant = the session's tenant only (no cross-tenant parameter). Problem+json as everywhere.

## 5. Secrets: OpenBao paths (9B)

KV v2 mount `kv`, prefix `sutraerp/<env>` (`<env>` = `global.environment`). Every key below is also
the env variable name the API reads; the OpenBao Agent template renders all groups into one dotenv
file `/vault/secrets/sutra.env` and the pod sets `SUTRA_ENV_FILE=/vault/secrets/sutra.env`
(`main.rs` loads it without overriding the environment — already in the branch).

| Path (`kv/data/sutraerp/<env>/…`) | Keys |
|-------------------------------------|------|
| `database` | `DATABASE_URL` (runtime role), `SUTRA_MIGRATION_DATABASE_URL` (owner), `SUTRA_DB_RUNTIME_USER`, `SUTRA_DB_RUNTIME_PASSWORD` |
| `app` | `SUTRA_CSRF_KEY`, `SUTRA_MFA_ENCRYPTION_KEY` (64 hex each) |
| `oidc` | `SUTRA_OIDC_CLIENT_SECRET` |
| `smtp` | `SUTRA_SMTP_USERNAME`, `SUTRA_SMTP_PASSWORD` (when used) |
| `audit` | `SUTRA_AUDIT_ANCHOR_KEY` (64 hex), `SUTRA_AUDIT_ANCHOR_KEY_ID` |
| `flowable` | `SUTRA_FLOWABLE_PASSWORD` (+ `FLOWABLE_DB_PASSWORD` for the Flowable pod) |
| `opensearch` | `SUTRA_OPENSEARCH_USER`, `SUTRA_OPENSEARCH_PASSWORD` |
| `backup` | `SUTRA_BACKUP_S3_ACCESS_KEY`, `SUTRA_BACKUP_S3_SECRET_KEY`, `SUTRA_BACKUP_AGE_RECIPIENT` |
| `payments` (INT, SEC-9D-003) | `SUTRA_PAYMENT_WEBHOOK_SECRET` (≥ 32 chars; only read when `SUTRA_PAYMENT_GATEWAY=mock`; the dev constant is refused in production) |

Kubernetes auth role `sutraerp` bound to ServiceAccounts `sutraerp-api`, `sutraerp-migrate`,
`sutraerp-backup` in the release namespace; policy `sutraerp-<env>` = `read` on
`kv/data/sutraerp/<env>/*` only. CI (kind) uses OpenBao **dev mode** with throwaway values written by
`deploy/openbao/seed-dev.sh`; no app credential is ever a Helm value or a committed file. Fallback
`secrets.mode=kubernetes` (an existing Secret with the same keys) exists for clusters without
OpenBao.

## 6. Kernel, API and database rules

### 6.1 Permissions (already in `sutra_kernel::auth`, administrators only, no step-up)

`AuditVerify` = `platform.audit.verify` (9A), `GovernanceManage` = `campus.governance.manage` (9E;
decisions reuse `campus.approve`, step-up), `AccreditationManage` = `services.accreditation.manage`
(9E; decisions reuse `services.report.approve`, step-up).

### 6.2 Migration ranges (`<version>_<schema>_<what>.sql`)

9F `20261120000000` (exists); 9A `20261120000010`–`019`; 9C `…020`–`029`; 9E `…030`–`039`
(`_campus_` governance) and `…040`–`049` (`_services_` accreditation). 9B / 9D: none.

### 6.3 Path prefixes, tags, operation ids

| Slice | Paths | Tag | operation_id prefix |
|-------|-------|-----|---------------------|
| 9A | `/api/v1/platform/audit/…` | `platform-audit` | `platform_audit_` |
| 9C | `/api/v1/platform/search` | `platform-search` | `platform_search_` |
| 9E | `/api/v1/campus/…` (governance) | `campus-governance` | `campus_governance_` |
| 9E | `/api/v1/services/…` (accreditation) | `services-accreditation` | `services_accreditation_` |

The 9E rows are already in `architecture.rs::batch8_operations_follow_the_slice_prefix_and_tag_rules`
(first path parameter `{id}`, explicit operation ids). Conventions as Batch 8 §5.1: ETag / If-Match,
Idempotency-Key on creates, audit + outbox in the same tx, problem+json, tenant scope from the session.

### 6.4 Approval-state normalisation (9E, additive)

Canonical wire field `approvalState` ∈ `not_requested | pending | approved | rejected` on every
Batch 8 aggregate view that has an approval step. 8A maintenance plans already comply. 8C library
policies + reporting frameworks: `not_submitted` → `not_requested` on the wire (DB value `none` stays;
map in the view; DB tests updated). 8B / 8D aggregates: add `approvalState` derived from status +
`approvalPending`; `approvalPending` stays (documented as deprecated) so existing clients keep
working. Web reads `approvalState` through one helper `approvalStateLabel` in
`apps/web/app/features/campus/approval.ts` (9E, new). Record the rule in `docs/batches.md`.

## 7. Flowable deployment (9C)

- Runtime: `flowable/flowable-rest` (pin the newest available **8.x** tag; if none exists, the newest
  7.x and record it), own PostgreSQL database `flowable` (role `flowable`) on the same server, basic
  auth user `rest-admin` (password from OpenBao). Chart: `templates/data/flowable-*.yaml`
  (Deployment 1 replica, Service `sutraerp-flowable:8080`, readiness
  `/flowable-rest/actuator/health`, `values.flowable.*`).
- BPMN: one file per approval process key `deploy/flowable/processes/<key>.bpmn20.xml`, generated by
  `deploy/flowable/gen-bpmn.py` from the `pub const …: &str = "<domain>.<process>"` keys in
  `modules/*/src/shared/approval.rs` (CI fails if a key has no BPMN). Shape: start → exclusive gateway
  on `${autoApprove}` → (true) end `approved` | (false) user task `decide` (candidate group =
  `sutra:approvers`) → end. Process id = the key verbatim.
- Adapter `FlowableWorkflowAdapter` (`SUTRA_WORKFLOW=flowable`): `start_process` →
  `POST {url}/service/runtime/process-instances` `{processDefinitionKey, businessKey, variables:
  [sutraTenantId, sutraInstitutionId, autoApprove, …caller variables]}` (no Flowable tenancy);
  `complete_task` / `signal` as in the module doc; deploys missing BPMN on start when
  `SUTRA_FLOWABLE_DEPLOY_ON_START=true`; errors → `KernelError` (unavailable → 503 problem); 9C
  passes `autoApprove = (ApprovalMode == AutoApprove)` through a new optional variable, without editing
  modules: the adapter sets `autoApprove=true` unless the caller variables contain `"autoApprove":
  false`.
- Decision sync: a platform outbox consumer completes the `decide` task when a `…status_changed` /
  decided event carries `processInstanceId` of a live Flowable instance (`outcome` = `toStatus`), so
  domain decision endpoints stay unchanged.
- Default stays **noop**; DB tests keep auto-approving. CI proof: `Data platform` workflow + kind.

## 8. OpenSearch projections (9C)

- Index naming: `<prefix>-<projection>-v<major>`, write + read alias `<prefix>-<projection>`;
  `<prefix>` = `SUTRA_OPENSEARCH_INDEX_PREFIX` (default `sutra-<env>`). Templates in
  `deploy/opensearch/templates/<projection>.json`.
- Projections (v1): `students` (id, tenantId, institutionId, number, displayName, programme, status),
  `staff` (id, tenantId, institutionId, number, displayName, department, status), `library-catalogue`
  (id, tenantId, institutionId, title, authors, isbn, subjects, availableCopies). Document `_id` =
  aggregate id; every document carries `tenantId` and every query filters on it (no cross-tenant
  reads, ever).
- Feed: outbox events (by `type`) through a projector job `search_project` with checkpoint table
  `platform.search_checkpoint (projection PK, last_outbox_id, updated_at)` (9C migration), external
  versioning by aggregate `version` so replays are idempotent; `search_rebuild` re-reads through
  the read-only `SearchSource` port (`sutra_contracts::platform::search`), implemented by each owning module in
  `src/search_source.rs` over its own schema (no foreign-schema SQL in `sutra-platform`).
- API `GET /api/v1/platform/search?q=&projection=&limit=` (`platform_search_query`), permission = the
  projection's read permission (`student.read`, `people.read`, `campus.read`).
- `SUTRA_SEARCH=off` (default): projector not started, search API answers 503 problem
  `search_disabled`.

## 9. Performance and capacity gates (9D)

Runner: GitHub `ubuntu-24.04` (4 vCPU), API release build + PostgreSQL 18 on the same runner, dev
auth, data from the pilot seed (+ 2 000 generated students). k6 scripts `deploy/perf/{smoke,capacity}.js`.

| Gate | Load | Thresholds (fail the job) |
|------|------|---------------------------|
| `perf-smoke` (every push) | 20 VU, 60 s, mixed read (70 %) / write (30 %) | `http_req_failed < 1 %`; reads `p(95) < 300 ms`; writes `p(95) < 800 ms`; `checks > 99 %` |
| `capacity` (erp_merge, tags, `workflow_dispatch`) | ramp 10 → 100 VU over 5 min, hold 3 min | `http_req_failed < 2 %`; overall `p(95) < 1 s`; sustained `≥ 150 req/s`; results vs `deploy/perf/baseline.json` (regression > 25 % **and** > 10 ms on a p95, or < 75 % of the baseline req/s, fails — INT deviation: 10 ms noise floor, see §14) |

Summary JSON uploaded as artifact `perf-results`; the job summary prints the table. Thresholds change
only by a contract change.

## 10. Packaging, images, CI

### 10.1 Helm values layout

`deploy/helm/sutraerp/values.yaml` (in the branch) is the layout: `global`, `image.{api,web}`, `api`,
`web`, `migrations`, `ingress`, `secrets`, `postgres|nats|valkey` (`mode: external|bundled`),
`networkPolicy` (9B); `observability` (9A); `flowable`, `opensearch`, `backup` (9C). CI values:
`ci/kind.yaml`; pilot values: `values-pilot.yaml` (9B). Release name **`sutraerp`** in namespace
**`sutraerp`** in CI, so in-cluster names are `sutraerp-<component>` (`sutraerp-api`,
`sutraerp-web`, `sutraerp-postgres`, `sutraerp-nats`, `sutraerp-valkey`, `sutraerp-flowable`,
`sutraerp-opensearch`, `sutraerp-lgtm`). Components label `app.kubernetes.io/component`.

### 10.2 Images, tags, signing

| Image | Dockerfile |
|-------|-----------|
| `ghcr.io/varad1614/sutraerp-api` | `deploy/docker/api.Dockerfile` (`CARGO_PROFILE=release`, build arg `SUTRA_GIT_SHA`) |
| `ghcr.io/varad1614/sutraerp-web` | `deploy/docker/web.Dockerfile` |

Tags: `sha-<12 hex>` always; `edge` on `erp_merge`; `vX.Y.Z` + `X.Y` on tag `vX.Y.Z`; branch builds
tag `ci` and never push. Signing: `cosign sign --yes <image>@<digest>` keyless (GitHub OIDC,
`id-token: write`), SBOM `syft` SPDX JSON attested with `cosign attest --type spdxjson`, provenance
via `docker/build-push-action` `provenance: mode=max`. Verify identity:
`--certificate-identity-regexp '^https://github.com/Varad1614/ERP_hark_2/.github/workflows/sutraerp-ci.yml@refs/(heads/erp_merge|tags/v.*)$'
--certificate-oidc-issuer https://token.actions.githubusercontent.com`. The chart deploys by digest
when `image.*.digest` is set.

### 10.3 Workflows and job names (stable; the pinger and `b9-ci.sh` read them)

| Workflow file | Owner | Jobs |
|---------------|-------|------|
| `sutraerp-ci.yml` (existing, now also on `batch9/**`) | all / 9B images | Rust, Rust integration, OpenAPI drift, Web, E2E matrix, Container images |
| `sutraerp-observability.yml` | 9A | `Observability (OTel → Prometheus · Loki · Tempo)` |
| `sutraerp-deploy.yml` | 9B | `Helm (lint · template)`, `Kind (install · migrate · smoke · telemetry · DR)`, + `Compose boot (docker compose --profile app)` |
| `sutraerp-data.yml` | 9C | `Data platform (Flowable · OpenSearch · backup → restore)` |
| `sutraerp-quality.yml` | 9D | `Security (…)`, `Image scan + ZAP baseline (…)`, `Performance gate (k6)` |
| `sutraerp-pilot.yml` | 9E | `Pilot seed (API + PostgreSQL)` |
| `sutraerp-lockfile.yml` | 9F | `Cargo.lock refresh (artifact cargo-lock)` (always green) |

All Batch 9 workflows trigger on `main`, `erp_merge`, `batch9/**`, tags `v*`; GHCR push / sign
steps are gated `if: github.ref == 'refs/heads/erp_merge' || startsWith(github.ref, 'refs/tags/v')`
and need `permissions: packages: write, id-token: write` on that job only.

### 10.4 Kind stage scripts (`deploy/ci/*.sh`, run from `sutraerp/`)

Order in the kind job: `smoke.sh` (9B) → `check-telemetry.sh` (9A) → `seed-pilot.sh` (9E) →
`dr-drill.sh` (9C). Inputs: `SUTRA_CI_API_URL` (e.g. `http://localhost:8080`, port-forwarded),
`SUTRA_CI_WEB_URL`, `SUTRA_CI_NS=sutraerp`, `KUBECONFIG`, plus `SUTRA_CI_MODE=kind|compose`
(9A / 9C also run their script in their own workflow with `compose`). Output: one `ok <check>` line
per check; non-zero exit on the first failure; `--dry-run` prints and exits 0 (foundation stubs).
Each script must be idempotent.

## 11. Backup format (9C)

Directory (or tarball `<name>.tar`) `sutra-backup-<env>-<UTC YYYYMMDDTHHMMSSZ>/`:
`manifest.json` + `sutra.dump` + `flowable.dump` (if Flowable enabled), dumps `pg_dump -Fc
--no-owner` — **ACLs are kept** (integration deviation from the original `--no-privileges`: the
append-only audit grants of ADR-L5-0012 are part of the evidence, so `sutra-restore.sh` first
re-creates the cluster roles from `deploy/backup/roles.sql`, then `pg_restore --no-owner` restores the
grants; `sutra-api migrate` re-grants afterwards). Manifest:

```json
{ "format": "sutra-backup/v1", "createdAt": "…Z", "environment": "development",
  "gitSha": "<api image SUTRA_GIT_SHA>", "migrationVersion": 20261120000049,
  "databases": [{ "name": "sutra", "file": "sutra.dump", "bytes": 0, "sha256": "…" }],
  "auditHeads": [{ "tenantId": "…", "seq": 0, "headHash": "…" }],
  "rowCounts": { "<schema>.<table>": 0 } }
```

Restore (`sutra-restore.sh <dir>`): verify sha256 → `pg_restore --clean --if-exists` into empty DBs →
`sutra-api migrate` (idempotent; re-grants roles) → compare `rowCounts` and `auditHeads` → `POST
/platform/audit/chain/verify` ok → `search_rebuild`. NATS / Valkey are not backed up (the outbox and
PG are the source of truth). Optional encryption: `age -r $SUTRA_BACKUP_AGE_RECIPIENT` (production);
CI uses plain files. Targets: PVC (CI, pilot) or S3-compatible (`values.backup.target.s3`). RPO /
RTO targets for the pilot: RPO 24 h (nightly), RTO 2 h; the DR drill records the measured RTO.

## 12. Domain carry-overs (9E)

### 12.1 G00 campus governance — 15 scenarios (`sutra-campus`, `governance/`, schema `campus`)

G-001 readiness assessment → governance readiness decision; 002 campus operations evidence →
accreditation evidence readiness; 003 operational review → governance closure; 004 executive review →
recommendation; 005 operational risk → closure (risk register); 006 multi-domain action plan →
verified closure; 007 multi-domain SLA deviation → resolution (reads 8A SLA breaches through
`campus.service_ticket`); 008 operational health assessment → decision; 009 KPI review → outcome
(reads `state`-local campus metrics; no other schema); 010 maturity assessment; 011 finding →
corrective action decision; 012 escalation → executive resolution; 013 evidence / dependency exception
→ resolution; 014 governance configuration change → effective state (versioned, `campus.approve`);
015 governance review → accreditation evidence (publishes `sutraerp.campus.governance_evidence.published`
that 9E's accreditation consumes via the inbox/outbox, never by SQL). Tables (suggested):
`campus.governance_{assessment,review,risk,action_plan,action_item,finding,escalation,exception,config_version,evidence}`.

### 12.2 H09 accreditation — 11 scenarios (`sutra-services`, `accreditation/`, schema `services`)

H-061 framework change → active framework (NAAC / NBA / NIRF criteria, versioned); 062 cycle →
approved assessment plan; 063 compliance obligation → requirement lifecycle; 064 evidence request →
approved evidence record (may cite 8C regulatory snapshots and G-015 governance evidence by id);
065 internal audit plan → closed audit report; 066 external assessment notice → accepted outcome;
067 compliance finding → verified CAPA closure; 068 quality improvement proposal → verified outcome;
069 source evidence → institutional readiness decision; 070 periodic compliance review → compliance
status; 071 approved evidence → self-study report approval + hand-off. Figures only through
`state.metrics` (`OperationalMetricsPort`). Tables `services.accreditation_*`.

### 12.3 Web

Admin Campus tab `governance` (`GovernanceAdminTab`, folder `features/campus/governance/`), Admin
Reports tab `accreditation` (`AccreditationAdminTab`, folder `features/reports/accreditation/`);
9E adds the two tab entries to the frames `routes/admin/campus-setup.tsx` and
`routes/admin/reports.tsx` (one line each, marked `// 9E`). Person pickers: a reusable
`PersonPicker` in `features/campus/PersonPicker.tsx` over the existing directory endpoints. Home:
replace `lib/roles.ts` KPI values with live queries in `features/home/live.tsx`; a figure without a
source shows the pending tile, never a number. `@sutra/ui` only.

### 12.4 Pilot

`deploy/pilot/seed-pilot.mjs` (node, uses the e2e / demo-seed harness): tenant
`0192f5a0-7c3e-7000-8000-000000009110` (fixed), institution code
`PILOT`, active, one campus hierarchy, programme + calendar, 3 staff, 50 students, fee structure,
library + hostel basics; idempotent. `docs/runbooks/pilot-go-live.md` (roles, timeline T-14 … T+7,
cut-over steps on a real cluster using the chart + OpenBao, data migration from spreadsheets, rollback)
and `docs/runbooks/go-live-checklist.md` (checkbox list: security sign-off from 9D findings, DR drill
evidence, dashboards + alerts, backups verified, accounts + MFA, support contacts).

## 13. Worker environment: CI-only

| Slice | Worktree | Branch (off `batch9/foundation`) |
|-------|----------|--------|
| 9A | `/workspace/work/erp-9a` | `batch9/9a` |
| 9B | `/workspace/work/erp-9b` | `batch9/9b` |
| 9C | `/workspace/work/erp-9c` | `batch9/9c` |
| 9D | `/workspace/work/erp-9d` | `batch9/9d` |
| 9E | `/workspace/work/erp-9e` | `batch9/9e` |

**NO local cargo** except `cargo fmt --all`; no pnpm build / typecheck / test, no docker, no kind, no
chromium. `helm lint` / `helm template` locally are allowed (static binary). Push and poll
`bash /workspace/work/tools/b9-ci.sh batch9/9x` (all workflows of the head sha). New crates: commit
the `cargo-lock` artifact (§2.3). OpenAPI drift: commit the `openapi-regenerated` artifact.

## 14. Merge order and known gaps

- Merge order: **9A → 9C → 9B → 9D → 9E** (9B's kind job then runs the real 9A / 9C stage scripts;
  9D scans the final images; 9E last because it carries the OpenAPI churn). After each merge:
  regenerate `Cargo.lock` + OpenAPI from CI artifacts.
- Integrator: one green run of every workflow on `batch9/integrate` with the kind job's four stages
  live, `docs/batches.md` Batch 9 section (in-scope / deferred from §1), squash to `batch9/squash`;
  erp_merge only with the user's OK. The first real GHCR push + signature happens on that merge.
- `NoopWorkflowAdapter` remains the default; Flowable is proved but switched on per environment.
- Logical backups only (no PITR); RPO 24 h for the pilot.
- **Integration deviations (recorded at merge):** (1) §11 backups keep ACLs (see §11); (2) §9 the
  capacity regression check has a 10 ms absolute noise floor and a throughput floor (75 % of the
  baseline req/s) — 9D's dispatched capacity run 38564995071 read +52 % on a 15 ms p95 with no API
  change since the baseline commit; (3) the 9C `metrics` facade families are bridged into 9A's
  prometheus-client registry (`apps/api/src/metrics_bridge.rs`), one `/metrics` listener; (4)
  SEC-9D-003: `SUTRA_PAYMENT_GATEWAY` (`disabled` default in production, `mock` in development /
  test) + `SUTRA_PAYMENT_WEBHOOK_SECRET` (OpenBao group `payments`, dev constant refused in
  production); disabled → online checkout, recovery polling, gateway refunds and the webhook answer
  503 `dependency-unavailable`.
