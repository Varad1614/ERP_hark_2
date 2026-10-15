#!/usr/bin/env bash
# Checks a dev container / Codespace that .devcontainer/start.sh brought up. Used by
# .github/workflows/sutraerp-devcontainer.yml; also handy in a codespace terminal.
#   API ready · demo seed done · web 200 · dev login through the web proxy with the browser's
#   Codespaces Origin → authenticated · a foreign Origin is still refused.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../sutraerp"
DC=(docker compose --env-file .env.codespaces -f docker-compose.yml -f docker-compose.codespaces.yml)
# shellcheck disable=SC1091
source .env.codespaces
ORIGIN="$SUTRA_CS_ORIGIN"
WEB=http://localhost:3000
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

code=$(curl -s -o /tmp/ready.json -w '%{http_code}' http://localhost:8080/health/ready)
[[ "$code" == 200 ]] || fail "health/ready $code $(cat /tmp/ready.json)"
pass "health/ready 200 $(cat /tmp/ready.json)"

seed=$("${DC[@]}" exec -T postgres psql -U sutra -d sutra -tAc \
  "SELECT i.status || ':' || (SELECT count(*) FROM academic.course_offering o WHERE o.institution_id = i.id)
     FROM institution.institution i WHERE upper(i.code) = 'DEMO'" | tr -d '[:space:]')
[[ "$seed" == active:* && "${seed#active:}" -ge 1 ]] || fail "demo seed: '$seed' (want active with an offering)"
pass "demo college DEMO active, offerings ${seed#active:}"

code=$(curl -s -o /tmp/index.html -w '%{http_code}' "$WEB/")
[[ "$code" == 200 ]] || fail "GET / $code"
pass "web GET / 200 ($(wc -c </tmp/index.html) bytes)"

# README API flow, through nginx, as the browser in the codespace would send it.
login() { # $1 = Origin, $2 = Host header (empty = localhost:3000)
  local hosth=(); [[ -n "${2:-}" ]] && hosth=(-H "Host: $2")
  curl -s -D /tmp/csrf.h -o /tmp/csrf.json "${hosth[@]}" "$WEB/api/v1/auth/csrf"
  local cookie token
  cookie=$(grep -i '^set-cookie:' /tmp/csrf.h | head -1 | sed -E 's/^[Ss]et-[Cc]ookie: *([^;]*).*/\1/' | tr -d '\r')
  token=$(sed -E 's/.*"csrfToken":"([^"]*)".*/\1/' /tmp/csrf.json)
  [[ -n "$cookie" && -n "$token" ]] || fail "csrf: $(cat /tmp/csrf.h /tmp/csrf.json)"
  curl -s -D /tmp/login.h -o /tmp/login.json -w '%{http_code}' "${hosth[@]}" -X POST "$WEB/api/v1/auth/login" \
    -H "Origin: $1" -H 'Sec-Fetch-Site: same-origin' -H 'Content-Type: application/json' \
    -H "Cookie: $cookie" -H "X-CSRF-Token: $token" \
    --data '{"loginId":"admin@sutra.dev","password":"sutra"}'
}

code=$(login "$ORIGIN")
grep -q '"outcome":"authenticated"' /tmp/login.json || fail "login via proxy, Origin $ORIGIN: $code $(cat /tmp/login.json)"
session=$(grep -i '^set-cookie:' /tmp/login.h | grep -iv login_csrf | head -1 | sed -E 's/^[Ss]et-[Cc]ookie: *([^;]*).*/\1/' | tr -d '\r')
grep -i '^set-cookie:' /tmp/login.h | grep -iv login_csrf | head -1 | grep -qi 'secure' || fail "session cookie is not Secure"
pass "dev login via web proxy with Origin $ORIGIN → authenticated (Secure session cookie)"

code=$(curl -s -o /tmp/session.json -w '%{http_code}' -H "Cookie: $session" "$WEB/api/v1/auth/session")
[[ "$code" == 200 ]] || fail "GET /auth/session with the new cookie: $code $(cat /tmp/session.json)"
pass "GET /api/v1/auth/session 200"

# Same, when the forwarding proxy passes the public Host through.
if [[ "$ORIGIN" == https://* ]]; then
  code=$(login "$ORIGIN" "${ORIGIN#https://}")
  grep -q '"outcome":"authenticated"' /tmp/login.json || fail "login with public Host header: $code $(cat /tmp/login.json)"
  pass "dev login with Host ${ORIGIN#https://} → authenticated"
fi

# The Origin guard still refuses other sites.
code=$(login "https://evil.example")
[[ "$code" == 403 ]] || fail "foreign Origin should be 403, got $code $(cat /tmp/login.json)"
pass "foreign Origin https://evil.example → 403"

echo "ALL CHECKS PASSED"
