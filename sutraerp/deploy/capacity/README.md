# Capacity test — production-mode stack, 2,000-student college (Part 2 · S5-capacity)

Report: [`docs/readiness/CAPACITY_TEST_REPORT.md`](../../docs/readiness/CAPACITY_TEST_REPORT.md).
CI: `.github/workflows/sutraerp-capacity.yml` — **SutraERP Part 2 capacity (2,000 students)**
(`workflow_dispatch`; inputs `stage`, `students`, `scale`, `attendance_weeks`; a push that changes
`deploy/capacity/RUN` on a `part2/**` branch also starts it). Artifact `capacity-evidence`.

| File | Purpose |
|---|---|
| `run.sh prepare\|up\|seed\|load\|report\|logs\|down` | the whole procedure (CI and the college server run the same steps) |
| `compose.yml` | production-mode stack: edge nginx (TLS, host network) → web nginx → api (`SUTRA_ENV=production`), migrate, PostgreSQL 18 (TLS, `pg_stat_statements`), Keycloak 26.7 (`start`), Flowable 8, NATS, Valkey, MinIO (SSE-S3) |
| `edge.conf.template` | TLS for `erp.` / `sso.` / `s3.<domain>`; replaces client `X-Forwarded-For` (API: `SUTRA_TRUSTED_PROXY_HOPS=2`) |
| `gen-secrets.sh` | per-run random passwords / keys (`$CAP_DIR/env`) and a private CA (API trusts it via `SUTRA_TLS_CA_FILE`) |
| `gen-realm.py` | realm from `deploy/keycloak/realm-sutra-dev.json` with https URIs, random client secrets, 100 faculty and 40 office accounts (office = administrator → TOTP pre-enrolled) |
| `initdb/00-capacity.sh` | Keycloak database / role with a random password, owner roles, `pg_stat_statements` |
| `seed.mjs` | the college through the API (Keycloak OIDC + TOTP, human approvals): Stage 4 imports (departments, programmes, 150 staff, 2,000 students + guardians, opening balances with approval), identities provisioned by the API, 102 divisions with faculty, registrations, timetable, attendance, fees + payments, exam → published results, an open internal assessment, one verified PDF per student |
| `k6/college-day.js` | 21.5-minute day: 60 faculty + 40 office staff throughout; 300 students (0–10 min), results-day spike 500 (10–15), fee-deadline spike 400 (15–21.5); each VU with its own source address (`--local-ips`) |
| `collect.sh` | docker stats (5 s), PostgreSQL connections / waits / locks (10 s), snapshots (DB + table sizes, cache hit, deadlocks, temp files, volume sizes, API `/metrics`), `pg_stat_statements` |
| `analyze.py` | `capacity-results.{md,json}`; enforces `thresholds.json` per phase and per endpoint (exit 1) |
| `thresholds.json` | pass criteria — fixed before the first measured run; never loosened to pass |

## Run it (any Ubuntu host with Docker, sudo, Node 22, k6, psql, a Rust toolchain or a prebuilt API)

```bash
cd sutraerp
cargo build --release -p sutra-api --locked
mkdir -p deploy/docker/demo-bin && cp target/release/sutra-api deploy/docker/demo-bin/
MINIO_TAG=RELEASE.2025-10-15T17-29-55Z OUT=deploy/docker/demo-bin/minio bash deploy/minio/build-from-source.sh
export CAP_DIR=$PWD/capacity-run CAP_OUT=$PWD/capacity-out
bash deploy/capacity/run.sh prepare
bash deploy/capacity/run.sh up
bash deploy/capacity/run.sh seed
bash deploy/capacity/run.sh load
bash deploy/capacity/run.sh report     # exit 1 = a threshold was breached
bash deploy/capacity/run.sh logs; bash deploy/capacity/run.sh down
```

`$CAP_DIR` holds the generated secrets, the CA key and the office accounts' TOTP secrets: never
copy it off the host; `$CAP_OUT` is the evidence (safe to share).

The college-server procedure (its own certificates, DNS and a load generator on a separate
machine) is in the report, section "To complete on the college server".
