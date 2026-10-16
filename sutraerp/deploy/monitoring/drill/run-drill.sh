#!/usr/bin/env bash
# SutraERP monitoring fault-injection drill (Part 2 S5-monitor).
#
# Injects real faults into a running stack (compose.yaml + compose.drill.yaml, the API on this
# host) and asserts that the matching alert FIRES in Prometheus, REACHES Alertmanager and is
# DELIVERED (Mailpit e-mail / webhook sink). Writes measured times to $DRILL_OUT/drill.jsonl.
#
# Env: MON_DIR (deploy/monitoring of the installed bundle), DRILL_OUT, API_URL, MINIO_PID,
#      COMPOSE (the docker compose command line incl. -f files), TEXTFILE_DIR.
set -euo pipefail

PROM="${PROM_URL:-http://localhost:9090}"
AM="${AM_URL:-http://localhost:9093}"
SINK="${SINK_URL:-http://localhost:8000}"
MAILPIT="${MAILPIT_URL:-http://localhost:8025}"
GRAFANA="${GRAFANA_URL:-http://localhost:3000}"
API_URL="${API_URL:-http://localhost:8080}"
OUT="${DRILL_OUT:?}"
TEXTFILE_DIR="${TEXTFILE_DIR:?}"
FIRE_TIMEOUT="${FIRE_TIMEOUT:-240}"
read -r -a DC <<< "${COMPOSE:?}"
mkdir -p "$OUT/backup"
: > "$OUT/drill.jsonl"

log() { echo "[$(date -u +%H:%M:%S)] $*"; }
now() { date +%s; }
q() { curl -fsS --get "$PROM/api/v1/query" --data-urlencode "query=$1" | jq -r '.data.result | length'; }

prom_firing() { # alertname -> 0 when firing in Prometheus
  curl -fsS "$PROM/api/v1/alerts" | jq -e --arg n "$1" \
    '[.data.alerts[] | select(.labels.alertname == $n and .state == "firing")] | length > 0' >/dev/null
}
am_active() { # alertname -> 0 when Alertmanager holds it active
  curl -fsS --get "$AM/api/v2/alerts" --data-urlencode "filter=alertname=\"$1\"" --data-urlencode "active=true" \
    | jq -e 'length > 0' >/dev/null
}

# expect_alert <alertname> <fault> <t0>: waits for Prometheus firing, then Alertmanager.
expect_alert() {
  local name="$1" fault="$2" t0="$3" tp="" ta=""
  local deadline=$(( $(now) + FIRE_TIMEOUT ))
  while [ "$(now)" -lt "$deadline" ]; do
    if [ -z "$tp" ] && prom_firing "$name"; then tp="$(now)"; log "  $name FIRING in Prometheus after $((tp - t0)) s"; fi
    if [ -n "$tp" ] && am_active "$name"; then ta="$(now)"; log "  $name ACTIVE in Alertmanager after $((ta - t0)) s"; break; fi
    sleep 2
  done
  if [ -z "$ta" ]; then
    log "::error::$name did not fire for fault '$fault' within ${FIRE_TIMEOUT}s"
    curl -fsS "$PROM/api/v1/alerts" > "$OUT/alerts-at-failure.json" || true
    return 1
  fi
  jq -nc --arg alert "$name" --arg fault "$fault" --argjson prom "$((tp - t0))" --argjson am "$((ta - t0))" \
    '{alert: $alert, fault: $fault, seconds_to_prometheus_firing: $prom, seconds_to_alertmanager: $am}' \
    >> "$OUT/drill.jsonl"
}

expect_quiet() {
  for n in "$@"; do
    if prom_firing "$n"; then log "::error::$n is firing before its fault was injected"; return 1; fi
  done
}

backup() { # the drill's logical backup, wrapped by the metrics writer
  "$MON_DIR/textfile/sutra-backup-metrics.sh" --kind logical --dir "$TEXTFILE_DIR" --size-of "$OUT/backup/sutra.dump" \
    -- bash -c "${DC[*]} exec -T postgres pg_dump -U sutra -Fc sutra > '$OUT/backup/sutra.dump'"
}

# ------------------------------------------------------------------ 0. baseline
log "baseline: scrape targets"
for i in $(seq 1 60); do
  down="$(curl -fsS "$PROM/api/v1/targets" | jq -r '[.data.activeTargets[] | select(.health != "up") | .labels.job] | join(",")')"
  [ -z "$down" ] && break
  [ "$i" = 60 ] && { log "::error::targets not up: $down"; curl -fsS "$PROM/api/v1/targets" > "$OUT/targets.json"; exit 1; }
  sleep 3
done
curl -fsS "$PROM/api/v1/targets" | jq '[.data.activeTargets[] | {job: .labels.job, instance: .labels.instance, health}]' \
  > "$OUT/targets.json"
log "first backup (success)"
backup
log "baseline: required metric families"
missing=()
for m in pg_up pg_stat_archiver_failed_count pg_stat_archiver_archived_count pg_stat_activity_count \
         pg_settings_max_connections pg_stat_statements_calls_total pg_stat_user_tables_n_dead_tup \
         pg_stat_activity_max_tx_duration pg_database_size_bytes pg_stat_database_deadlocks \
         node_filesystem_avail_bytes node_cpu_seconds_total node_memory_MemAvailable_bytes \
         sutra_backup_last_success_timestamp_seconds sutra_backup_last_run_success \
         sutra_http_requests_total sutra_http_requests_in_flight sutra_security_events_total \
         sutra_integration_calls_total sutra_idp_sync_links sutra_gateway_payments_open_stale \
         sutra_outbox_pending sutra_job_runs_total sutra_process_start_time_seconds; do
  ok=""
  for i in $(seq 1 30); do [ "$(q "count($m)")" != "0" ] && { ok=1; break; }; sleep 2; done
  [ -n "$ok" ] || missing+=("$m")
done
nats_families="$(curl -fsS "$PROM/api/v1/label/__name__/values" | jq -r '[.data[] | select(test("^(gnatsd|jetstream)_"))] | length')"
log "NATS exporter families: $nats_families"
[ "${nats_families:-0}" -gt 0 ] || missing+=("gnatsd_*/jetstream_*")
if [ "${#missing[@]}" -gt 0 ]; then log "::error::missing metric families: ${missing[*]}"; exit 1; fi
curl -fsS "$PROM/api/v1/label/__name__/values" | jq -r '.data[]' > "$OUT/metric-names.txt"
log "baseline: every rule evaluates"
bad_rules="$(curl -fsS "$PROM/api/v1/rules" | jq -r '[.data.groups[].rules[] | select(.health != "ok") | .name] | join(",")')"
[ -z "$bad_rules" ] || { log "::error::rules unhealthy: $bad_rules"; exit 1; }
curl -fsS "$PROM/api/v1/rules" | jq '{groups: [.data.groups[] | {name, rules: (.rules | length)}]}' > "$OUT/rules.json"
log "baseline: Grafana dashboards provisioned"
uids="$(curl -fsS -u "admin:${SUTRA_GRAFANA_ADMIN_PASSWORD:?}" "$GRAFANA/api/search?type=dash-db" | jq -r '[.[].uid] | sort | join(",")')"
log "  dashboards: $uids"
for u in sutra-postgres sutra-security sutra-queues-integrations sutra-host-backups sutra-api-red sutra-database \
         sutra-outbox-jobs sutra-audit-chain sutra-workflow-search; do
  case ",$uids," in *",$u,"*) ;; *) log "::error::dashboard $u not provisioned"; exit 1 ;; esac
done
# A dashboard query answers through Grafana's datasource proxy.
curl -fsS -u "admin:${SUTRA_GRAFANA_ADMIN_PASSWORD}" --get "$GRAFANA/api/datasources/proxy/uid/prometheus/api/v1/query" \
  --data-urlencode 'query=sum(pg_up)' | jq -e '.data.result[0].value[1] == "1"' >/dev/null \
  || { log "::error::Grafana → Prometheus query failed"; exit 1; }
expect_quiet SutraWebhookSignatureFailures SutraS3Errors SutraWalArchiveFailing SutraPostgresDown SutraBackupFailed
log "baseline: the watchdog reaches Alertmanager (heartbeat path)"
expect_alert SutraWatchdog "heartbeat (always firing)" "$(now)"

# ------------------------------------------------------------------ 1. webhook signature
log "FAULT 1: payment webhooks with a wrong signature"
t0="$(now)"
for i in 1 2 3 4 5 6; do
  code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_URL/api/v1/finance/gateway/webhook" \
    -H 'content-type: application/json' -H 'X-Razorpay-Signature: 00deadbeef' \
    --data '{"event":"payment.captured","payload":{}}')"
  [ "$code" = 401 ] || { log "::error::bad-signature webhook answered $code (want 401)"; exit 1; }
done
expect_alert SutraWebhookSignatureFailures "6 webhooks with a wrong X-Razorpay-Signature (401)" "$t0"

# ------------------------------------------------------------------ 2. S3 blocked
log "FAULT 2: object storage blocked (MinIO process frozen with SIGSTOP; requests time out)"
kill -STOP "${MINIO_PID:?}"
t0="$(now)"
( while :; do curl -s -o /dev/null --max-time 5 "$API_URL/health/ready" || true; sleep 1; done ) &
poll=$!
rc=0
expect_alert SutraS3Errors "MinIO frozen (SIGSTOP), /health/ready probes the bucket" "$t0" || rc=$?
kill "$poll" 2>/dev/null || true
kill -CONT "$MINIO_PID"
[ "$rc" = 0 ] || exit "$rc"
curl -fsS --get "$PROM/api/v1/query" --data-urlencode 'query=sum by (outcome) (sutra_integration_calls_total{integration="s3"})' \
  | jq -c '.data.result | map({(.metric.outcome): .value[1]}) | add' > "$OUT/s3-calls.json"

# ------------------------------------------------------------------ 3. WAL archive failure
log "FAULT 3: WAL archiving fails (archive_command=/bin/false)"
psqlc() { "${DC[@]}" exec -T postgres psql -U sutra -d sutra -v ON_ERROR_STOP=1 -qtAc "$1"; }
psqlc "ALTER SYSTEM SET archive_command = '/bin/false'" && psqlc "SELECT pg_reload_conf()" >/dev/null
t0="$(now)"
psqlc "CREATE TABLE IF NOT EXISTS public.drill_wal (n int); INSERT INTO public.drill_wal SELECT generate_series(1, 20000)"
psqlc "SELECT pg_switch_wal()" >/dev/null
rc=0
expect_alert SutraWalArchiveFailing "archive_command fails for a switched WAL segment" "$t0" || rc=$?
psqlc "ALTER SYSTEM RESET archive_command" && psqlc "SELECT pg_reload_conf()" >/dev/null
psqlc "DROP TABLE public.drill_wal"
[ "$rc" = 0 ] || exit "$rc"

# ------------------------------------------------------------------ 4. PostgreSQL stopped + backup fails
log "FAULT 4: PostgreSQL stopped; the scheduled backup runs and fails"
t0="$(now)"
"${DC[@]}" stop postgres
if backup; then log "::error::backup succeeded with PostgreSQL stopped"; exit 1; fi
rc=0
expect_alert SutraPostgresDown "docker compose stop postgres" "$t0" || rc=$?
expect_alert SutraBackupFailed "backup run while PostgreSQL is down (pg_dump fails)" "$t0" || rc=$?
code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$API_URL/health/ready" || true)"
log "  API /health/ready with PostgreSQL down: $code"
jq -nc --arg code "$code" '{check: "api readiness while postgres down", http_status: $code}' >> "$OUT/drill.jsonl"
"${DC[@]}" start postgres
for i in $(seq 1 40); do "${DC[@]}" exec -T postgres pg_isready -U sutra -d sutra >/dev/null 2>&1 && break; sleep 2; done
[ "$rc" = 0 ] || exit "$rc"
log "recovery: backup succeeds again; SutraBackupFailed and SutraPostgresDown resolve"
backup
for i in $(seq 1 60); do
  prom_firing SutraBackupFailed || prom_firing SutraPostgresDown || break
  sleep 3
done
expect_quiet SutraBackupFailed SutraPostgresDown

# ------------------------------------------------------------------ 5. delivery
log "delivery: e-mail (Mailpit stands in for the college SMTP relay) and webhooks (chat / SMS / dead-man)"
deliver_deadline=$(( $(now) + 120 ))
mail_ok=""; hook_ok=""
while [ "$(now)" -lt "$deliver_deadline" ]; do
  curl -fsS "$MAILPIT/api/v1/messages?limit=200" > "$OUT/mailpit.json" || true
  curl -fsS "$SINK/received" > "$OUT/webhooks.json" || true
  jq -e '[.messages[]?.Subject | select(test("SutraWebhookSignatureFailures"))] | length > 0' "$OUT/mailpit.json" >/dev/null 2>&1 && mail_ok=1
  jq -e '[.[] | select(.path == "/sms") | .body.alerts[]? | select(.labels.alertname == "SutraPostgresDown")] | length > 0' \
    "$OUT/webhooks.json" >/dev/null 2>&1 \
    && jq -e '[.[] | select(.path == "/deadman") | .body.alerts[]? | select(.labels.alertname == "SutraWatchdog")] | length > 0' \
    "$OUT/webhooks.json" >/dev/null 2>&1 && hook_ok=1
  [ -n "$mail_ok" ] && [ -n "$hook_ok" ] && break
  sleep 5
done
[ -n "$mail_ok" ] || { log "::error::no alert e-mail for SutraWebhookSignatureFailures in Mailpit"; exit 1; }
[ -n "$hook_ok" ] || { log "::error::SMS / dead-man webhooks not delivered"; exit 1; }
jq -c '[.messages[] | .Subject]' "$OUT/mailpit.json" > "$OUT/mail-subjects.json"
jq -c '[.[] | {path, alerts: [.body.alerts[]? | .labels.alertname + ":" + .status]}]' "$OUT/webhooks.json" > "$OUT/webhook-summary.json"
log "delivered: $(jq -r 'length' "$OUT/mail-subjects.json") e-mails, $(jq -r 'length' "$OUT/webhooks.json") webhook posts"
jq -s '.' "$OUT/drill.jsonl" > "$OUT/drill.json"
log "DRILL PASSED"; cat "$OUT/drill.json"
