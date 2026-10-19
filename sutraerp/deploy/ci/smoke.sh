#!/usr/bin/env bash
# Batch 9 CI stage `smoke` — owned by slice 9B (docs/batch9-contracts.md §10.4).
# Checks (one "ok <check>" line each, non-zero exit on the first failure):
#   api /health/live + /health/ready · dev sign-in (CSRF → login → session) · the SMOKE institution
#   (created once, reused after: idempotent) · one authenticated GET per domain (institution,
#   student, academic offerings, finance config, people staff, campus tree, services metrics,
#   resources stock) · web `/` serves the SPA, falls back for deep links and proxies /api · the
#   migrate Job / container succeeded · kind only: OpenBao injected /vault/secrets/sutra.env into
#   api + migrate and no Kubernetes Secret in the namespace carries an app credential (§5).
# Inputs: SUTRA_CI_MODE=kind|compose, SUTRA_CI_API_URL (http://localhost:8080), SUTRA_CI_WEB_URL
#   (http://localhost:8081 kind / http://localhost:3000 compose), SUTRA_CI_NS (sutraerp), KUBECONFIG,
#   SUTRA_CI_LOGIN (admin@sutra.dev), SUTRA_CI_PASSWORD (default: $SUTRA_DEV_AUTH_PASSWORD — dev auth has no
#   built-in password, audit 2026-10 D1/C1).
# Runs from sutraerp/. Needs curl + jq (+ kubectl in kind mode, docker compose in compose mode).
set -euo pipefail

MODE=${SUTRA_CI_MODE:-kind}
API=${SUTRA_CI_API_URL:-http://localhost:8080}
if [[ "$MODE" == compose ]]; then WEB=${SUTRA_CI_WEB_URL:-http://localhost:3000}; else WEB=${SUTRA_CI_WEB_URL:-http://localhost:8081}; fi
NS=${SUTRA_CI_NS:-sutraerp}
LOGIN=${SUTRA_CI_LOGIN:-admin@sutra.dev}
PASSWORD=${SUTRA_CI_PASSWORD:-${SUTRA_DEV_AUTH_PASSWORD:?set SUTRA_CI_PASSWORD or SUTRA_DEV_AUTH_PASSWORD}}
CODE=${SUTRA_CI_SMOKE_CODE:-SMOKE}

if [[ "${1:-}" == "--dry-run" ]]; then
  extra=""
  [[ $MODE == kind ]] && extra=", openbao injection, no credential Secrets"
  echo "smoke ($MODE): health live/ready, dev sign-in $LOGIN, institution $CODE, 8 domain reads, web SPA + /api proxy, migrate succeeded$extra"
  exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
ok() { echo "ok $*"; }
fail() { echo "FAIL $*" >&2; exit 1; }

COOKIE=""
CSRF=""
CODE_=""
# req METHOD PATH [JSON] → body in $TMP/body, status in $CODE_; keeps the newest Set-Cookie set
# (harness.mjs semantics). Called directly (not in $(…)) so COOKIE survives.
req() {
  local method=$1 path=$2 data=${3:-}
  local args=(-s -o "$TMP/body" -D "$TMP/headers" -w '%{http_code}' -X "$method" --max-time 30)
  if [[ -n "$COOKIE" ]]; then args+=(-H "Cookie: $COOKIE"); fi
  if [[ "$method" != GET ]]; then
    [[ -n "$data" ]] || data='{}'
    args+=(-H "Content-Type: application/json" -H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)")
    if [[ -n "$CSRF" ]]; then args+=(-H "X-CSRF-Token: $CSRF"); fi
    args+=(--data "$data")
  fi
  : >"$TMP/headers"
  CODE_=$(curl "${args[@]}" "$API$path") || CODE_=000
  local set
  set=$(grep -i '^set-cookie:' "$TMP/headers" | sed -E 's/^[^:]*:[[:space:]]*//; s/;.*//' | tr -d '\r' | paste -sd ';' - | sed 's/;/; /g' || true)
  if [[ -n "$set" ]]; then COOKIE=$set; fi
}
expect200() { # name path
  req GET "$2"
  [[ "$CODE_" == 200 ]] || fail "$1: GET $2 → $CODE_ $(head -c 300 "$TMP/body")"
  jq -e . "$TMP/body" >/dev/null || fail "$1: GET $2 is not JSON"
  ok "$1"
}

# ---- health ----------------------------------------------------------------------------------
for i in $(seq 1 90); do
  curl -fs --max-time 5 "$API/health/live" >/dev/null 2>&1 && break
  [[ $i == 90 ]] && fail "api /health/live not up at $API"
  sleep 2
done
ok "api-live"
for i in $(seq 1 60); do
  code=$(curl -s -o "$TMP/ready" -w '%{http_code}' --max-time 5 "$API/health/ready" || echo 000)
  [[ "$code" == 200 ]] && break
  [[ $i == 60 ]] && fail "api /health/ready → $code $(head -c 300 "$TMP/ready")"
  sleep 2
done
ok "api-ready"

# ---- dev sign-in -----------------------------------------------------------------------------
req GET /api/v1/auth/csrf
[[ "$CODE_" == 200 ]] || fail "csrf → $CODE_"
CSRF=$(jq -r '.csrfToken' "$TMP/body")
req POST /api/v1/auth/login "$(jq -nc --arg l "$LOGIN" --arg p "$PASSWORD" '{loginId: $l, password: $p}')"
[[ "$CODE_" == 200 ]] || fail "login $LOGIN → $CODE_ $(head -c 300 "$TMP/body")"
[[ "$(jq -r '.outcome' "$TMP/body")" == authenticated ]] || fail "login outcome $(jq -c . "$TMP/body")"
CSRF=$(jq -r '.csrfToken' "$TMP/body")
ok "dev-sign-in"

# ---- institution (idempotent) ----------------------------------------------------------------
req GET /api/v1/institution/institutions
[[ "$CODE_" == 200 ]] || fail "institutions → $CODE_"
INST=$(jq -r --arg c "$CODE" '.items[] | select(.code == $c) | .id' "$TMP/body" | head -n1)
if [[ -z "$INST" ]]; then
  req POST /api/v1/institution/institutions "$(jq -nc --arg c "$CODE" '{code: $c, name: "Smoke Test Institute", timezone: "Asia/Kolkata"}')"
  [[ "$CODE_" == 201 || "$CODE_" == 200 ]] || fail "create institution → $CODE_ $(head -c 300 "$TMP/body")"
  INST=$(jq -r '.id' "$TMP/body")
fi
[[ -n "$INST" && "$INST" != null ]] || fail "no institution id"
ok "institution $CODE"

# ---- one read per domain ---------------------------------------------------------------------
expect200 read-institution "/api/v1/institution/institutions/$INST/campuses"
expect200 read-student "/api/v1/student/institutions/$INST/students?limit=5"
expect200 read-academic "/api/v1/academic/institutions/$INST/offerings"
expect200 read-finance "/api/v1/finance/institutions/$INST/finance-config"
expect200 read-people "/api/v1/people/institutions/$INST/staff?limit=5"
expect200 read-campus "/api/v1/campus/institutions/$INST/infrastructure-tree"
expect200 read-services "/api/v1/services/institutions/$INST/metrics"
expect200 read-resources "/api/v1/resources/institutions/$INST/stock-on-hand"

# ---- web -------------------------------------------------------------------------------------
for i in $(seq 1 30); do
  code=$(curl -s -o "$TMP/index" -w '%{http_code}' --max-time 10 "$WEB/" || echo 000)
  [[ "$code" == 200 ]] && break
  [[ $i == 30 ]] && fail "web / → $code"
  sleep 2
done
grep -qi '<html' "$TMP/index" || fail "web / is not HTML"
grep -qiE '<script[^>]+type="module"|/assets/' "$TMP/index" || fail "web / does not reference the SPA bundle"
ok "web-spa"
code=$(curl -s -o "$TMP/deep" -w '%{http_code}' --max-time 10 "$WEB/admin/some/deep/link" || echo 000)
{ [[ "$code" == 200 ]] && grep -qi '<html' "$TMP/deep"; } || fail "web SPA fallback → $code"
ok "web-spa-fallback"
code=$(curl -s -o "$TMP/proxied" -w '%{http_code}' --max-time 10 "$WEB/api/v1/auth/csrf" || echo 000)
{ [[ "$code" == 200 ]] && jq -e '.csrfToken' "$TMP/proxied" >/dev/null; } || fail "web /api proxy → $code"
ok "web-api-proxy"

# ---- migrations ------------------------------------------------------------------------------
if [[ "$MODE" == kind ]]; then
  succeeded=$(kubectl -n "$NS" get jobs -l app.kubernetes.io/component=migrate -o json | jq '[.items[] | select((.status.succeeded // 0) >= 1)] | length')
  [[ "$succeeded" -ge 1 ]] || { kubectl -n "$NS" get jobs -o wide >&2; fail "no succeeded migrate Job"; }
  ok "migrate-job-succeeded"
else
  exit_code=$(docker compose ps -a --format json migrate | jq -rs 'flatten | .[0].ExitCode // empty')
  [[ "$exit_code" == 0 ]] || fail "compose migrate exit code '$exit_code'"
  ok "migrate-container-succeeded"
fi

# ---- kind: OpenBao secrets path ----------------------------------------------------------------
if [[ "$MODE" == kind ]]; then
  pods=$(kubectl -n "$NS" get pods -o json)
  api_pod=$(jq -c '[.items[] | select(.metadata.labels["app.kubernetes.io/component"] == "api" and .status.phase == "Running")][0]' <<<"$pods")
  [[ "$api_pod" != null ]] || fail "no running api pod"
  jq -e '.metadata.annotations["vault.hashicorp.com/agent-inject-status"] == "injected"' <<<"$api_pod" >/dev/null \
    || fail "api pod was not mutated by the OpenBao injector"
  jq -e '[.status.initContainerStatuses[]? | select(.name == "vault-agent-init") | .state.terminated.exitCode == 0] | any' <<<"$api_pod" >/dev/null \
    || fail "vault-agent-init did not render the secrets file"
  jq -e '[.spec.containers[] | select(.name == "api") | .volumeMounts[]? | select(.mountPath == "/vault/secrets")] | length > 0' <<<"$api_pod" >/dev/null \
    || fail "api container does not mount /vault/secrets"
  jq -e '[.spec.containers[] | select(.name == "api") | .env[]? | select(.name == "SUTRA_ENV_FILE" and .value == "/vault/secrets/sutra.env")] | length == 1' <<<"$api_pod" >/dev/null \
    || fail "api container does not set SUTRA_ENV_FILE=/vault/secrets/sutra.env"
  # The api has no other source of DATABASE_URL: no env, no envFrom Secret.
  jq -e '[.spec.containers[] | select(.name == "api") | (.env[]?.name), (.envFrom[]? | select(.secretRef) | "secretRef")] | map(select(. == "DATABASE_URL" or . == "secretRef")) | length == 0' <<<"$api_pod" >/dev/null \
    || fail "api pod gets credentials from somewhere other than OpenBao"
  ok "openbao-injected api reads /vault/secrets/sutra.env"
  mig=$(kubectl -n "$NS" get pods -l app.kubernetes.io/component=migrate -o json | jq '[.items[] | select(.metadata.annotations["vault.hashicorp.com/agent-inject-status"] == "injected")] | length')
  [[ "$mig" -ge 1 ]] || fail "migrate pod was not injected"
  ok "openbao-injected migrate"
  # §5: no app credential is a Kubernetes Secret (Helm release records and SA tokens aside).
  keys='DATABASE_URL|SUTRA_MIGRATION_DATABASE_URL|SUTRA_DB_RUNTIME_PASSWORD|POSTGRES_PASSWORD|SUTRA_CSRF_KEY|SUTRA_MFA_ENCRYPTION_KEY|SUTRA_OIDC_CLIENT_SECRET|SUTRA_SMTP_PASSWORD|SUTRA_AUDIT_ANCHOR_KEY|SUTRA_FLOWABLE_PASSWORD|FLOWABLE_DB_PASSWORD|SUTRA_OPENSEARCH_PASSWORD|SUTRA_BACKUP_S3_SECRET_KEY'
  bad=$(kubectl -n "$NS" get secrets -o json | jq -r --arg k "^($keys)$" '.items[] | select(.type != "helm.sh/release.v1" and .type != "kubernetes.io/service-account-token") | select([.data // {} | keys[] | test($k)] | any) | .metadata.name')
  [[ -z "$bad" ]] || fail "Secrets holding app credentials: $bad"
  # …and no database URL with a password leaked into the Helm release record either.
  rel=$(kubectl -n "$NS" get secrets -l owner=helm,name=sutraerp -o json | jq -r '.items[].data.release' | while read -r r; do base64 -d <<<"$r" | base64 -d | gunzip; done 2>/dev/null || true)
  [[ -n "$rel" ]] || fail "could not read the Helm release record"
  # INT: a shell-interpolated password (`${FLOWABLE_DB_PASSWORD:?…}` in the 9C backup script) is not a credential.
  if grep -qE 'postgres://[^:"]+:[^$@"][^@"]*@' <<<"$rel"; then fail "a database URL with a password is in the Helm release"; fi
  ok "no-credential-secrets"
fi

echo "smoke: all checks passed ($MODE)"
