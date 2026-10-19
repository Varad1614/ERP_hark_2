#!/usr/bin/env bash
# Batch 9 CI stage `dr-drill` — owned by slice 9C (docs/batch9-contracts.md §10.4 / §11,
# docs/runbooks/backup-dr.md). Runs from sutraerp/.
#
#   seed → (search baseline) → stop API → backup → WIPE (databases + cluster roles + search
#   indices) → restore → migrate → row counts / audit heads / append-only audit grants →
#   start API → login → audit chain verify (9A) → Flowable definitions → search rebuild counts
#   → measured RTO (also to $GITHUB_STEP_SUMMARY).
#
# One "ok <check>" line per check, non-zero exit on the first failure, `--dry-run` prints the plan
# and exits 0. Idempotent: every run takes a fresh backup and restores it.
#
# Inputs (all optional except where noted):
#   SUTRA_CI_MODE            compose (default) | kind
#   SUTRA_CI_API_URL         http://localhost:8080
#   SUTRA_CI_DB_URL          postgres://sutra:sutra@localhost:5432/sutra   (superuser of the cluster)
#   SUTRA_CI_FLOWABLE_DB_URL flowable database URL (as the same superuser) → flowable.dump
#   SUTRA_CI_FLOWABLE_URL    http://localhost:8090/flowable-rest   (+ SUTRA_CI_FLOWABLE_USER / _PASSWORD)
#   FLOWABLE_DB_PASSWORD     re-provisions the flowable login role after the wipe (init-db.sh)
#   SUTRA_CI_OPENSEARCH_URL  http://localhost:9200 (empty = skip the search checks)
#   SUTRA_CI_OPENSEARCH_PREFIX  index prefix of the API (default sutra-development)
#   SUTRA_CI_API_START / SUTRA_CI_API_STOP   compose: shell commands that start / stop the API
#   SUTRA_CI_FLOWABLE_STOP / _RESTART       compose: shell commands stopping Flowable before the wipe and
#                                           starting it (healthy) after the restore
#   SUTRA_API_BIN            sutra-api binary for `migrate` (compose; default target/debug/sutra-api)
#   SUTRA_CI_BACKUP_DIR      backup output directory (default $RUNNER_TEMP or /tmp)/sutra-dr
#   kind: KUBECONFIG, SUTRA_CI_NS (sutraerp), SUTRA_CI_RELEASE (sutraerp), SUTRA_CI_CHART
#         (deploy/helm/sutraerp), SUTRA_CI_VALUES (deploy/helm/sutraerp/ci/kind.yaml),
#         SUTRA_CI_PG_LOCAL_PORT (15432) — postgres is port-forwarded and the same scripts run on
#         the runner; the wipe deletes the postgres StatefulSet + PVC and reinstalls the release.
set -euo pipefail

mode=${SUTRA_CI_MODE:-compose}
api=${SUTRA_CI_API_URL:-http://localhost:8080}
ns=${SUTRA_CI_NS:-sutraerp}
release=${SUTRA_CI_RELEASE:-sutraerp}
os_url=${SUTRA_CI_OPENSEARCH_URL:-}
os_prefix=${SUTRA_CI_OPENSEARCH_PREFIX:-sutra-development}
fl_url=${SUTRA_CI_FLOWABLE_URL:-}
fl_auth="${SUTRA_CI_FLOWABLE_USER:-rest-admin}:${SUTRA_CI_FLOWABLE_PASSWORD:-}"
projections=(students staff library-catalogue)

if [[ "${1:-}" == "--dry-run" ]]; then
  cat <<EOF
dr-drill ($mode): api=$api
  1 seed (deploy/ci/seed-pilot.sh when implemented, else API login + non-empty database)
  2 search baseline (rebuild, counts per projection)    [${os_url:-skipped: no SUTRA_CI_OPENSEARCH_URL}]
  3 stop API → deploy/backup/sutra-backup.sh (sutra + flowable dumps, ACLs kept)
  4 wipe: drop databases + sutra_* / flowable roles + search indices  ($mode)
  5 deploy/backup/sutra-restore.sh: checksums → roles.sql → pg_restore → migrate → counts/heads/grants
  6 start API → login → POST /api/v1/platform/audit/chain/verify (skipped until 9A)
  7 Flowable process definitions equal  [${fl_url:-skipped}]; search rebuild counts equal
  8 RTO → \$GITHUB_STEP_SUMMARY
EOF
  exit 0
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
# shellcheck source=../backup/lib.sh
. "$root/deploy/backup/lib.sh"
need curl jq "$PSQL"

pf_pid=""
cleanup() {
  [[ -n "$pf_pid" ]] && kill "$pf_pid" 2>/dev/null || true
  [[ -n "${api_pf_pid:-}" ]] && kill "$api_pf_pid" 2>/dev/null || true
}
trap cleanup EXIT

kind_port_forward() {
  local port=${SUTRA_CI_PG_LOCAL_PORT:-15432} i
  [[ -n "$pf_pid" ]] && kill "$pf_pid" 2>/dev/null || true
  kubectl -n "$ns" port-forward "svc/${release}-postgres" "$port:5432" >/tmp/dr-pf.log 2>&1 &
  pf_pid=$!
  for i in $(seq 1 60); do
    "$PSQL" "$db_admin" -qAtX -c 'SELECT 1' >/dev/null 2>&1 && return 0
    sleep 2
  done
  cat /tmp/dr-pf.log >&2
  die "postgres port-forward did not come up"
}

if [[ "$mode" == kind ]]; then
  need kubectl helm
  pg_pass=${SUTRA_CI_PG_PASSWORD:-sutra}
  db=${SUTRA_CI_DB_URL:-postgres://sutra:${pg_pass}@127.0.0.1:${SUTRA_CI_PG_LOCAL_PORT:-15432}/sutra}
else
  db=${SUTRA_CI_DB_URL:-postgres://sutra:sutra@localhost:5432/sutra}
fi
db_admin=$(url_with_db "$db" postgres)
fdb=${SUTRA_CI_FLOWABLE_DB_URL:-}
[[ "$mode" == kind ]] && kind_port_forward

wait_ready() {
  local i
  for i in $(seq 1 90); do
    curl -fsS "$api/health/ready" >/dev/null 2>&1 && return 0
    sleep 2
  done
  [[ -s /tmp/dr-pf-api.log ]] && cat /tmp/dr-pf-api.log >&2
  [[ "$mode" == kind ]] && kubectl -n "$ns" get pods -o wide >&2 || true
  die "API not ready at $api/health/ready"
}

jar=$(mktemp)
csrf=""
login() { # admin dev login → cookie jar + $csrf
  local t body
  rm -f "$jar"
  t=$(curl -fsS -c "$jar" -b "$jar" "$api/api/v1/auth/csrf" | jq -r .csrfToken)
  body=$(curl -fsS -c "$jar" -b "$jar" -H 'content-type: application/json' -H "x-csrf-token: $t" \
    -d "$(jq -cn --arg p "${SUTRA_DEV_AUTH_PASSWORD:?set SUTRA_DEV_AUTH_PASSWORD}" '{loginId:"admin@sutra.dev",password:$p}')" \
    "$api/api/v1/auth/login") \
    || die "admin login failed"
  csrf=$(jq -r .csrfToken <<<"$body")
  [[ -n "$csrf" && "$csrf" != null ]] || die "login returned no csrfToken: $body"
}

api_stop() {
  if [[ "$mode" == kind ]]; then
    kubectl -n "$ns" scale "deploy/${release}-api" --replicas=0 >/dev/null
    kubectl -n "$ns" rollout status "deploy/${release}-api" --timeout=120s >/dev/null || true
  elif [[ -n "${SUTRA_CI_API_STOP:-}" ]]; then
    bash -c "$SUTRA_CI_API_STOP"
  else
    log "SUTRA_CI_API_STOP not set: backing up a live API (the snapshot keeps the manifest consistent)"
  fi
}
api_pf_pid=""
# INT: a `kubectl port-forward svc/…` is pinned to the pod it picked, so the forward to $api (opened by
# deploy/kind/up.sh) dies with the api pod that api_stop scaled away; re-open it on the same port.
kind_api_port_forward() {
  local port
  port=$(sed -E 's#^[a-z]+://[^:/]+:([0-9]+).*#\1#' <<<"$api")
  [[ "$port" =~ ^[0-9]+$ ]] || port=8080
  pkill -f "port-forward svc/${release}-api ${port}:" 2>/dev/null || true
  [[ -n "$api_pf_pid" ]] && kill "$api_pf_pid" 2>/dev/null || true
  sleep 1
  kubectl -n "$ns" port-forward "svc/${release}-api" "${port}:8080" >/tmp/dr-pf-api.log 2>&1 &
  api_pf_pid=$!
}
api_start() {
  if [[ "$mode" == kind ]]; then
    kubectl -n "$ns" scale "deploy/${release}-api" --replicas=1 >/dev/null
    kubectl -n "$ns" rollout status "deploy/${release}-api" --timeout=300s >/dev/null
    kind_api_port_forward
  elif [[ -n "${SUTRA_CI_API_START:-}" ]]; then
    bash -c "$SUTRA_CI_API_START"
  fi
  wait_ready
}

# Waits until every projection finished a rebuild after $1 (DB timestamp); prints "proj=count …".
wait_rebuild() {
  local since=$1 i done_n p out=""
  for i in $(seq 1 120); do
    done_n=$(q "$db" -v s="$since" <<'SQL'
SELECT count(*) FROM platform.search_checkpoint
 WHERE projection IN ('students', 'staff', 'library-catalogue')
   AND rebuild_requested_at IS NULL AND last_rebuild_at >= (:'s')::timestamptz;
SQL
)
    [[ "$done_n" == 3 ]] && break
    [[ $i -eq 120 ]] && die "search rebuild did not finish (done $done_n/3)"
    sleep 2
  done
  for p in "${projections[@]}"; do
    local c n
    c=$(q "$db" -v p="$p" <<'SQL'
SELECT last_rebuild_count FROM platform.search_checkpoint WHERE projection = :'p';
SQL
)
    curl -fsS -X POST "$os_url/${os_prefix}-${p}/_refresh" >/dev/null
    n=$(curl -fsS "$os_url/${os_prefix}-${p}/_count" | jq -r .count)
    [[ "$c" == "$n" ]] || die "search $p: rebuild wrote $c documents but the alias holds $n"
    out+="$p=$n "
  done
  printf '%s' "${out% }"
}

request_rebuild() { # → DB timestamp of the request
  q "$db" <<'SQL'
INSERT INTO platform.search_checkpoint (projection, rebuild_requested_at)
SELECT p, now() FROM unnest(ARRAY['students', 'staff', 'library-catalogue']) p
ON CONFLICT (projection) DO UPDATE SET rebuild_requested_at = now(), updated_at = now();
SELECT now();
SQL
}

flowable_defs() {
  curl -fsS -u "$fl_auth" "$fl_url/service/repository/process-definitions?size=1" | jq -r .total
}

# ---- 1. seed ---------------------------------------------------------------------------------
wait_ready
ok "api ready ($mode)"
if [[ -x "$root/deploy/ci/seed-pilot.sh" ]] && ! grep -q 'not implemented yet' "$root/deploy/ci/seed-pilot.sh"; then
  # INT: the drill proves data survives, not which dev tenant the API runs as — the pilot seed
  # is tenant-strict only in its own workflow (where the API runs with the pilot tenant).
  PILOT_STRICT_TENANT=${PILOT_STRICT_TENANT:-false} bash "$root/deploy/ci/seed-pilot.sh"
  ok "seed (pilot seed)"
else
  login
  ok "seed (minimal API seed: admin session; pilot seed not present)"
fi
rows=$(q "$db" -c "SELECT (SELECT count(*) FROM platform.audit_log) + (SELECT count(*) FROM platform.outbox);")
[[ "$rows" -gt 0 ]] || die "database has no audit / outbox rows to back up"
ok "seed data present ($rows audit + outbox rows)"

# ---- 2. search baseline ----------------------------------------------------------------------
if [[ -n "$os_url" ]]; then
  t0=$(request_rebuild | tail -1)
  search_before=$(wait_rebuild "$t0")
  ok "search baseline ($search_before)"
fi
if [[ -n "$fl_url" ]]; then
  fl_before=$(flowable_defs)
  [[ "$fl_before" -gt 0 ]] || die "Flowable has no process definitions before the backup"
  ok "flowable baseline ($fl_before process definitions)"
fi
grants_before=$(q "$db" -c "$(audit_grants_sql)")

# ---- 3. backup -------------------------------------------------------------------------------
api_stop
ok "api stopped"
bdir=${SUTRA_CI_BACKUP_DIR:-${RUNNER_TEMP:-/tmp}/sutra-dr}
backup=$(SUTRA_BACKUP_DATABASE_URL=$db SUTRA_BACKUP_FLOWABLE_DATABASE_URL=$fdb \
  SUTRA_ENV=${SUTRA_ENV:-development} SUTRA_GIT_SHA=${SUTRA_GIT_SHA:-${GITHUB_SHA:-local}} \
  bash "$root/deploy/backup/sutra-backup.sh" --out-dir "$bdir" --tar | tee /dev/stderr | tail -1)
[[ -f "$backup" ]] || die "backup archive not found: $backup"
ok "backup $(basename "$backup") ($(file_bytes "$backup") bytes)"

# ---- 4. wipe ---------------------------------------------------------------------------------
rto_start=$(date +%s)
[[ -n "${SUTRA_CI_FLOWABLE_STOP:-}" ]] && bash -c "$SUTRA_CI_FLOWABLE_STOP" >/dev/null
sutra_db=$(db_name_of "$db")
if [[ "$mode" == kind ]]; then
  kubectl -n "$ns" delete statefulset "${release}-postgres" --wait=true
  kubectl -n "$ns" delete pvc -l "app.kubernetes.io/component=postgres" --wait=true || true
  kubectl -n "$ns" wait --for=delete pod -l "app.kubernetes.io/component=postgres" --timeout=120s || true
  helm upgrade "$release" "${SUTRA_CI_CHART:-deploy/helm/sutraerp}" -n "$ns" --reuse-values \
    ${SUTRA_CI_VALUES:+-f "$SUTRA_CI_VALUES"} --wait --timeout 10m >/dev/null
  api_stop
  kind_port_forward
  ok "wipe (postgres StatefulSet + PVC deleted, release reinstalled)"
else
  q "$db_admin" -v d="$sutra_db" <<'SQL' >/dev/null
SELECT format('DROP DATABASE IF EXISTS %I WITH (FORCE)', :'d') \gexec
SQL
  if [[ -n "$fdb" ]]; then
    q "$db_admin" -v d="$(db_name_of "$fdb")" <<'SQL' >/dev/null
SELECT format('DROP DATABASE IF EXISTS %I WITH (FORCE)', :'d') \gexec
SQL
  fi
  # Cluster roles go too, so the restore has to re-create them (roles.sql) for the ACLs to apply.
  dropped=$(q "$db_admin" <<'SQL'
SELECT format('DROP ROLE IF EXISTS %I', rolname) FROM pg_roles
 WHERE (rolname LIKE 'sutra\_%' AND rolname <> current_user) OR rolname = 'flowable'
 ORDER BY (rolname = 'sutra_app') DESC, rolname \gexec
SELECT count(*) FROM pg_roles WHERE (rolname LIKE 'sutra\_%' AND rolname <> current_user) OR rolname = 'flowable';
SQL
)
  [[ "$(tail -1 <<<"$dropped")" == 0 ]] || die "roles survived the wipe: $dropped"
  left=$(q "$db_admin" -v d="$sutra_db" -c "SELECT count(*) FROM pg_database WHERE datname = '$sutra_db';")
  [[ "$left" == 0 ]] || die "database $sutra_db still exists"
  ok "wipe (databases dropped, sutra_* + flowable roles dropped)"
fi
if [[ -n "$os_url" ]]; then
  for p in "${projections[@]}"; do
    curl -sS -o /dev/null -X DELETE "$os_url/${os_prefix}-${p}-v1" || true
  done
  ok "wipe search indices (${os_prefix}-*)"
fi

# ---- 5. restore ------------------------------------------------------------------------------
if [[ -n "$fdb" && -n "${FLOWABLE_DB_PASSWORD:-}" ]]; then
  SUTRA_FLOWABLE_ADMIN_URL=$db_admin FLOWABLE_DB_NAME=$(db_name_of "$fdb") \
    bash "$root/deploy/flowable/init-db.sh"
fi
restore_args=()
# Rebuild requests made by the restore are newer than this (the API is stopped until step 6).
if [[ "$mode" == kind ]]; then
  # The reinstalled release ran its migrations hook; migrate again through a fresh hook run below.
  restore_args+=(--skip-migrate)
fi
SUTRA_BACKUP_DATABASE_URL=$db SUTRA_BACKUP_FLOWABLE_DATABASE_URL=$fdb \
  SUTRA_API_BIN=${SUTRA_API_BIN:-$root/target/debug/sutra-api} \
  bash "$root/deploy/backup/sutra-restore.sh" "$backup" "${restore_args[@]}"
if [[ "$mode" == kind ]]; then
  helm upgrade "$release" "${SUTRA_CI_CHART:-deploy/helm/sutraerp}" -n "$ns" --reuse-values \
    ${SUTRA_CI_VALUES:+-f "$SUTRA_CI_VALUES"} --wait --timeout 10m >/dev/null
  ok "migrate (helm upgrade migrations hook)"
fi
t1=$(q "$db" -c "SELECT now();")
grants_after=$(q "$db" -c "$(audit_grants_sql)")
[[ "$grants_before" == "$grants_after" ]] || {
  diff <(echo "$grants_before") <(echo "$grants_after") >&2 || true
  die "audit-table privileges differ after the restore"
}
ok "audit grants equal before / after ($(wc -l <<<"$grants_after") facts)"

# ---- 6. API back ------------------------------------------------------------------------------
[[ -n "${SUTRA_CI_FLOWABLE_RESTART:-}" ]] && bash -c "$SUTRA_CI_FLOWABLE_RESTART"
api_start
rto=$(( $(date +%s) - rto_start ))
ok "api ready after restore (RTO ${rto}s)"
login
ok "login after restore"
code=$(curl -s -o /tmp/dr-verify.json -w '%{http_code}' -b "$jar" -c "$jar" -X POST \
  -H 'content-type: application/json' -H "x-csrf-token: $csrf" -H "idempotency-key: $(cat /proc/sys/kernel/random/uuid)" \
  -d '{}' "$api/api/v1/platform/audit/chain/verify")
case "$code" in
  200) jq -e '.ok == true' /tmp/dr-verify.json >/dev/null \
         || die "audit chain verify failed: $(cat /tmp/dr-verify.json)"
       ok "audit chain verify" ;;
  404|405) ok "audit chain verify (skipped: no 9A)" ;;
  *) die "audit chain verify HTTP $code: $(head -c 400 /tmp/dr-verify.json)" ;;
esac

# ---- 7. Flowable + search ----------------------------------------------------------------------
if [[ -n "$fl_url" ]]; then
  fl_after=""
  for i in $(seq 1 60); do
    fl_after=$(flowable_defs 2>/dev/null || true)
    [[ -n "$fl_after" ]] && break
    sleep 3
  done
  [[ "$fl_after" == "$fl_before" ]] || die "Flowable process definitions: before $fl_before, after ${fl_after:-unreachable}"
  ok "flowable restored ($fl_after process definitions)"
fi
if [[ -n "$os_url" ]]; then
  # sutra-restore.sh requested the rebuild while the API was down; it ran after $t1.
  search_after=$(wait_rebuild "$t1")
  [[ "$search_after" == "$search_before" ]] || die "search rebuild counts: before [$search_before], after [$search_after]"
  ok "search rebuild equals the baseline ($search_after)"
fi
total=$(( $(date +%s) - rto_start ))

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### DR drill ($mode)"
    echo
    echo "| Measure | Value |"
    echo "|---|---|"
    echo "| Backup | \`$(basename "$backup")\` ($(file_bytes "$backup") bytes) |"
    echo "| RTO (wipe → API ready, verified) | ${rto}s |"
    echo "| Wipe → search rebuilt | ${total}s |"
    echo "| Target RTO | 7200s |"
    [[ -n "$os_url" ]] && echo "| Search documents | $search_after |"
    [[ -n "$fl_url" ]] && echo "| Flowable process definitions | $fl_after |"
  } >>"$GITHUB_STEP_SUMMARY"
fi
ok "dr-drill complete (RTO ${rto}s, target 7200s)"
