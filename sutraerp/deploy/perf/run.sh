#!/usr/bin/env bash
# Performance gate runner (Batch 9 · slice 9D, docs/batch9-contracts.md §9). Run from sutraerp/.
#
#   bash deploy/perf/run.sh smoke|capacity [--dry-run]
#
# Expects PostgreSQL in DATABASE_URL (schema owner) and the API release binary at SUTRA_API_BIN
# (default target/release/sutra-api), `psql`, `jq`, `node` and `k6` on PATH. Steps:
#   1. `sutra-api migrate` (development);
#   2. `sutra-api serve` in dev auth on SUTRA_PERF_API (default http://127.0.0.1:8080), memory
#      cache, outbox relay off (the gate measures the request path, not NATS);
#   3. data: demo institution (apps/web/e2e/demo-seed.mjs), the pilot seed when 9E's
#      deploy/pilot/seed-pilot.mjs exists, then deploy/perf/seed.mjs (2 000 students through the
#      admission API);
#   4. `k6 run deploy/perf/<gate>.js` → $SUTRA_PERF_OUT/<gate>-summary.json (+ raw), then
#      deploy/perf/compare.mjs (job summary table, capacity regression check vs baseline.json).
# Prints one "ok <check>" line per step; exits non-zero on the first failure (k6 threshold
# failures included) after the summary has been written. Idempotent per database (the seeds
# reuse what exists; perf students are only added).
set -euo pipefail
# SUTRA_PERF_API_ENV: optional space-separated KEY=value pairs for the API under test (capacity
# A/B via the workflow_dispatch input `api_env`; empty in every gate run).
gate="${1:-}"
[[ "$gate" == smoke || "$gate" == capacity ]] || { echo "usage: run.sh smoke|capacity [--dry-run]" >&2; exit 2; }
if [[ "${2:-}" == "--dry-run" ]]; then
  echo "perf $gate: would migrate, serve, seed (demo + pilot + ${PERF_STUDENTS:-2000} students), k6 run deploy/perf/$gate.js"
  exit 0
fi

export DATABASE_URL="${DATABASE_URL:-postgres://sutra:sutra@localhost:5432/sutra}"
API_BIN="${SUTRA_API_BIN:-target/release/sutra-api}"
API_URL="${SUTRA_PERF_API:-http://127.0.0.1:8080}"
OUT="${SUTRA_PERF_OUT:-$PWD/perf-results}"
mkdir -p "$OUT"
export SUTRA_PERF_API="$API_URL" SUTRA_PERF_OUT="$OUT" SUTRA_PERF_DATA="$OUT/perf-data.json"
export E2E_API_URL="$API_URL" PERF_DATA="$OUT/perf-data.json"
export SUTRA_DEV_TENANT_ID="${SUTRA_DEV_TENANT_ID:-0192f5a0-7c3e-7000-8000-000000000001}"

# SUTRA_PERF_SYNC_COMMIT_OFF=true (set by the CI jobs only, never for a shared database):
# synchronous_commit=off on the throwaway CI PostgreSQL. Measured on runs 38604890454 (c6ae5964,
# p95 227.6 ms) and 38604887078 (131.6 ms): the slow runs spend their time in LWLock:WALWrite /
# IO:WalSync (commit fsync on the runner's disk, iowait 5–9 %), while the same code on another
# runner reads 15.6 ms — the gate measured the runner's disk, not the API. Durability of a
# production database is untouched (Helm / compose keep the PostgreSQL defaults).
if [[ "${SUTRA_PERF_SYNC_COMMIT_OFF:-false}" == true ]]; then
  psql "$DATABASE_URL" -XAtq -c "ALTER SYSTEM SET synchronous_commit = off" -c "SELECT pg_reload_conf()" >/dev/null
  echo "ok ci-database synchronous_commit=$(psql "$DATABASE_URL" -XAtc 'SHOW synchronous_commit')"
fi

SUTRA_ENV=development "$API_BIN" migrate >"$OUT/migrate.log" 2>&1 || { tail -n 40 "$OUT/migrate.log" >&2; exit 1; }
echo "ok migrate"

if ! curl -fsS "$API_URL/health/live" >/dev/null 2>&1; then
  hostport="${API_URL#http://}"
  SUTRA_ENV=development SUTRA_HTTP_ADDR="$hostport" SUTRA_LOG_FORMAT=json RUST_LOG="${RUST_LOG:-warn}" \
    SUTRA_AUTH_MODE=dev SUTRA_DEV_MFA=false SUTRA_SESSION_COOKIE_SECURE=false \
    SUTRA_OUTBOX_RELAY=off SUTRA_CACHE=memory SUTRA_INBOX_CONSUMER=false \
    SUTRA_WEBAUTHN_RP_ID=localhost SUTRA_WEBAUTHN_ORIGINS=http://localhost:3000 \
    nohup env ${SUTRA_PERF_API_ENV:-} "$API_BIN" serve >"$OUT/api.log" 2>&1 &
  echo $! >"$OUT/api.pid"
  for _ in $(seq 1 120); do
    curl -fsS "$API_URL/health/live" >/dev/null 2>&1 && break
    sleep 1
  done
  curl -fsS "$API_URL/health/live" >/dev/null || { echo "API did not come up" >&2; tail -n 50 "$OUT/api.log" >&2; exit 1; }
fi
echo "ok api-serve $API_URL"

DEMO_CODE="${DEMO_CODE:-DEMO}" node apps/web/e2e/demo-seed.mjs >"$OUT/seed-demo.log" 2>&1 || { tail -n 40 "$OUT/seed-demo.log" >&2; exit 1; }
echo "ok seed-demo"

if [[ -f deploy/pilot/seed-pilot.mjs ]]; then
  PILOT_STRICT_TENANT=false SUTRA_CI_MODE=compose SUTRA_CI_API_URL="$API_URL" bash deploy/ci/seed-pilot.sh >"$OUT/seed-pilot.log" 2>&1 || { tail -n 40 "$OUT/seed-pilot.log" >&2; exit 1; }
  echo "ok seed-pilot"
else
  echo "ok seed-pilot (skipped: deploy/pilot/seed-pilot.mjs not present on this branch)"
fi

# pipefail: a failed seed stops the script here.
DEMO_CODE="${DEMO_CODE:-DEMO}" node deploy/perf/seed.mjs 2>&1 | tee "$OUT/seed-perf.log"
echo "ok seed-students $(jq -r .students "$OUT/perf-data.json")"

# Diagnostics next to the load (cheap, one sample per 5 s): host CPU / IO / steal (vmstat),
# PostgreSQL wait events of the active backends, and the API's own /metrics at the end
# (server-side per-route latency, pool gauges) — so a slow run can be attributed to the
# runner, the database or the API.
vmstat -t 5 >"$OUT/$gate-vmstat.log" 2>&1 &
diag_pids=$!
(
  while sleep 5; do
    psql "$DATABASE_URL" -XAtF ' | ' -c "SELECT to_char(now(), 'HH24:MI:SS'), coalesce(wait_event_type, 'CPU'), \
      coalesce(wait_event, '-'), count(*) FROM pg_stat_activity WHERE datname = current_database() \
      AND pid <> pg_backend_pid() AND state = 'active' GROUP BY 2, 3 ORDER BY 4 DESC" 2>&1
  done
) >"$OUT/$gate-pg-waits.log" 2>&1 &
diag_pids="$diag_pids $!"
set +e
k6 run --quiet deploy/perf/"$gate".js 2>&1 | tee "$OUT/$gate-k6.log"
k6_status="${PIPESTATUS[0]}"
# shellcheck disable=SC2086
kill $diag_pids 2>/dev/null
curl -fsS "http://127.0.0.1:${SUTRA_PERF_METRICS_PORT:-9464}/metrics" >"$OUT/$gate-api-metrics.txt" 2>/dev/null
psql "$DATABASE_URL" -XAtc "SELECT 'audit_log', count(*) FROM platform.audit_log UNION ALL SELECT 'outbox', count(*) FROM platform.outbox" >"$OUT/$gate-db-counts.txt" 2>&1
set -e
[[ -f "$OUT/$gate-summary.json" ]] || { echo "k6 wrote no summary (exit $k6_status)" >&2; exit 1; }
cmp_status=0
node deploy/perf/compare.mjs "$gate" "$OUT/$gate-summary.json" deploy/perf/baseline.json || cmp_status=$?
if [[ "$k6_status" != 0 || "$cmp_status" != 0 ]]; then
  echo "perf $gate FAILED (k6 exit $k6_status, compare exit $cmp_status)" >&2
  exit 1
fi
echo "ok perf-$gate"
