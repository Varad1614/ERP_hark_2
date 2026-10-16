#!/usr/bin/env bash
# SutraERP restore of a single-server (docker compose) installation — Part 2 · Stage 5.
# Counterpart of sutra-fullbackup.sh / sutra-wal-ship.sh. Every command prints "ok …" lines and
# stops at the first failure.
#
#   sutra-fullrestore.sh verify  <set-dir>                    checksums only
#   sutra-fullrestore.sh decrypt <set-dir> <out-dir>          verify → decrypt → plaintext checksums
#   sutra-fullrestore.sh wal     <offsite-wal-dir> <out-dir>  verify SHA256SUMS → decrypt every file
#   sutra-fullrestore.sh pgdata  <docker-volume> <base-plain-dir> <wal-plain-dir>
#                                [--target-time 'YYYY-MM-DD HH:MM:SS.ffffff+00'] [--target-name N]
#        Fills an EMPTY docker volume (mounted at /var/lib/postgresql, PGDATA 18/docker) with the
#        base backup and the WAL to replay: recovery.signal + restore_command; with a target the
#        server stops replay there and promotes (point-in-time recovery), without one it replays
#        every archived segment (recovery to the latest shipped WAL) and promotes.
#   sutra-fullrestore.sh wait-recovery <container> [timeout-s]  waits until recovery has ended;
#        prints "recovered-to <timestamp of the last replayed transaction>"
#   sutra-fullrestore.sh logical <decrypted-dir>            database.tar → sutra-restore.sh (sutra +
#        flowable, migrate, row counts / audit heads / grants vs the manifest); keycloak.dump →
#        the Keycloak database (SUTRA_BACKUP_KEYCLOAK_DATABASE_URL, restored as role keycloak)
#   sutra-fullrestore.sh objects <decrypted-dir> [--tenant T]   objects.tar → bucket (all or one
#        tenant's prefix) → download back and verify every document's sha256 against documents.tsv
#
# Env: SUTRA_BACKUP_AGE_IDENTITY (age identity file, kept OFF the server), SUTRA_BACKUP_DATABASE_URL,
#      SUTRA_BACKUP_KEYCLOAK_DATABASE_URL, SUTRA_S3_* (see sutra-objects.sh), SUTRA_API_BIN,
#      SUTRA_PG_IMAGE (postgres:18), SUTRA_PG_SUPERUSER (sutra).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=set-lib.sh
. "$here/set-lib.sh"
load_backup_env   # the caller's environment wins over backup.env
# shellcheck source=lib.sh
. "$here/lib.sh"

cmd=${1:-}
shift || true
img=${SUTRA_PG_IMAGE:-postgres:18}
su=${SUTRA_PG_SUPERUSER:-sutra}

case "$cmd" in
verify)
  verify_set "${1:?set dir}"
  ;;
decrypt)
  decrypt_set "${1:?set dir}" "${2:?out dir}"
  ;;
wal)
  srcd=${1:?offsite wal dir}; out=${2:?out dir}
  [[ -n "${SUTRA_BACKUP_AGE_IDENTITY:-}" ]] || die "set SUTRA_BACKUP_AGE_IDENTITY"
  need age
  (cd "$srcd" && sha256sum --quiet --strict -c SHA256SUMS) >&2 || die "WAL archive: checksum mismatch (corrupted segment)"
  mkdir -p "$out"
  n=0
  while read -r _ f; do
    age -d -i "$SUTRA_BACKUP_AGE_IDENTITY" -o "$out/${f%.age}" "$srcd/$f" || die "$f: decryption failed"
    n=$((n + 1))
  done <"$srcd/SHA256SUMS"
  last=$(cd "$out" && ls -1 | grep -E '^[0-9A-F]{24}$' | LC_ALL=C sort | tail -1 || true)
  ok "wal $n files verified + decrypted → $out (last segment ${last:-none})"
  ;;
pgdata)
  vol=${1:?docker volume}; base=${2:?base plain dir}; walp=${3:?wal plain dir}
  shift 3
  target=""; tname=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --target-time) target=$2; shift 2 ;;
      --target-name) tname=$2; shift 2 ;;
      *) die "unknown argument $1" ;;
    esac
  done
  [[ -f "$base/base.tar.gz" ]] || die "$base/base.tar.gz missing"
  rec="restore_command = 'cp /var/lib/postgresql/wal-restore/%f %p'"
  if [[ -n "$target" ]]; then
    rec+=$'\n'"recovery_target_time = '$target'"$'\n'"recovery_target_action = 'promote'"$'\n'"recovery_target_inclusive = true"
  fi
  if [[ -n "$tname" ]]; then
    rec+=$'\n'"recovery_target_name = '$tname'"$'\n'"recovery_target_action = 'promote'"
  fi
  docker run --rm -u root -v "$vol:/var/lib/postgresql" -v "$(cd "$base" && pwd):/in-base:ro" \
    -v "$(cd "$walp" && pwd):/in-wal:ro" -e REC="$rec" --entrypoint bash "$img" -euc '
      P=/var/lib/postgresql/18/docker
      if [ -e "$P/PG_VERSION" ]; then echo "volume is not empty ($P/PG_VERSION exists)" >&2; exit 1; fi
      mkdir -p "$P" /var/lib/postgresql/wal-restore
      tar -xzf /in-base/base.tar.gz -C "$P"
      mkdir -p "$P/pg_wal"
      if [ -f /in-base/pg_wal.tar.gz ]; then tar -xzf /in-base/pg_wal.tar.gz -C "$P/pg_wal"; fi
      cp /in-wal/* /var/lib/postgresql/wal-restore/ 2>/dev/null || true
      rm -f "$P/postmaster.pid"
      printf "\n# sutra-fullrestore.sh (archive recovery)\n%s\n" "$REC" >> "$P/postgresql.auto.conf"
      touch "$P/recovery.signal"
      chown -R postgres:postgres /var/lib/postgresql
      chmod 700 "$P"
      echo "PGDATA $(du -sh "$P" | cut -f1), $(ls /var/lib/postgresql/wal-restore | wc -l) WAL files to replay"
    ' >&2
  how="replay to the end of the archive, then promote"
  [[ -n "$target" ]] && how="point in time $target, then promote"
  [[ -n "$tname" ]] && how="restore point $tname, then promote"
  ok "pgdata volume $vol prepared ($how)"
  ;;
wait-recovery)
  c=${1:?container}; t=${2:-1800}
  deadline=$(( $(date +%s) + t ))
  until docker exec "$c" psql -U "$su" -d postgres -qAtX -c 'SELECT 1' >/dev/null 2>&1; do
    [[ $(date +%s) -lt $deadline ]] || { docker logs --tail 80 "$c" >&2; die "postgres in $c did not accept connections"; }
    sleep 1
  done
  while [[ $(docker exec "$c" psql -U "$su" -d postgres -qAtX -c 'SELECT pg_is_in_recovery()') == t ]]; do
    [[ $(date +%s) -lt $deadline ]] || die "recovery in $c did not finish within ${t}s"
    sleep 1
  done
  at=$(docker exec "$c" psql -U "$su" -d postgres -qAtX -c "SELECT coalesce(pg_last_xact_replay_timestamp()::text, '')")
  docker logs "$c" 2>&1 | grep -E 'redo done|recovery stopping|last completed transaction|selected new timeline|archive recovery complete|consistent recovery state' | tail -8 >&2 || true
  ok "recovery finished in $c"
  echo "recovered-to ${at:-unknown}"
  ;;
logical)
  d=${1:?decrypted dir}
  [[ -f "$d/database.tar" ]] || die "$d/database.tar missing"
  bash "$here/sutra-restore.sh" "$d/database.tar"
  if [[ -f "$d/keycloak.dump" ]]; then
    kurl=${SUTRA_BACKUP_KEYCLOAK_DATABASE_URL:?SUTRA_BACKUP_KEYCLOAK_DATABASE_URL}
    errlog=$(mktemp)
    "$PG_RESTORE" --clean --if-exists --no-owner --role="${SUTRA_KEYCLOAK_DB_ROLE:-keycloak}" -d "$kurl" "$d/keycloak.dump" 2>"$errlog" \
      || { grep -iE 'error' "$errlog" | head -30 >&2; die "pg_restore keycloak failed"; }
    rm -f "$errlog"
    ok "restore keycloak → $(db_name_of "$kurl") ($(q "$kurl" -c "SELECT count(*) FROM user_entity") users)"
  fi
  ;;
objects)
  d=${1:?decrypted dir}; shift
  [[ -f "$d/objects.tar" ]] || die "$d/objects.tar missing"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  tar -C "$tmp" -xf "$d/objects.tar"
  bash "$here/sutra-objects.sh" verify "$tmp" "$tmp/documents.tsv" "$@"
  bash "$here/sutra-objects.sh" restore "$tmp" "$@"
  bash "$here/sutra-objects.sh" verify-live "$tmp/documents.tsv" "$@"
  ;;
*)
  die "usage: sutra-fullrestore.sh verify|decrypt|wal|pgdata|wait-recovery|logical|objects …"
  ;;
esac
