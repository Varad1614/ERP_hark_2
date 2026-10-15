#!/usr/bin/env bash
# SutraERP dev container / GitHub Codespaces bootstrap.
#
#   bash .devcontainer/start.sh --build    # postCreate: build images, start, wait, seed demo data
#   bash .devcontainer/start.sh --resume   # postStart: start the existing images again, wait
#   bash .devcontainer/start.sh            # same as --build (use after pulling new code)
#
# Runs the compose app profile (postgres, valkey, nats, mailpit, migrate → api, web) with
# docker-compose.codespaces.yml on top: no Keycloak, no observability stack. The browser origin
# comes from CODESPACE_NAME + GITHUB_CODESPACES_PORT_FORWARDING_DOMAIN (http://localhost:3000
# outside Codespaces). Demo seed is skipped when institution DEMO is already active.
#
# Env: SUTRA_CS_PRUNE=0 keeps the Docker build cache after the build (default: prune it, saves
# ~3 GB of codespace storage; a later --build then recompiles the API from scratch).
set -euo pipefail

MODE="${1:---build}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/sutraerp"
cd "$APP"

log() { printf '\n\033[1;36m[sutra %s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }

# ---- browser origin -------------------------------------------------------------------------
if [[ -n "${CODESPACE_NAME:-}" && -n "${GITHUB_CODESPACES_PORT_FORWARDING_DOMAIN:-}" ]]; then
  HOST="${CODESPACE_NAME}-3000.${GITHUB_CODESPACES_PORT_FORWARDING_DOMAIN}"
  ORIGIN="https://${HOST}"
else
  HOST="localhost"
  ORIGIN="http://localhost:3000"
fi
cat > .env.codespaces <<EOF
# Written by .devcontainer/start.sh — do not commit.
SUTRA_CS_ORIGIN=${ORIGIN}
SUTRA_CS_HOST=${HOST}
EOF
DC=(docker compose --env-file .env.codespaces -f docker-compose.yml -f docker-compose.codespaces.yml)

# Convenience for the terminal: `sutra-compose ps`, `sutra-compose logs -f api`, …
if ! grep -q 'alias sutra-compose=' "$HOME/.bashrc" 2>/dev/null; then
  echo "alias sutra-compose='cd \"$APP\" && docker compose --env-file .env.codespaces -f docker-compose.yml -f docker-compose.codespaces.yml'" >> "$HOME/.bashrc"
fi

# ---- docker daemon (docker-in-docker starts it asynchronously) -------------------------------
for i in $(seq 1 60); do docker info >/dev/null 2>&1 && break; sleep 2; done
docker info >/dev/null 2>&1 || { echo "docker daemon not available" >&2; exit 1; }

log "browser origin: $ORIGIN"

# ---- start ----------------------------------------------------------------------------------
if [[ "$MODE" == "--resume" ]] && docker image inspect sutraerp-api >/dev/null 2>&1; then
  log "starting the existing stack (no rebuild)"
  "${DC[@]}" --profile app up -d
else
  log "building and starting the stack — the first Rust build takes ~10–15 min"
  "${DC[@]}" --profile app up --build -d
fi

# ---- wait for the API -----------------------------------------------------------------------
log "waiting for http://localhost:8080/health/ready"
ok=""
for i in $(seq 1 180); do
  if [[ "$(curl -s -o /dev/null -w '%{http_code}' http://localhost:8080/health/ready || true)" == "200" ]]; then ok=1; break; fi
  sleep 5
done
if [[ -z "$ok" ]]; then
  "${DC[@]}" --profile app ps -a || true
  "${DC[@]}" --profile app logs --tail 80 migrate api || true
  echo "API did not become ready in 15 min" >&2
  exit 1
fi
log "API ready"

# ---- demo seed (idempotent; skipped when DEMO is already active) -----------------------------
status="$("${DC[@]}" exec -T postgres psql -U sutra -d sutra -tAc \
  "SELECT status FROM institution.institution WHERE upper(code) = 'DEMO'" 2>/dev/null | tr -d '[:space:]' || true)"
if [[ "$status" == "active" ]]; then
  log "demo college DEMO already seeded — skipping"
else
  log "seeding the demo college (Sutra Demo College, DEMO)"
  "${DC[@]}" --profile demo run --rm --build demo-seed
fi

# ---- trim storage ---------------------------------------------------------------------------
if [[ "$MODE" != "--resume" && "${SUTRA_CS_PRUNE:-1}" == "1" ]]; then
  log "pruning the Docker build cache and dangling images (SUTRA_CS_PRUNE=0 keeps them)"
  docker builder prune -af >/dev/null 2>&1 || true
  docker image prune -f >/dev/null 2>&1 || true
fi

log "SutraERP is up: $ORIGIN  (Ports tab → 3000 'SutraERP'; Mailpit on 8025)"
log "sign in with a role card, password: sutra — then pick 'Sutra Demo College (DEMO)'"
