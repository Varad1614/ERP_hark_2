#!/usr/bin/env bash
# Batch 9 CI stage `check-telemetry` — owned by slice 9A (docs/batch9-contracts.md §3, §4, §10.4).
#
# Proves, against a running API and the LGTM stack:
#   ok api-ready            /health/ready answers 200
#   ok dev-sign-in          CSRF + dev login (every request carries one known W3C traceparent)
#   ok metrics-names        the API's /metrics lists every contract family (§3.3)
#   ok tempo-trace          Tempo has the trace of that traceparent with an `http.route` server span
#   ok prometheus-http      Prometheus scraped sutra_http_requests_total > 0
#   ok loki-trace-log       Loki has a log line carrying that trace_id
#   ok audit-chain-sealed   the sealer moved the tenant's chain head (headSeq > 0)
#   ok audit-chain-verify   POST /api/v1/platform/audit/chain/verify → ok: true
#   ok alert-rules-loaded   Prometheus loaded the `sutra` rule group (7 alerts)
#   ok grafana-dashboards   the five sutra-* dashboards exist in Grafana
#
# Inputs (contract §10.4): SUTRA_CI_MODE=compose|kind (default compose), SUTRA_CI_API_URL
# (default http://localhost:8080), SUTRA_CI_NS (default sutraerp), KUBECONFIG (kind),
# SUTRA_CI_TELEMETRY_TIMEOUT seconds per wait (default 180), SUTRA_CI_TRACE_OUT (optional file
# that receives the trace id). compose: the stack of deploy/observability/compose.yaml on
# localhost (Grafana 3000, Prometheus 9090, Loki 3100, Tempo 3200) and the API's metrics
# listener on localhost:9464. kind: port-forwards svc/<release>-lgtm and svc/<release>-api-metrics.
# Exits non-zero on the first failed check; `--dry-run` prints the plan and exits 0. Idempotent.
set -euo pipefail

MODE="${SUTRA_CI_MODE:-compose}"
API="${SUTRA_CI_API_URL:-http://localhost:8080}"
NS="${SUTRA_CI_NS:-sutraerp}"
RELEASE="${SUTRA_CI_RELEASE:-sutraerp}"
TIMEOUT="${SUTRA_CI_TELEMETRY_TIMEOUT:-180}"
LOGIN="${SUTRA_CI_LOGIN:-admin@sutra.dev}"
PASSWORD="${SUTRA_CI_PASSWORD:-sutra}"
DASHBOARDS=(sutra-api-red sutra-database sutra-outbox-jobs sutra-audit-chain sutra-workflow-search)
METRICS=(sutra_http_requests sutra_http_request_duration_seconds sutra_http_requests_in_flight
  sutra_db_pool_connections sutra_outbox_pending sutra_outbox_published sutra_outbox_failed
  sutra_job_runs sutra_audit_unsealed sutra_audit_chain_verify_failures sutra_build_info)

if [[ "${1:-}" == "--dry-run" ]]; then
  echo "check-telemetry (mode=$MODE, api=$API, ns=$NS): api-ready dev-sign-in metrics-names tempo-trace"
  echo "  prometheus-http loki-trace-log audit-chain-sealed audit-chain-verify alert-rules-loaded grafana-dashboards"
  exit 0
fi

for bin in curl jq openssl; do
  command -v "$bin" >/dev/null || { echo "FAIL prerequisites: $bin not found" >&2; exit 1; }
done

WORK="$(mktemp -d)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() { echo "FAIL $1: ${2:-}" >&2; exit 1; }
ok() { echo "ok $*"; }

case "$MODE" in
  compose)
    GRAFANA=http://localhost:3000 PROM=http://localhost:9090 LOKI=http://localhost:3100
    TEMPO=http://localhost:3200 METRICS_URL="${SUTRA_CI_METRICS_URL:-http://localhost:9464/metrics}"
    ;;
  kind)
    command -v kubectl >/dev/null || fail prerequisites "kubectl not found"
    kubectl -n "$NS" rollout status "deploy/$RELEASE-lgtm" --timeout="${TIMEOUT}s" >/dev/null \
      || fail prerequisites "deploy/$RELEASE-lgtm not ready"
    kubectl -n "$NS" port-forward "svc/$RELEASE-lgtm" 13000:3000 19090:9090 13100:3100 13200:3200 \
      >"$WORK/pf-lgtm.log" 2>&1 &
    PIDS+=($!)
    kubectl -n "$NS" port-forward "svc/$RELEASE-api-metrics" 19464:9464 >"$WORK/pf-metrics.log" 2>&1 &
    PIDS+=($!)
    GRAFANA=http://localhost:13000 PROM=http://localhost:19090 LOKI=http://localhost:13100
    TEMPO=http://localhost:13200 METRICS_URL="${SUTRA_CI_METRICS_URL:-http://localhost:19464/metrics}"
    sleep 3
    ;;
  *) fail prerequisites "SUTRA_CI_MODE must be compose or kind (got $MODE)" ;;
esac

# wait_for <check> <command…>: retries every 3 s until the command succeeds or TIMEOUT passes.
wait_for() {
  shift # the check name documents the call site
  local deadline=$((SECONDS + TIMEOUT))
  until "$@"; do
    (( SECONDS < deadline )) || return 1
    sleep 3
  done
}

TRACE_ID="$(openssl rand -hex 16)"
PARENT_ID="$(openssl rand -hex 8)"
TRACEPARENT="00-${TRACE_ID}-${PARENT_ID}-01"
[[ -n "${SUTRA_CI_TRACE_OUT:-}" ]] && echo "$TRACE_ID" >"$SUTRA_CI_TRACE_OUT"
JAR="$WORK/cookies"

api() { # api <method> <path> [json-body] → body on stdout, fails on non-2xx
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -f -X "$method" -b "$JAR" -c "$JAR" -H "traceparent: $TRACEPARENT" -H "accept: application/json")
  [[ -n "${CSRF:-}" ]] && args+=(-H "x-csrf-token: $CSRF")
  [[ -n "$body" ]] && args+=(-H "content-type: application/json" --data "$body")
  curl "${args[@]}" "$API$path"
}

# ---- api-ready
ready() { curl -sf -o /dev/null "$API/health/ready"; }
wait_for api-ready ready || fail api-ready "$API/health/ready did not answer 200 within ${TIMEOUT}s"
ok api-ready

# ---- dev-sign-in (all requests share TRACE_ID through the inbound traceparent)
CSRF="$(api GET /api/v1/auth/csrf | jq -r .csrfToken)" || fail dev-sign-in "GET /api/v1/auth/csrf"
[[ -n "$CSRF" && "$CSRF" != null ]] || fail dev-sign-in "no csrfToken"
LOGIN_BODY="$(jq -cn --arg l "$LOGIN" --arg p "$PASSWORD" '{loginId: $l, password: $p}')"
SESSION_CSRF="$(api POST /api/v1/auth/login "$LOGIN_BODY" | jq -r .csrfToken)" \
  || fail dev-sign-in "POST /api/v1/auth/login as $LOGIN"
CSRF="$SESSION_CSRF"
api GET /api/v1/auth/session >/dev/null || fail dev-sign-in "GET /api/v1/auth/session"
api GET /api/v1/platform/audit/chain >"$WORK/chain0.json" || fail dev-sign-in "GET /api/v1/platform/audit/chain"
ok dev-sign-in "(trace $TRACE_ID)"

# ---- metrics-names
metrics_complete() {
  curl -sf "$METRICS_URL" >"$WORK/metrics.txt" || return 1
  local m
  for m in "${METRICS[@]}"; do
    grep -q "^# HELP $m " "$WORK/metrics.txt" || { echo "waiting for $m" >"$WORK/metrics.missing"; return 1; }
  done
}
wait_for metrics-names metrics_complete \
  || fail metrics-names "$(cat "$WORK/metrics.missing" 2>/dev/null) on $METRICS_URL"
grep -q '^sutra_http_requests_total{.*route="/api/v1/platform/audit/chain"' "$WORK/metrics.txt" \
  || fail metrics-names "no sutra_http_requests_total series for the matched route"
grep -q '^sutra_build_info{' "$WORK/metrics.txt" || fail metrics-names "sutra_build_info has no series"
ok metrics-names "(${#METRICS[@]} families)"

# ---- tempo-trace: the trace exists and carries a server span with http.route
tempo_trace() {
  curl -sf -H 'accept: application/json' "$TEMPO/api/traces/$TRACE_ID" >"$WORK/trace.json" || return 1
  jq -e '[.. | objects | select(.key? == "http.route")] | length > 0' "$WORK/trace.json" >/dev/null
}
wait_for tempo-trace tempo_trace || fail tempo-trace "trace $TRACE_ID with an http.route span not found in Tempo"
ROUTES="$(jq -r '[.. | objects | select(.key? == "http.route") | .value.stringValue] | unique | join(" ")' "$WORK/trace.json")"
ok tempo-trace "($ROUTES)"

# ---- prometheus-http
prom_http() {
  curl -sfG "$PROM/api/v1/query" --data-urlencode 'query=sum(sutra_http_requests_total)' >"$WORK/prom.json" || return 1
  jq -e '(.data.result[0].value[1] // "0" | tonumber) > 0' "$WORK/prom.json" >/dev/null
}
wait_for prometheus-http prom_http || fail prometheus-http "sum(sutra_http_requests_total) not > 0 in Prometheus"
ok prometheus-http "(sum=$(jq -r '.data.result[0].value[1]' "$WORK/prom.json"))"

# ---- loki-trace-log
START_NS="$(( ($(date +%s) - 3600) * 1000000000 ))"
loki_log() {
  curl -sfG "$LOKI/loki/api/v1/query_range" \
    --data-urlencode "query={service_name=\"sutra-api\"} | trace_id=\"$TRACE_ID\"" \
    --data-urlencode "start=$START_NS" --data-urlencode "limit=20" >"$WORK/loki.json" || return 1
  jq -e '[.data.result[].values[]] | length > 0' "$WORK/loki.json" >/dev/null
}
wait_for loki-trace-log loki_log || fail loki-trace-log "no Loki line with trace_id=$TRACE_ID"
ok loki-trace-log "($(jq '[.data.result[].values[]] | length' "$WORK/loki.json") lines)"

# ---- audit-chain-sealed + audit-chain-verify (the sign-in above wrote audit rows)
sealed() {
  api GET /api/v1/platform/audit/chain >"$WORK/chain.json" || return 1
  jq -e '.headSeq > 0' "$WORK/chain.json" >/dev/null
}
wait_for audit-chain-sealed sealed || fail audit-chain-sealed "headSeq stayed 0 ($(cat "$WORK/chain.json" 2>/dev/null))"
ok audit-chain-sealed "(headSeq=$(jq .headSeq "$WORK/chain.json"), unsealed=$(jq .unsealed "$WORK/chain.json"))"
api POST /api/v1/platform/audit/chain/verify '{}' >"$WORK/verify.json" || fail audit-chain-verify "POST verify"
jq -e '.ok == true and .firstBrokenSeq == null' "$WORK/verify.json" >/dev/null \
  || fail audit-chain-verify "$(cat "$WORK/verify.json")"
ok audit-chain-verify "(checked=$(jq .checked "$WORK/verify.json"))"

# ---- alert-rules-loaded
rules() {
  curl -sf "$PROM/api/v1/rules" >"$WORK/rules.json" || return 1
  jq -e '[.data.groups[] | select(.name == "sutra") | .rules[]] | length == 7' "$WORK/rules.json" >/dev/null
}
wait_for alert-rules-loaded rules || fail alert-rules-loaded "group sutra with 7 rules not loaded by Prometheus"
ok alert-rules-loaded

# ---- grafana-dashboards (compose imports them through the API; kind provisions the ConfigMaps)
if [[ "$MODE" == compose ]]; then
  for d in "${DASHBOARDS[@]}"; do
    jq -c '{dashboard: (. + {id: null}), overwrite: true}' "deploy/observability/dashboards/$d.json" \
      | curl -sf -X POST -H 'content-type: application/json' --data @- "$GRAFANA/api/dashboards/db" >/dev/null \
      || fail grafana-dashboards "import $d"
  done
fi
dash() {
  local d
  for d in "${DASHBOARDS[@]}"; do curl -sf -o /dev/null "$GRAFANA/api/dashboards/uid/$d" || return 1; done
}
wait_for grafana-dashboards dash || fail grafana-dashboards "not every sutra-* dashboard is in Grafana"
ok grafana-dashboards "(${#DASHBOARDS[@]})"
