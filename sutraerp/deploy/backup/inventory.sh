#!/usr/bin/env bash
# Configuration / secrets INVENTORY of a docker compose installation (Part 2 · Stage 5) — JSON on
# stdout. Records what a rebuild needs to know WITHOUT any secret value:
#   services[]  name, image, image id, environment variable NAMES (values never), ports, volumes
#   volumes[]   compose volumes; files[] sha256 + size of the config files (compose files, the
#   env file, realm import, nginx config); envFileKeys[] = the variable NAMES of SUTRA_ENV_FILE;
#   versions    docker, compose, postgres server, git sha.
# The secret VALUES live in the college's secret store (OpenBao / sealed envelope / password
# manager — see the runbook); this file tells the operator which ones must be re-supplied.
#
# Env: SUTRA_COMPOSE_DIR (default: current directory; COMPOSE_FILE / COMPOSE_PROFILES honoured),
#      SUTRA_ENV_FILE (optional .env of the installation), SUTRA_PG_CONTAINER (optional),
#      SUTRA_INVENTORY_FILES (extra config files, space separated). Needs docker + jq.
set -euo pipefail
cd "${SUTRA_COMPOSE_DIR:-.}"
command -v jq >/dev/null || { echo "inventory: jq missing" >&2; exit 1; }

cfg=$(docker compose config --format json 2>/dev/null || echo '{}')
services=$(jq -c '[(.services // {}) | to_entries[] | {
    name: .key,
    image: (.value.image // null),
    environmentNames: ((.value.environment // {}) | keys | sort),
    ports: [(.value.ports // [])[] | "\(.published // "")->\(.target)"],
    volumes: [(.value.volumes // [])[] | "\(.type):\(.source // "")->\(.target)"]
  }]' <<<"$cfg")
ids='{}'
while read -r svc img; do
  [[ -n "$svc" ]] || continue
  id=$(docker image inspect -f '{{.Id}}' "$img" 2>/dev/null || echo unknown)
  ids=$(jq -c --arg s "$svc" --arg i "$id" '. + {($s): $i}' <<<"$ids")
done < <(docker compose ps -a --format '{{.Service}} {{.Image}}' 2>/dev/null || true)
services=$(jq -c --argjson ids "$ids" '[.[] | . + {imageId: ($ids[.name] // null)}]' <<<"$services")
volumes=$(jq -c '[(.volumes // {}) | keys[]]' <<<"$cfg")

files='[]'
cands=(docker-compose.yml docker-compose.demo.yml deploy/backup/compose.backup.yml deploy/keycloak/realm-sutra-dev.json deploy/docker/nginx.conf)
for f in ${SUTRA_INVENTORY_FILES:-}; do cands+=("$f"); done
if [[ -n "${COMPOSE_FILE:-}" ]]; then IFS=: read -r -a extra <<<"$COMPOSE_FILE"; cands+=("${extra[@]}"); fi
if [[ -n "${SUTRA_ENV_FILE:-}" ]]; then cands+=("$SUTRA_ENV_FILE"); fi
while IFS= read -r f; do
  [[ -f "$f" ]] || continue
  files=$(jq -c --arg f "$f" --arg s "$(sha256sum "$f" | cut -d' ' -f1)" --argjson b "$(stat -c %s "$f")" '. + [{path: $f, sha256: $s, bytes: $b}]' <<<"$files")
done < <(printf '%s\n' "${cands[@]}" | awk 'NF && !seen[$0]++')
env_keys='[]'
if [[ -n "${SUTRA_ENV_FILE:-}" && -f "${SUTRA_ENV_FILE}" ]]; then
  env_keys=$(sed -n 's/^[[:space:]]*\(export[[:space:]]\+\)\?\([A-Za-z_][A-Za-z0-9_]*\)=.*/\2/p' "$SUTRA_ENV_FILE" | sort -u | jq -R . | jq -sc .)
fi
pg=""
if [[ -n "${SUTRA_PG_CONTAINER:-}" ]]; then pg=$(docker exec "$SUTRA_PG_CONTAINER" postgres --version 2>/dev/null || true); fi

jq -n --argjson services "$services" --argjson volumes "$volumes" --argjson files "$files" --argjson envKeys "$env_keys" \
  --arg docker "$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)" \
  --arg compose "$(docker compose version --short 2>/dev/null || echo unknown)" \
  --arg pg "$pg" --arg sha "${SUTRA_GIT_SHA:-unknown}" --arg at "$(date -u +%FT%TZ)" '{
    format: "sutra-inventory/v1", createdAt: $at, gitSha: $sha,
    note: "Names only. Secret VALUES are never written here; re-supply them from the secret store.",
    versions: {docker: $docker, compose: $compose, postgres: $pg},
    services: $services, volumes: $volumes, files: $files, envFileKeys: $envKeys }'
