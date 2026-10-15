#!/usr/bin/env bash
# Part 1 demo: waits until every service of the compose stack is up and healthy, then prints
# `docker compose ps` and each health endpoint (evidence for the job log).
#   COMPOSE_FILE=docker-compose.yml:docker-compose.demo.yml COMPOSE_PROFILES=app bash deploy/ci/demo-wait.sh
# Fails (exit 1) after DEMO_WAIT_SECS (default 420) with the logs of whatever is not ready.
set -uo pipefail
deadline=$(( $(date +%s) + ${DEMO_WAIT_SECS:-420} ))

health() { docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$(docker compose ps -q "$1" 2>/dev/null | head -1)" 2>/dev/null || echo missing; }
http_ok() { curl -fsS -o /dev/null -m 5 "$1"; }

pending=""
while :; do
  pending=""
  for s in postgres keycloak valkey nats mailpit; do
    [ "$(health "$s")" = "healthy" ] || pending="$pending $s(container:$(health "$s"))"
  done
  m=$(docker compose ps -a --format '{{.State}} {{.ExitCode}}' migrate 2>/dev/null | head -1)
  case "$m" in "exited 0") ;; "exited "*) echo "migrate FAILED: $m"; docker compose logs --no-color migrate | tail -80; exit 1 ;; *) pending="$pending migrate($m)" ;; esac
  http_ok http://localhost:8080/health/ready || pending="$pending api(/health/ready)"
  http_ok http://localhost:3000/ || pending="$pending web(/)"
  http_ok http://localhost:3000/api/v1/auth/csrf || pending="$pending web→api(/api/v1/auth/csrf)"
  http_ok http://localhost:8180/realms/sutra/.well-known/openid-configuration || pending="$pending keycloak(realm sutra)"
  [ -z "$pending" ] && break
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "NOT READY:$pending"
    docker compose ps -a
    for s in $pending; do n=${s%%(*}; echo "---- logs $n"; docker compose logs --no-color --tail 60 "$n" 2>&1; done
    exit 1
  fi
  sleep 3
done

echo "=== all services ready ($(date -u +%FT%TZ))"
docker compose ps -a
echo "=== health endpoints"
for u in http://localhost:8080/health/live http://localhost:8080/health/ready http://localhost:3000/api/v1/auth/csrf \
         "http://localhost:8222/healthz?js-enabled-only=true" http://localhost:8025/readyz \
         http://localhost:8180/realms/sutra/.well-known/openid-configuration; do
  body=$(curl -sS -m 5 -w ' [HTTP %{http_code}]' "$u" 2>&1 | head -c 400)
  echo "$u → $body"
done
echo "postgres: $(docker compose exec -T postgres pg_isready -U sutra -d sutra)"
echo "valkey:   $(docker compose exec -T valkey valkey-cli ping)"
echo "keycloak: $(health keycloak) (management /health/ready via container healthcheck)"
echo "migrations applied: $(docker compose exec -T postgres psql -U sutra -d sutra -tAc 'SELECT count(*) || '' (last '' || max(version) || '')'' FROM _sqlx_migrations WHERE success')"
