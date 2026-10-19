#!/usr/bin/env bash
# SutraERP backup (slice 9C) — format `sutra-backup/v1` (docs/batch9-contracts.md §11).
#
#   sutra-backup.sh [--out-dir DIR] [--tar] [--retention-days N]
#
# Writes DIR/sutra-backup-<env>-<UTC YYYYMMDDTHHMMSSZ>/ (or .tar with --tar):
#   sutra.dump      pg_dump -Fc --no-owner of the `sutra` database (ACLs KEPT: the append-only
#                   grants on platform.audit_log / audit_chain_* must survive a restore)
#   flowable.dump   same for the `flowable` database when SUTRA_BACKUP_FLOWABLE_DATABASE_URL is set
#   manifest.json   format, createdAt, environment, gitSha, migrationVersion, databases[{name, file,
#                   bytes, sha256}], auditHeads[], rowCounts{} — counts and heads are read inside
#                   the SAME snapshot pg_dump uses (pg_export_snapshot), so they match the dump even
#                   while the API keeps writing.
# Prints the backup path on the last stdout line.
#
# Env: SUTRA_BACKUP_DATABASE_URL (else DATABASE_URL), SUTRA_BACKUP_FLOWABLE_DATABASE_URL,
#      SUTRA_BACKUP_DIR (default ./backups), SUTRA_ENV (default development), SUTRA_GIT_SHA,
#      SUTRA_BACKUP_RETENTION_DAYS (delete older backups in the out dir; default 0 = keep all),
#      SUTRA_BACKUP_AGE_RECIPIENT (optional: encrypt the tarball with `age`, implies --tar).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

out_dir=${SUTRA_BACKUP_DIR:-./backups}
tar_it=false
retention=${SUTRA_BACKUP_RETENTION_DAYS:-0}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir) out_dir=$2; shift 2 ;;
    --tar) tar_it=true; shift ;;
    --retention-days) retention=$2; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ -n "${SUTRA_BACKUP_AGE_RECIPIENT:-}" ]] && tar_it=true

need "$PSQL" "$PG_DUMP" sha256sum stat
url=$(sutra_url)
furl=$(flowable_url)
env_name=${SUTRA_ENV:-development}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
name="sutra-backup-${env_name}-${stamp}"
mkdir -p "$out_dir"
dir="$out_dir/$name"
mkdir "$dir"
trap 'rc=$?; if [[ $rc -ne 0 ]]; then rm -rf "$dir"; fi' EXIT

# --- one REPEATABLE READ snapshot: counts + heads + version, then pg_dump --snapshot ----------
coproc PSQLCO { "$PSQL" "$url" -qAtX -v ON_ERROR_STOP=1 2>&1; }
co_in=${PSQLCO[1]}
co_out=${PSQLCO[0]}
ans=""
ask() { # one SQL statement → its single output line in $ans (no subshell: coproc fds stay usable)
  printf '%s\n' "$1" >&"$co_in"
  IFS= read -r -t 900 ans <&"$co_out" || die "psql ended while running: ${1:0:60}…"
}
ask "BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY; SELECT pg_export_snapshot();"
snapshot=$ans
[[ "$snapshot" =~ ^[0-9A-Fa-f-]+$ ]] || die "could not export a snapshot: $snapshot"
ask "$(row_counts_sql | tr '\n' ' ')"
row_counts=$ans
[[ "$row_counts" == \{* ]] || die "row counts: $row_counts"
ask "SELECT to_regclass('platform.audit_chain_head') IS NOT NULL;"
if [[ "$ans" == t ]]; then
  ask "$(audit_heads_sql | tr '\n' ' ')"
  audit_heads=$ans
else
  audit_heads='[]'
fi
[[ "$audit_heads" == \[* ]] || die "audit heads: $audit_heads"
ask "SELECT to_regclass('public._sqlx_migrations') IS NOT NULL;"
if [[ "$ans" == t ]]; then
  ask "$(migration_version_sql)"
  mig_version=$ans
else
  mig_version=0
fi
[[ "$mig_version" =~ ^[0-9]+$ ]] || die "migration version: $mig_version"
log "snapshot $snapshot: migration $mig_version"

"$PG_DUMP" "$url" -Fc --no-owner --snapshot="$snapshot" -f "$dir/sutra.dump" \
  || die "pg_dump sutra failed"
printf 'COMMIT;\n\\q\n' >&"$co_in"
wait "$PSQLCO_PID" 2>/dev/null || true
ok "dump sutra ($(file_bytes "$dir/sutra.dump") bytes)"

dbs_json="[{\"name\": \"$(db_name_of "$url")\", \"file\": \"sutra.dump\", \"bytes\": $(file_bytes "$dir/sutra.dump"), \"sha256\": \"$(file_sha256 "$dir/sutra.dump")\"}"
if [[ -n "$furl" ]]; then
  "$PG_DUMP" "$furl" -Fc --no-owner -f "$dir/flowable.dump" || die "pg_dump flowable failed"
  dbs_json+=", {\"name\": \"$(db_name_of "$furl")\", \"file\": \"flowable.dump\", \"bytes\": $(file_bytes "$dir/flowable.dump"), \"sha256\": \"$(file_sha256 "$dir/flowable.dump")\"}"
  ok "dump flowable ($(file_bytes "$dir/flowable.dump") bytes)"
fi
dbs_json+="]"

# --- manifest (pretty JSON rendered by PostgreSQL; no jq needed) -------------------------------
q "$url" \
  -v env_name="$env_name" -v sha="${SUTRA_GIT_SHA:-unknown}" \
  -v mig="$mig_version" -v dbs="$dbs_json" -v heads="$audit_heads" -v counts="$row_counts" \
  >"$dir/manifest.json" <<'SQL'
SELECT jsonb_pretty(jsonb_build_object(
  'format', 'sutra-backup/v1',
  'createdAt', to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'), 'environment', :'env_name',
  'gitSha', :'sha', 'migrationVersion', (:'mig')::bigint, 'databases', (:'dbs')::jsonb,
  'auditHeads', (:'heads')::jsonb, 'rowCounts', (:'counts')::jsonb));
SQL
tables=$(q "$url" -v counts="$row_counts" <<'SQL'
SELECT count(*) FROM jsonb_object_keys((:'counts')::jsonb);
SQL
)
ok "manifest ($tables tables, $(q "$url" -v heads="$audit_heads" <<'SQL'
SELECT jsonb_array_length((:'heads')::jsonb);
SQL
) audit heads, migration $mig_version)"

result=$dir
if $tar_it; then
  tar -C "$out_dir" -cf "$out_dir/$name.tar" "$name"
  rm -rf "$dir"
  result="$out_dir/$name.tar"
  if [[ -n "${SUTRA_BACKUP_AGE_RECIPIENT:-}" ]]; then
    need age
    age -r "$SUTRA_BACKUP_AGE_RECIPIENT" -o "$result.age" "$result"
    rm -f "$result"
    result="$result.age"
  fi
  ok "archive $(basename "$result")"
fi
trap - EXIT

if [[ "$retention" =~ ^[0-9]+$ && "$retention" -gt 0 ]]; then
  find "$out_dir" -maxdepth 1 -name 'sutra-backup-*' -mtime "+$retention" -print -exec rm -rf {} + >&2 || true
fi
echo "$result"
