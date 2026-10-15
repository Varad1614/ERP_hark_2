# Performance and capacity gates (slice 9D)

Contract: `docs/batch9-contracts.md` §9 (thresholds change only by a contract change).

| File | Purpose |
|------|---------|
| `run.sh smoke\|capacity` | migrate → serve the release API (dev auth) → seed → `k6 run` → `compare.mjs` (CI entry point) |
| `seed.mjs` | 2 000 students into the demo institution **through the admission API** (application → offer → documents → approval → clearance → confirm → create-student → enrol) |
| `lib.js` | shared k6 workload: 70 % reads / 30 % writes over student, academic, finance, campus (+ institution profile); requests tagged `kind` + `name` |
| `smoke.js` | `perf-smoke`: 20 VU · 60 s; `http_req_failed < 1 %`, read p95 < 300 ms, write p95 < 800 ms, checks > 99 % |
| `capacity.js` | `capacity`: 10 → 100 VU over 5 min, hold 3 min; `http_req_failed < 2 %`, p95 < 1 s, ≥ 150 req/s |
| `compare.mjs` | job-summary table + capacity regression check (a p95 > 25 % **and** > 10 ms above `baseline.json`, or throughput < 75 % of the baseline, fails; the 10 ms floor absorbs shared-runner noise — integration note in the file) |
| `baseline.json` | p95s from the first green CI run of each gate (artifacts `perf-results` / `perf-results-capacity`) |

**Capacity re-baseline (2026-10-15):** the capacity entry is now the mean of two green CI runs on the Batch 9 tree
(38645104974 + 38647826036, e1043a46), taken after the audit-sealer fix (fc785ffd) and `synchronous_commit=off` on CI PG.
The Batch 9 telemetry/audit overhead is accepted for now; profiling it is a pre-pilot follow-up. Thresholds and SLOs are unchanged.

CI: `.github/workflows/sutraerp-quality.yml` — `Performance gate (k6)` (every push) and `Capacity (k6)`
(erp_merge, `v*` tags, `workflow_dispatch`). Results: artifact `perf-results` (`smoke-summary.json`,
`smoke-raw.json`, `perf-data.json`, API + seed logs).

Local (needs a running PostgreSQL, a release `sutra-api`, node 22 and k6):
`DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra bash deploy/perf/run.sh smoke`.
