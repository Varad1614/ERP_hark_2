#!/usr/bin/env bash
# SutraERP full backup of a single-server (docker compose) installation — Part 2 · Stage 5.
# Wraps the slice-9C database backup (sutra-backup.sh, format sutra-backup/v1) and adds every
# other piece of state, encrypted at rest with age and checksummed:
#
#   sutra-fullbackup.sh nightly    → $SUTRA_BACKUP_DIR/sets/sutra-nightly-<UTC stamp>/
#       database.tar.age     sutra (+ flowable) pg_dump -Fc + manifest.json (row counts, audit heads)
#       keycloak.dump.age    pg_dump -Fc of the Keycloak database (users, credentials, realm config)
#       realm-sutra.json.age Keycloak partial export of realm `sutra` (clients, roles, groups) when
#                            SUTRA_KC_URL + admin credentials are set (configuration review; the
#                            database dump is what is restored)
#       objects.tar.age      every document object (bucket mirror) + documents.tsv (DB manifest:
#                            key, sha256, size, tenant, state) — verified BEFORE it is packed
#       inventory.json.age   configuration / secrets INVENTORY: compose services, images, volumes,
#                            environment variable NAMES (never values), config file checksums
#   sutra-fullbackup.sh base       → $SUTRA_BACKUP_DIR/sets/sutra-base-<UTC stamp>/
#       base.tar.gz.age, pg_wal.tar.gz.age, backup_manifest.age   pg_basebackup of the whole
#                            cluster (sutra + keycloak + flowable databases, roles) — the start
#                            of point-in-time recovery with the WAL archive (sutra-wal-ship.sh)
#   Each set: SHA256SUMS (encrypted files), PLAIN.SHA256 (plaintext), set.json. The last stdout
#   line is the set directory. Then rotation (rotate.sh) and the offsite copy (offsite.sh).
#
# Consistency: the database snapshot is taken FIRST, the object mirror second, so every document
# row in the dump has its object in the mirror (the vault stores the object before it commits the
# row); objects newer than the snapshot are harmless extras ("objects without a row").
#
# Configuration: environment, or SUTRA_BACKUP_ENV_FILE (default /etc/sutraerp/backup.env; example
# deploy/backup/host/backup.env.example). Run as root (deploy/backup/systemd/sutraerp-backup.timer).
# SUTRA_BACKUP_METRICS=<file> appends one JSON line per step (wall time) — used by the drill.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
envf=${SUTRA_BACKUP_ENV_FILE:-/etc/sutraerp/backup.env}
if [[ -f "$envf" ]]; then
  set -a
  # shellcheck disable=SC1090
  . "$envf"
  set +a
fi
# shellcheck source=lib.sh
. "$here/lib.sh"
# shellcheck source=set-lib.sh
. "$here/set-lib.sh"

kind=${1:-nightly}
root=${SUTRA_BACKUP_DIR:-/var/backups/sutraerp}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
umask 077
mkdir -p "$root/sets" "$root/work"
started=$(date +%s)

step_t0=0
step() { step_t0=$(date +%s.%N); log "── $*"; }
step_done() {
  local secs
  secs=$(awk -v a="$step_t0" -v b="$(date +%s.%N)" 'BEGIN { printf "%.1f", b - a }')
  ok "$1 (${secs}s)"
  if [[ -n "${SUTRA_BACKUP_METRICS:-}" ]]; then
    printf '{"phase":"backup-%s","step":"%s","seconds":%s}\n' "$kind" "${2:-$1}" "$secs" >>"$SUTRA_BACKUP_METRICS"
  fi
  return 0
}

case "$kind" in
nightly)
  name="sutra-nightly-$stamp"
  set_dir="$root/sets/$name"
  work=$(mktemp -d "$root/work/$name.XXXX")
  trap 'rm -rf "$work"; [[ -f "$set_dir/set.json" ]] || rm -rf "$set_dir"' EXIT
  mkdir -p "$set_dir"

  step "database (sutra-backup.sh: pg_dump -Fc in one exported snapshot + manifest)"
  db_tar=$(bash "$here/sutra-backup.sh" --out-dir "$work/db" --tar | tail -1)
  mv "$db_tar" "$work/database.tar"
  step_done "database dump ($(file_bytes "$work/database.tar") bytes)" "database dump"

  if [[ -n "${SUTRA_BACKUP_KEYCLOAK_DATABASE_URL:-}" ]]; then
    step "keycloak database (pg_dump -Fc)"
    "$PG_DUMP" "$SUTRA_BACKUP_KEYCLOAK_DATABASE_URL" -Fc --no-owner -f "$work/keycloak.dump"
    step_done "keycloak dump ($(file_bytes "$work/keycloak.dump") bytes)" "keycloak dump"
  fi

  if [[ -n "${SUTRA_KC_URL:-}" && -n "${SUTRA_KC_ADMIN_USER:-}" && -n "${SUTRA_KC_ADMIN_PASSWORD:-}" ]]; then
    step "keycloak realm export (admin REST partial-export)"
    need curl jq
    tok=$(curl -fsS -d grant_type=password -d client_id=admin-cli \
      --data-urlencode "username=$SUTRA_KC_ADMIN_USER" --data-urlencode "password=$SUTRA_KC_ADMIN_PASSWORD" \
      "$SUTRA_KC_URL/realms/master/protocol/openid-connect/token" | jq -r .access_token)
    curl -fsS -X POST -H "authorization: Bearer $tok" \
      "$SUTRA_KC_URL/admin/realms/${SUTRA_KC_REALM:-sutra}/partial-export?exportClients=true&exportGroupsAndRoles=true" \
      | jq . >"$work/realm-sutra.json"
    step_done "realm export ($(jq '.clients | length' "$work/realm-sutra.json") clients, $(jq '.roles.realm | length' "$work/realm-sutra.json") realm roles)" "realm export"
  fi

  if [[ -n "${SUTRA_S3_BUCKET:-}" ]]; then
    step "document objects (DB manifest → bucket mirror → sha256 verification)"
    mkdir -p "$work/objects-set"
    bash "$here/sutra-objects.sh" manifest "$work/objects-set/documents.tsv" >&2
    bash "$here/sutra-objects.sh" mirror "$work/objects-set" >&2
    bash "$here/sutra-objects.sh" verify "$work/objects-set" "$work/objects-set/documents.tsv" >&2
    tar -C "$work/objects-set" -cf "$work/objects.tar" documents.tsv objects
    rm -rf "$work/objects-set"
    step_done "objects ($(file_bytes "$work/objects.tar") bytes packed)" "objects mirror + verify"
  fi

  step "configuration / secrets inventory (names only)"
  bash "$here/inventory.sh" >"$work/inventory.json"
  step_done "inventory ($(jq '.services | length' "$work/inventory.json" 2>/dev/null || echo '?') services)" "inventory"

  step "encrypt (age) + checksums"
  for f in "$work"/database.tar "$work"/keycloak.dump "$work"/realm-sutra.json "$work"/objects.tar "$work"/inventory.json; do
    if [[ -f "$f" ]]; then encrypt_into "$f" "$set_dir"; fi
  done
  write_set_json "$set_dir" nightly
  step_done "encrypt $(wc -l <"$set_dir/SHA256SUMS") files ($(du -sb "$set_dir" | cut -f1) bytes)" "encrypt + checksums"
  ;;

base)
  name="sutra-base-$stamp"
  set_dir="$root/sets/$name"
  container=${SUTRA_PG_CONTAINER:?SUTRA_PG_CONTAINER (the postgres container name)}
  staging_host=${SUTRA_PG_STAGING_DIR:?SUTRA_PG_STAGING_DIR (host dir bind-mounted into the postgres container)}
  staging_ctr=${SUTRA_PG_STAGING_IN_CONTAINER:-/backup-staging}
  trap 'rm -rf "${staging_host:?}/$name"; [[ -f "$set_dir/set.json" ]] || rm -rf "$set_dir"' EXIT
  mkdir -p "$set_dir"
  step "pg_basebackup (fast checkpoint, tar + gzip, WAL streamed into the backup)"
  docker exec -u postgres "$container" pg_basebackup -U "${SUTRA_PG_SUPERUSER:-sutra}" \
    -D "$staging_ctr/$name" -Ft -z -X stream -c fast -l "$name" >&2
  label=$(docker exec -u postgres "$container" sh -c "tar -xzOf '$staging_ctr/$name/base.tar.gz' backup_label")
  start_lsn=$(sed -n 's/^START WAL LOCATION: \([^ ]*\).*/\1/p' <<<"$label")
  start_wal=$(sed -n 's/^START WAL LOCATION: .*(file \([0-9A-F]*\)).*/\1/p' <<<"$label")
  start_time=$(sed -n 's/^START TIME: //p' <<<"$label")
  step_done "base backup ($(du -sb "$staging_host/$name" | cut -f1) bytes, start $start_lsn $start_wal)" "pg_basebackup"
  step "encrypt (age) + checksums"
  for f in "$staging_host/$name"/*; do encrypt_into "$f" "$set_dir"; done
  pgv=$(docker exec "$container" postgres --version | awk '{print $3}')
  write_set_json "$set_dir" base "{\"startLsn\":\"$start_lsn\",\"startWalFile\":\"$start_wal\",\"startTime\":\"$start_time\",\"pgVersion\":\"$pgv\"}"
  step_done "encrypt $(wc -l <"$set_dir/SHA256SUMS") files ($(du -sb "$set_dir" | cut -f1) bytes)" "encrypt + checksums"
  ;;
*) die "usage: sutra-fullbackup.sh nightly|base" ;;
esac

verify_set "$set_dir" >&2
if [[ "${SUTRA_BACKUP_ROTATE:-true}" != false ]]; then
  bash "$here/rotate.sh" "$root/sets" >&2 || log "rotation failed (backup kept)"
fi
if [[ -n "${SUTRA_OFFSITE_TARGET:-}" ]]; then
  step "offsite copy → $SUTRA_OFFSITE_TARGET"
  bash "$here/offsite.sh" push "$set_dir" >&2
  step_done "offsite copy" "offsite copy"
fi
ok "$kind backup $name in $(( $(date +%s) - started ))s"
echo "$set_dir"
