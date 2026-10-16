#!/usr/bin/env bash
# Part 2 · S5-capacity: the capacity test, end to end (run from sutraerp/; README.md).
#
#   run.sh prepare   secrets + private CA, Keycloak realm, host names, CA trust, client IPs
#   run.sh flood-check  per-client flood buckets behind the edge (DEF-P2-CAP-003; run by up)
#   run.sh up        build images (api from deploy/docker/demo-bin/sutra-api, web, minio) and
#                    start the production-mode stack; wait until every service is healthy; bucket
#   run.sh seed      2,000-student college through the API (deploy/capacity/seed.mjs)
#   run.sh load      k6 college day + spikes with resource / database collectors
#   run.sh report    capacity-results.{md,json}; exits 1 when a threshold is breached
#   run.sh logs      container logs into $CAP_OUT/logs
#   run.sh down      stop and delete the stack (volumes included)
# Env: CAP_DIR (state, secrets — never upload it), CAP_OUT (evidence), CAP_DOMAIN (sutra.test),
# CAP_STUDENTS (2000), CAP_SCALE (k6 time scale, 1), CAP_CLIENT_IPS (500).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export CAP_DIR="${CAP_DIR:-$PWD/capacity-run}"
export CAP_OUT="${CAP_OUT:-$PWD/capacity-out}"
export CAP_DOMAIN="${CAP_DOMAIN:-sutra.test}"
mkdir -p "$CAP_DIR" "$CAP_OUT"
EDGE_IP="${CAP_EDGE_IP:-10.77.0.1}"
compose() { docker compose -f "$here/compose.yml" --env-file "$CAP_DIR/env" "$@"; }
envval() { grep -E "^$1=" "$CAP_DIR/env" | head -1 | cut -d= -f2-; }
pgurl() { echo "postgres://sutra:$(envval POSTGRES_PASSWORD)@127.0.0.1:5432/sutra?sslmode=require"; }

case "${1:-}" in
  prepare)
    bash "$here/gen-secrets.sh"
    set -a; . "$CAP_DIR/env"; set +a
    python3 "$here/gen-realm.py" "$CAP_DIR/realm-sutra.json" "$CAP_DIR/staff.json"
    chmod 644 "$CAP_DIR/realm-sutra.json"
    # Host names → the edge address; the CA trusted system-wide (curl, k6) and by node.
    sudo ip addr add "$EDGE_IP/16" dev lo 2>/dev/null || true
    for h in erp sso s3; do
      grep -q " $h.$CAP_DOMAIN\$" /etc/hosts || echo "$EDGE_IP $h.$CAP_DOMAIN" | sudo tee -a /etc/hosts >/dev/null
    done
    sudo cp "$CAP_DIR/certs/ca.pem" /usr/local/share/ca-certificates/sutra-capacity-ca.crt
    sudo update-ca-certificates >/dev/null
    echo "ok prepared: edge $EDGE_IP (lo /16: client addresses 10.77.1.1+), CA trusted"
    ;;
  up)
    compose build
    compose up -d
    deadline=$(( $(date +%s) + ${CAP_WAIT_SECS:-900} ))
    while :; do
      pending=""
      for s in postgres keycloak valkey nats minio; do
        st=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$(compose ps -q "$s" | head -1)" 2>/dev/null || echo missing)
        [[ "$st" == healthy ]] || pending="$pending $s($st)"
      done
      m=$(compose ps -a --format '{{.State}} {{.ExitCode}}' migrate 2>/dev/null | head -1)
      case "$m" in "exited 0") ;; "exited "*) echo "migrate FAILED: $m"; compose logs --no-color migrate | tail -60; exit 1 ;; *) pending="$pending migrate($m)" ;; esac
      curl -fsS -o /dev/null -m 5 "https://erp.$CAP_DOMAIN/api/v1/auth/csrf" || pending="$pending edge→web→api"
      curl -fsS -o /dev/null -m 5 "https://sso.$CAP_DOMAIN/realms/sutra/.well-known/openid-configuration" || pending="$pending edge→keycloak"
      curl -fsS -o /dev/null -m 5 "https://s3.$CAP_DOMAIN/minio/health/live" || pending="$pending edge→minio"
      [[ -z "$pending" ]] && break
      if [[ $(date +%s) -ge $deadline ]]; then
        echo "NOT READY:$pending"; compose ps -a
        for s in api keycloak flowable migrate edge; do echo "---- $s"; compose logs --no-color --tail 80 "$s" 2>&1; done
        exit 1
      fi
      # Production refuses SUTRA_S3_CREATE_BUCKET: the operator creates the private bucket
      # (S3 PUT Bucket, SigV4) as soon as MinIO answers through the edge.
      if [[ -z "${bucket:-}" ]] && curl -fsS -o /dev/null -m 5 "https://s3.$CAP_DOMAIN/minio/health/live" 2>/dev/null; then
        s3=(--aws-sigv4 "aws:amz:us-east-1:s3" --user "$(envval S3_ACCESS_KEY):$(envval S3_SECRET_KEY)")
        code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -X PUT "${s3[@]}" "https://s3.$CAP_DOMAIN/sutraerp-documents")
        if [[ "$code" == 200 || "$code" == 409 ]]; then bucket=1; echo "ok bucket sutraerp-documents ($code, private)"; fi
      fi
      sleep 5
    done
    compose ps
    compose run --rm --no-deps api check-config | tee "$CAP_OUT/api-check-config.txt"
    grep -q "env=Production" "$CAP_OUT/api-check-config.txt"
    grep -q "auth=Keycloak" "$CAP_OUT/api-check-config.txt"
    bash "$here/run.sh" flood-check
    echo "ok stack up (production mode)"
    ;;
  flood-check)
    # DEF-P2-CAP-003 regression: behind the edge every client has its OWN flood-guard bucket in
    # the web nginx (100 r/s + burst 400 on /api/). Client A floods until it is refused; client B
    # (another campus address) is still served at once.
    url="https://erp.$CAP_DOMAIN/api/v1/auth/csrf"
    seq 1500 | xargs -P 64 -I{} curl -s -o /dev/null -w '%{http_code}\n' -m 10 --interface 10.77.250.1 "$url" >"$CAP_DIR/flood-a.txt" || true
    b=$(curl -s -o /dev/null -w '%{http_code}' -m 10 --interface 10.77.250.2 "$url")
    a429=$(grep -c '^429$' "$CAP_DIR/flood-a.txt" || true)
    a200=$(grep -c '^200$' "$CAP_DIR/flood-a.txt" || true)
    echo "flood-check: client A 1500 requests → $a200 × 200, $a429 × 429; client B → $b" | tee "$CAP_OUT/flood-check.txt"
    [[ "$a429" -gt 0 && "$b" == 200 ]] || { echo "flood-check FAILED: the flood guard is not per client"; exit 1; }
    sleep 6 # let client A's bucket drain before anything else runs
    ;;
  seed)
    set -a; . "$CAP_DIR/env"; set +a
    E2E_AUTH=oidc E2E_BASE="https://erp.$CAP_DOMAIN" E2E_PASSWORD="$SEED_PASSWORD" NODE_EXTRA_CA_CERTS="$CAP_DIR/certs/ca.pem" \
      DATABASE_URL="$(pgurl)" KC_ADMIN_PASSWORD="$KC_ADMIN_PASSWORD" \
      node "$here/seed.mjs" 2>&1 | tee "$CAP_OUT/seed.log"
    cp "$CAP_DIR/seed-report.json" "$CAP_OUT/"
    ;;
  load)
    set -a; . "$CAP_DIR/env"; set +a
    export PGURL; PGURL="$(pgurl)"
    n="${CAP_CLIENT_IPS:-500}"
    last=$(( n - 1 ))
    ips="10.77.1.1-10.77.$(( 1 + last / 250 )).$(( 1 + last % 250 ))"
    bash "$here/collect.sh" snapshot "$CAP_OUT" before-load
    bash "$here/collect.sh" statements "$CAP_OUT" reset
    bash "$here/collect.sh" start "$CAP_OUT"
    set +e
    CAP_BASE="https://erp.$CAP_DOMAIN" CAP_DATA="$CAP_DIR/cap-data.json" CAP_PASSWORD="$SEED_PASSWORD" CAP_OUT="$CAP_OUT" \
      CAP_THRESHOLDS="$here/thresholds.json" CAP_SCALE="${CAP_SCALE:-1}" \
      k6 run --quiet --local-ips="$ips" "$here/k6/college-day.js" 2>&1 | tee "$CAP_OUT/k6.log"
    k6_status=${PIPESTATUS[0]}
    set -e
    bash "$here/collect.sh" stop "$CAP_OUT"
    bash "$here/collect.sh" statements "$CAP_OUT"
    bash "$here/collect.sh" snapshot "$CAP_OUT" after-load
    echo "$k6_status" >"$CAP_OUT/k6.exit"
    echo "k6 exit $k6_status"
    ;;
  report)
    python3 "$here/analyze.py" "$CAP_OUT" --scale "${CAP_SCALE:-1}"
    ;;
  logs)
    mkdir -p "$CAP_OUT/logs"
    for s in postgres keycloak flowable valkey nats minio migrate api web edge; do compose logs --no-color --timestamps "$s" >"$CAP_OUT/logs/$s.log" 2>&1 || true; done
    compose ps -a >"$CAP_OUT/logs/ps.txt" 2>&1 || true
    ;;
  down)
    compose down -v --remove-orphans
    ;;
  *) echo "usage: run.sh prepare|up|seed|load|report|logs|down" >&2; exit 2 ;;
esac
