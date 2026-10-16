#!/usr/bin/env bash
# Shared helpers of the on-prem staging drills (run-fresh.sh, run-upgrade.sh). Sourced.
# The staging host plays the college server: the bundle is installed with its own bin/sutra-onprem
# as root; this harness only drives it and checks the results from outside (https + counts).
set -euo pipefail

ERP_HOST=${ERP_HOST:-erp.sutra-staging.internal}
SSO_HOST=${SSO_HOST:-sso.sutra-staging.internal}
OUT=${OUT:?set OUT (results directory)}
CI_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
mkdir -p "$OUT"
export SUTRA_TIMINGS_FILE="$OUT/timings.tsv"
touch "$SUTRA_TIMINGS_FILE"
SO="sudo -E /opt/sutraerp/current/bin/sutra-onprem"

say() { printf '\n=== %s (%s)\n' "$*" "$(date +%H:%M:%S)"; }
now() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.1f", b - a }'; }
record() { printf '%s\t%s\n' "$1" "$2" >>"$SUTRA_TIMINGS_FILE"; echo "timing $1 = $2"; }
result() { printf '%s\t%s\n' "$1" "$2" >>"$OUT/results.tsv"; echo "result $1: $2"; }

# DNS for the staging host: both names → 127.0.0.1 (the college uses DNS A records instead).
staging_dns() {
  grep -q "$ERP_HOST" /etc/hosts || echo "127.0.0.1 $ERP_HOST $SSO_HOST" | sudo tee -a /etc/hosts >/dev/null
  getent hosts "$ERP_HOST" "$SSO_HOST"
}

# Offline proof: registries unresolvable for the rest of the job, every pre-pulled image removed.
go_offline() {
  local h
  for h in registry-1.docker.io index.docker.io auth.docker.io production.cloudflare.docker.com docker.io \
    quay.io cdn.quay.io cdn01.quay.io cdn02.quay.io cdn03.quay.io gcr.io ghcr.io pkg-containers.githubusercontent.com \
    registry.k8s.io mirror.gcr.io; do
    echo "0.0.0.0 $h" | sudo tee -a /etc/hosts >/dev/null
  done
  docker system prune -af >/dev/null 2>&1 || true
  if timeout 60 docker pull -q busybox:latest >/dev/null 2>&1; then
    echo "registry still reachable — offline proof failed" >&2
    return 1
  fi
  result offline "registries blackholed in /etc/hosts; docker pull busybox fails; $(docker images -q | wc -l) images present before install"
}

# Unpacks a downloaded bundle tar after checking its published sha256; prints the bundle dir.
unpack_bundle() { # tar dest
  local tar=$1 dest=$2
  (cd "$(dirname "$tar")" && sha256sum -c "$(basename "$tar").sha256" >&2)
  mkdir -p "$dest"
  tar -C "$dest" -xf "$tar"
  find "$dest" -mindepth 1 -maxdepth 1 -type d -name 'sutraerp-onprem-*' | head -1
}

# First tenant + the administrator and two checker accounts (TOTP enrolled at first sign-in).
bootstrap_people() {
  local pw tid
  pw="Stg-$(openssl rand -hex 10)"
  echo "::add-mask::$pw"
  printf '%s' "$pw" >"$OUT/.pw"
  chmod 600 "$OUT/.pw"
  tid=$($SO create-tenant --name "On-prem Staging College" | tail -1)
  echo "tenant $tid"
  $SO create-user --tenant "$tid" --email admin@osc.edu.in --role administrator --first Asha --last Kulkarni --password-file "$OUT/.pw" --no-temporary
  $SO create-user --tenant "$tid" --email registrar@osc.edu.in --role registrar --first Ravi --last Deshmukh --password-file "$OUT/.pw" --no-temporary
  $SO create-user --tenant "$tid" --email finance.approver@osc.edu.in --role finance_approver --first Farah --last Shaikh --password-file "$OUT/.pw" --no-temporary
  export E2E_PASSWORD=$pw
}

smoke() { # phase
  local t0 rc=0
  t0=$(now)
  (
    cd "$CI_DIR"
    E2E_AUTH=oidc E2E_BASE="https://$ERP_HOST" E2E_PASSWORD="${E2E_PASSWORD:-$(cat "$OUT/.pw")}" \
      E2E_ADMIN_LOGIN=admin@osc.edu.in E2E_REGISTRAR_LOGIN=registrar@osc.edu.in \
      E2E_FINANCE_APPROVER_LOGIN=finance.approver@osc.edu.in \
      NODE_EXTRA_CA_CERTS=/etc/sutraerp/tls/local-ca/ca.crt ONPREM_STATE="$OUT/smoke-state.json" \
      node onprem-smoke.mjs "$1"
  ) 2>&1 | tee -a "$OUT/smoke.log" || rc=$?
  [[ $rc -eq 0 ]] || { echo "smoke $1 failed" >&2; return 1; }
  record "smoke_$1${SMOKE_SUFFIX:-}" "$(since "$t0")"
}

counts() { $SO counts 2>/dev/null | tail -1; }

# Business counts must be identical (audit rows may only grow).
same_data() { # before after what
  local a=${1% audit_rows=*} b=${2% audit_rows=*} ra=${1##*audit_rows=} rb=${2##*audit_rows=}
  if [[ "$a" == "$b" && $rb -ge $ra ]]; then
    result "data-kept:$3" "identical ($a; audit $ra → $rb)"
  else
    result "data-kept:$3" "MISMATCH before [$1] after [$2]"
    return 1
  fi
}

# Only the business tables that existed before (upgrade adds tables).
same_fields() { # before after what fields…
  local before=$1 after=$2 what=$3 f vb va bad=0 msg=""
  shift 3
  for f in "$@"; do
    vb=$(sed -n "s/.*\b$f=\([^ ]*\).*/\1/p" <<<"$before")
    va=$(sed -n "s/.*\b$f=\([^ ]*\).*/\1/p" <<<"$after")
    msg+="$f=$vb→$va "
    [[ "$vb" == "$va" ]] || bad=1
  done
  if [[ $bad -eq 0 ]]; then result "data-kept:$what" "identical ($msg)"; else result "data-kept:$what" "MISMATCH ($msg)"; return 1; fi
}

resources() { # label
  {
    echo "### docker stats ($1, $(date -u +%FT%TZ))"
    docker stats --no-stream --format 'table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.BlockIO}}'
    echo
    free -m
    echo
    docker system df -v | sed -n '/^Local Volumes/,/^Build cache/p'
    echo
  } | tee -a "$OUT/resources.md"
  docker stats --no-stream --format '{{.Name}}\t{{.MemUsage}}\t{{.CPUPerc}}' >"$OUT/stats-$1.tsv"
}

collect_logs() {
  mkdir -p "$OUT/logs"
  sudo cp -r /etc/sutraerp/state.env /etc/sutraerp/sutraerp.conf "$OUT/logs/" 2>/dev/null || true
  sudo chown -R "$(id -u)" "$OUT/logs" 2>/dev/null || true
  if [[ -e /opt/sutraerp/current ]]; then
    $SO status >"$OUT/logs/compose-ps.txt" 2>&1 || true
    for s in postgres keycloak valkey nats minio flowable api web edge; do
      SUTRA_LOG_TAIL=2000 $SO logs "$s" >"$OUT/logs/$s.log" 2>&1 || true
    done
  fi
  # Credentials never leave the host: the staging password and the TOTP secrets in the smoke state.
  rm -f "$OUT/.pw"
  if [[ -f "$OUT/smoke-state.json" ]]; then
    python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); d.pop("totp", None); json.dump(d, open(sys.argv[1], "w"), indent=2)' "$OUT/smoke-state.json"
  fi
}
