#!/usr/bin/env bash
# Ships the PostgreSQL WAL archive offsite, encrypted (Part 2 · Stage 5).
#
#   sutra-wal-ship.sh [--loop SECONDS]
#
# PostgreSQL archives every completed WAL segment into SUTRA_WAL_ARCHIVE_DIR (archive_mode=on,
# archive_command in deploy/backup/host/postgresql-archive.conf; archive_timeout bounds how long a
# quiet segment stays open). This script encrypts each new segment / .history / .backup file with
# age into SUTRA_WAL_OFFSITE_DIR as <file>.age, appends its sha256 to SHA256SUMS there and a line
# "<file> <archived-at UTC> <shipped-at UTC>" to ship.log (evidence for the achieved RPO). It is
# idempotent (already shipped files are skipped) and safe to run every minute
# (deploy/backup/systemd/sutraerp-wal-ship.timer). SUTRA_WAL_OFFSITE_DIR is a second disk / NFS /
# rclone mount, or a local staging directory that `offsite.sh push-wal` pushes.
# Shipped local segments older than SUTRA_WAL_LOCAL_KEEP_MIN minutes (default 1440) are removed
# from the local archive (the offsite copy is authoritative; rotate.sh prunes it).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=set-lib.sh
. "$here/set-lib.sh"
load_backup_env   # the caller's environment wins over backup.env

src=${SUTRA_WAL_ARCHIVE_DIR:?SUTRA_WAL_ARCHIVE_DIR}
dst=${SUTRA_WAL_OFFSITE_DIR:?SUTRA_WAL_OFFSITE_DIR}
loop=0
if [[ "${1:-}" == "--loop" ]]; then loop=${2:?--loop SECONDS}; fi
mkdir -p "$dst"
touch "$dst/SHA256SUMS" "$dst/ship.log"

ship_once() {
  local f n=0 tmp keep
  while IFS= read -r f; do
    [[ "$f" =~ ^[0-9A-F]{24}$|^[0-9A-F]{8}\.history$|^[0-9A-F]{24}\.[0-9A-F]{8}\.backup$ ]] || continue
    [[ -f "$dst/$f.age" ]] && continue
    tmp="$dst/.$f.age.tmp"
    age "${AGE_ARGS[@]}" -o "$tmp" "$src/$f"
    printf '%s  %s\n' "$(sha256sum <"$tmp" | cut -d' ' -f1)" "$f.age" >>"$dst/SHA256SUMS"
    mv "$tmp" "$dst/$f.age"
    printf '%s %s %s\n' "$f" "$(date -u -r "$src/$f" +%FT%T.%3NZ)" "$(date -u +%FT%T.%3NZ)" >>"$dst/ship.log"
    n=$((n + 1))
  done < <(cd "$src" && ls -1 2>/dev/null | LC_ALL=C sort)
  keep=${SUTRA_WAL_LOCAL_KEEP_MIN:-1440}
  if [[ "$keep" =~ ^[0-9]+$ && "$keep" -gt 0 ]]; then
    while IFS= read -r f; do
      if [[ -f "$dst/$(basename "$f").age" ]]; then rm -f -- "$f"; fi
    done < <(find "$src" -maxdepth 1 -type f -mmin "+$keep" 2>/dev/null)
  fi
  if [[ $n -gt 0 ]]; then set_log "shipped $n WAL file(s) → $dst"; fi
  return 0
}

# WAL archive status for node_exporter (S5-monitor): pg_stat_archiver + newest shipped file.
archive_metrics() {
  local dir=${SUTRA_BACKUP_METRICS_DIR:-/var/lib/sutraerp/node-exporter} row tmp shipped
  [[ -n "${SUTRA_BACKUP_DATABASE_URL:-}" ]] && command -v psql >/dev/null 2>&1 || return 0
  mkdir -p "$dir" 2>/dev/null || return 0
  row=$(psql "$SUTRA_BACKUP_DATABASE_URL" -qAtX -F ' ' -c "SELECT archived_count, failed_count, coalesce(extract(epoch FROM last_archived_time)::bigint, 0), coalesce(extract(epoch FROM last_failed_time)::bigint, 0) FROM pg_stat_archiver" 2>/dev/null) || return 0
  read -r ac fc lat lft <<<"$row"
  shipped=$(stat -c %Y "$dst/SHA256SUMS" 2>/dev/null || echo 0)
  tmp=$(mktemp "$dir/.sutra_wal_archive.XXXXXX") || return 0
  {
    echo "# TYPE sutra_wal_archived_total counter"; echo "sutra_wal_archived_total $ac"
    echo "# TYPE sutra_wal_archive_failed_total counter"; echo "sutra_wal_archive_failed_total $fc"
    echo "# TYPE sutra_wal_last_archived_timestamp_seconds gauge"; echo "sutra_wal_last_archived_timestamp_seconds $lat"
    echo "# TYPE sutra_wal_last_failed_timestamp_seconds gauge"; echo "sutra_wal_last_failed_timestamp_seconds $lft"
    echo "# TYPE sutra_wal_last_shipped_timestamp_seconds gauge"; echo "sutra_wal_last_shipped_timestamp_seconds $shipped"
  } >"$tmp" && chmod 0644 "$tmp" && mv -f "$tmp" "$dir/sutra_wal_archive.prom"
  return 0
}

set_need_age
age_recipient_args
if [[ "$loop" -gt 0 ]]; then
  while :; do ship_once; sleep "$loop"; done
else
  ship_once
  archive_metrics
  printf 'ok wal-ship (%s files offsite)\n' "$(wc -l <"$dst/SHA256SUMS")"
fi
