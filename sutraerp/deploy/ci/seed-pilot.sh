#!/usr/bin/env bash
# Batch 9 CI stage `seed-pilot` — owned by slice 9E (docs/batch9-contracts.md §10.4, §12.4).
# Seeds the pilot institution (deploy/pilot/seed-pilot.mjs) against SUTRA_CI_API_URL, runs it a
# second time to prove it is idempotent (no changes), then reads the pilot back through the API.
#
# Runs from sutraerp/. Inputs:
#   SUTRA_CI_API_URL   API base URL (default http://localhost:8080)
#   SUTRA_CI_MODE      compose (default; DATABASE_URL reachable from here) | kind (psql runs
#                      inside the bundled PostgreSQL pod through kubectl exec)
#   DATABASE_URL       schema owner (compose; default postgres://sutra:sutra@localhost:5432/sutra)
#   SUTRA_CI_NS        namespace in kind mode (default sutraerp); KUBECONFIG as usual
#   SUTRA_CI_PG_TARGET kubectl exec target in kind mode (default statefulset/sutraerp-postgres)
#   SUTRA_CI_PG_USER / SUTRA_CI_PG_DB  role + database inside the pod (default sutra / sutra)
#   PILOT_STRICT_TENANT  default true in compose (the API must run with the pilot tenant as its dev
#                      tenant), false in kind (the chart's dev tenant is seeded instead)
# Output: one "ok <check>" line per check; non-zero exit on the first failure; --dry-run prints the
# plan and exits 0. Idempotent.
set -euo pipefail

API_URL="${SUTRA_CI_API_URL:-http://localhost:8080}"
MODE="${SUTRA_CI_MODE:-compose}"
NS="${SUTRA_CI_NS:-sutraerp}"
PILOT_TENANT="${PILOT_TENANT_ID:-0192f5a0-7c3e-7000-8000-000000009110}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ "${1:-}" == "--dry-run" ]]; then
  echo "seed-pilot (dry run): mode=$MODE api=$API_URL tenant=$PILOT_TENANT"
  echo "  1. node deploy/pilot/seed-pilot.mjs            (seed PILOT: institution, 50 students, 3 staff, fees, library, hostel)"
  echo "  2. node deploy/pilot/seed-pilot.mjs again      (PILOT_EXPECT_NO_CHANGES=1: must change nothing)"
  echo "  3. API reads: institution active, students, staff, fee structure, library + hostel policies"
  exit 0
fi

command -v node >/dev/null || { echo "seed-pilot: node is required" >&2; exit 1; }
command -v curl >/dev/null || { echo "seed-pilot: curl is required" >&2; exit 1; }
command -v jq >/dev/null || { echo "seed-pilot: jq is required" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ "$MODE" == "kind" ]]; then
  # psql shim: the harness calls `psql <url> -qtAX -v ON_ERROR_STOP=1 -c <sql>`; run it in the pod.
  PG_TARGET="${SUTRA_CI_PG_TARGET:-statefulset/sutraerp-postgres}"
  mkdir -p "$WORK/bin"
  cat >"$WORK/bin/psql" <<EOF
#!/usr/bin/env bash
shift # the connection URL (only meaningful outside the cluster)
exec kubectl -n "$NS" exec -i "$PG_TARGET" -c postgres -- psql -U "${SUTRA_CI_PG_USER:-sutra}" -d "${SUTRA_CI_PG_DB:-sutra}" "\$@"
EOF
  chmod +x "$WORK/bin/psql"
  export PATH="$WORK/bin:$PATH"
  export PILOT_STRICT_TENANT="${PILOT_STRICT_TENANT:-false}"
  export DATABASE_URL="${DATABASE_URL:-postgres://in-cluster/sutra}"
else
  command -v psql >/dev/null || { echo "seed-pilot: psql is required" >&2; exit 1; }
  export PILOT_STRICT_TENANT="${PILOT_STRICT_TENANT:-true}"
  export DATABASE_URL="${DATABASE_URL:-postgres://sutra:sutra@localhost:5432/sutra}"
fi
export E2E_API_URL="$API_URL"
export PILOT_TENANT_ID="$PILOT_TENANT"

# ---- 1. seed
rc=0
node "$HERE/deploy/pilot/seed-pilot.mjs" >"$WORK/first.log" 2>&1 || rc=$?
grep -E '^(ok |changes=)' "$WORK/first.log" | sed 's/^ok /ok seed: /' || true
[[ "$rc" == "0" ]] || { echo "seed-pilot: first run failed (exit $rc)" >&2; cat "$WORK/first.log" >&2; exit 1; }
INST="$(sed -n 's/^PILOT_INSTITUTION_ID=//p' "$WORK/first.log" | tail -1)"
[[ -n "$INST" ]] || { echo "seed-pilot: no institution id reported" >&2; exit 1; }
echo "ok seed-first-run ($(grep -c '^ok ' "$WORK/first.log") checks, $(sed -n 's/^changes=//p' "$WORK/first.log"))"

# ---- 2. idempotency
PILOT_EXPECT_NO_CHANGES=1 node "$HERE/deploy/pilot/seed-pilot.mjs" >"$WORK/second.log" 2>&1 || {
  echo "seed-pilot: second run changed data or failed" >&2
  cat "$WORK/second.log" >&2
  exit 1
}
grep -q '^changes=0$' "$WORK/second.log" || { echo "seed-pilot: second run reported changes" >&2; cat "$WORK/second.log" >&2; exit 1; }
echo "ok seed-idempotent (second run: changes=0)"

# ---- 3. reads through the API (dev login as the administrator)
JAR="$WORK/cookies"
CSRF="$(curl -fsS -c "$JAR" "$API_URL/api/v1/auth/csrf" | jq -r .csrfToken)"
LOGIN="$(curl -fsS -b "$JAR" -c "$JAR" -H 'content-type: application/json' -H "x-csrf-token: $CSRF" \
  -d '{"loginId":"admin@sutra.dev","password":"sutra"}' "$API_URL/api/v1/auth/login")"
TENANT="$(echo "$LOGIN" | jq -r '.principal.scope.tenantId // empty')"
get() { curl -fsS -b "$JAR" "$API_URL$1"; }

STATUS="$(get "/api/v1/institution/institutions/$INST" | jq -r .status)"
CODE="$(get "/api/v1/institution/institutions/$INST" | jq -r .code)"
[[ "$STATUS" == "active" && "$CODE" == "PILOT" ]] || { echo "seed-pilot: institution $INST is $CODE/$STATUS" >&2; exit 1; }
echo "ok read-institution PILOT active ($INST)"
if [[ "$PILOT_STRICT_TENANT" == "true" && "$TENANT" != "$PILOT_TENANT" ]]; then
  echo "seed-pilot: API tenant $TENANT is not the pilot tenant $PILOT_TENANT" >&2
  exit 1
fi
echo "ok read-tenant ${TENANT:-unknown}"

STUDENTS="$(get "/api/v1/student/institutions/$INST/students?limit=200" | jq '[.items[] | select(.status == "active")] | length')"
[[ "$STUDENTS" -ge 50 ]] || { echo "seed-pilot: $STUDENTS active students (want 50)" >&2; exit 1; }
echo "ok read-students $STUDENTS"

STAFF="$(get "/api/v1/people/institutions/$INST/staff?limit=200" | jq '[.items[] | select(.status == "active")] | length')"
[[ "$STAFF" -ge 3 ]] || { echo "seed-pilot: $STAFF active staff (want 3)" >&2; exit 1; }
echo "ok read-staff $STAFF"

FEES="$(get "/api/v1/finance/institutions/$INST/fee-structures" | jq -r '[.items[] | select(.code == "PILOT-Y1" and .status == "published")] | length')"
[[ "$FEES" == "1" ]] || { echo "seed-pilot: fee structure PILOT-Y1 not published" >&2; exit 1; }
echo "ok read-fee-structure PILOT-Y1 published"

LIB="$(get "/api/v1/campus/institutions/$INST/library-policies" | jq -r '[.items[] | select(.status == "active")] | length')"
HOSTEL="$(get "/api/v1/campus/institutions/$INST/hostel-policies" | jq -r '[.items[] | select(.status == "active")] | length')"
[[ "$LIB" == "1" && "$HOSTEL" == "1" ]] || { echo "seed-pilot: library policies active=$LIB hostel=$HOSTEL" >&2; exit 1; }
echo "ok read-library-and-hostel policies active"

METRICS="$(get "/api/v1/services/institutions/$INST/metrics" | jq -r '[.items[] | select(.key == "student.active_students")][0].value // empty')"
[[ -n "$METRICS" && "${METRICS%%.*}" -ge 50 ]] || { echo "seed-pilot: metrics report $METRICS active students" >&2; exit 1; }
echo "ok read-metrics student.active_students=$METRICS"
