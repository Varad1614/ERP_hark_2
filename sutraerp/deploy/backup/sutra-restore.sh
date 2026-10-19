#!/usr/bin/env bash
# SutraERP restore (slice 9C) — format `sutra-backup/v1` (docs/batch9-contracts.md §11,
# docs/runbooks/backup-dr.md).
#
#   sutra-restore.sh <backup dir | .tar | .tar.age> [--skip-migrate] [--no-rebuild]
#
# 1. verify format + sha256 / size of every dump against manifest.json
# 2. re-create the cluster roles (roles.sql) so the dumps' ACLs (append-only audit grants) apply
# 3. create the target databases when missing, `pg_restore --clean --if-exists --no-owner`
#    (ACLs restored; the flowable dump is restored as role `flowable`)
# 4. `sutra-api migrate` (idempotent; provisions SUTRA_DB_RUNTIME_USER when set)
# 5. compare rowCounts + auditHeads with the manifest, check the audit-table grants
# 6. request an OpenSearch rebuild (platform.search_checkpoint.rebuild_requested_at = now())
# One "ok <check>" line per step; exits non-zero on the first failure.
#
# Env: SUTRA_BACKUP_DATABASE_URL (else DATABASE_URL) = the target `sutra` database URL (superuser /
#      CREATEROLE + CREATEDB); SUTRA_BACKUP_FLOWABLE_DATABASE_URL = target flowable database (default:
#      same server, database named as in the manifest); SUTRA_API_BIN (default `sutra-api`);
#      SUTRA_BACKUP_AGE_IDENTITY (age identity file for .age archives).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

src=""
migrate=true
rebuild=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-migrate) migrate=false; shift ;;
    --no-rebuild) rebuild=false; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    -*) die "unknown argument: $1" ;;
    *) src=$1; shift ;;
  esac
done
[[ -n "$src" ]] || die "usage: sutra-restore.sh <backup dir | .tar | .tar.age>"
need "$PSQL" "$PG_RESTORE" sha256sum stat
url=$(sutra_url)
admin_url=$(url_with_db "$url" postgres)
sutra_db=$(db_name_of "$url")

work=""
# `if`, not `[[ … ]] && …`: the EXIT trap's last status becomes the script's exit status, so the
# short form made every successful restore of a backup DIRECTORY exit 1 (S5-deploy rollback drill).
cleanup() { if [[ -n "$work" ]]; then rm -rf "$work"; fi; }
trap cleanup EXIT
if [[ -f "$src" ]]; then
  work=$(mktemp -d)
  archive=$src
  if [[ "$src" == *.age ]]; then
    need age
    [[ -n "${SUTRA_BACKUP_AGE_IDENTITY:-}" ]] || die "set SUTRA_BACKUP_AGE_IDENTITY for $src"
    age -d -i "$SUTRA_BACKUP_AGE_IDENTITY" -o "$work/backup.tar" "$src"
    archive="$work/backup.tar"
  fi
  tar -C "$work" -xf "$archive"
  dir=$(find "$work" -mindepth 1 -maxdepth 1 -type d -name 'sutra-backup-*' | head -1)
  [[ -n "$dir" ]] || die "no sutra-backup-* directory inside $src"
else
  dir=$src
fi
[[ -f "$dir/manifest.json" ]] || die "$dir/manifest.json missing"
manifest=$(cat "$dir/manifest.json")

# --- 1. manifest + checksums -----------------------------------------------------------------
fmt=$(q "$admin_url" -v m="$manifest" <<'SQL'
SELECT (:'m')::jsonb->>'format';
SQL
)
[[ "$fmt" == "sutra-backup/v1" ]] || die "unsupported backup format: $fmt"
mig_backup=$(q "$admin_url" -v m="$manifest" <<'SQL'
SELECT coalesce((:'m')::jsonb->>'migrationVersion', '0');
SQL
)
declare -A db_file=()
while IFS='|' read -r name file bytes sha; do
  [[ -n "$name" ]] || continue
  f="$dir/$file"
  [[ -f "$f" ]] || die "$file listed in the manifest is missing"
  [[ "$(file_bytes "$f")" == "$bytes" ]] || die "$file: size $(file_bytes "$f") != manifest $bytes"
  [[ "$(file_sha256 "$f")" == "$sha" ]] || die "$file: sha256 mismatch"
  db_file[$file]=$name
done < <(q "$admin_url" -F '|' -v m="$manifest" <<'SQL'
SELECT d->>'name', d->>'file', d->>'bytes', d->>'sha256'
  FROM jsonb_array_elements((:'m')::jsonb->'databases') d;
SQL
)
[[ -n "${db_file[sutra.dump]:-}" ]] || die "manifest has no sutra.dump"
ok "checksums (${#db_file[@]} dumps, migration $mig_backup)"

# --- 2. roles --------------------------------------------------------------------------------
q "$admin_url" -f "$here/roles.sql" >/dev/null
ok "roles (roles.sql)"

# --- 3. databases + pg_restore ---------------------------------------------------------------
ensure_db() { # name owner
  local exists
  exists=$(q "$admin_url" -v d="$1" <<'SQL'
SELECT count(*) FROM pg_database WHERE datname = :'d';
SQL
)
  if [[ "$exists" == 0 ]]; then
    q "$admin_url" -v d="$1" -v o="$2" <<'SQL' >/dev/null
SELECT format('CREATE DATABASE %I OWNER %I', :'d', :'o') \gexec
SQL
  fi
}
restore_one() { # dump-file target-url [role]
  local file=$1 target=$2 role=${3:-} errlog rc=0
  errlog=$(mktemp)
  local args=(--clean --if-exists --no-owner -d "$target")
  [[ -n "$role" ]] && args+=(--role="$role")
  "$PG_RESTORE" "${args[@]}" "$dir/$file" 2>"$errlog" || rc=$?
  if [[ $rc -ne 0 ]]; then
    grep -E 'error|ERROR' "$errlog" | head -40 >&2 || true
    rm -f "$errlog"
    die "pg_restore $file failed (exit $rc)"
  fi
  rm -f "$errlog"
}
me=$(q "$admin_url" -c "SELECT current_user;")
ensure_db "$sutra_db" "$me"
restore_one sutra.dump "$url"
ok "restore sutra -> $sutra_db"
if [[ -n "${db_file[flowable.dump]:-}" ]]; then
  furl=$(flowable_url)
  [[ -n "$furl" ]] || furl=$(url_with_db "$url" "${db_file[flowable.dump]}")
  fdb=$(db_name_of "$furl")
  ensure_db "$fdb" flowable
  restore_one flowable.dump "$furl" flowable
  ok "restore flowable -> $fdb"
fi

# --- 4. migrate ------------------------------------------------------------------------------
if $migrate; then
  api_bin=${SUTRA_API_BIN:-sutra-api}
  need "$api_bin"
  DATABASE_URL=$url SUTRA_MIGRATION_DATABASE_URL=$url "$api_bin" migrate >&2 \
    || die "sutra-api migrate failed"
  ok "migrate"
fi

# --- 5. verify -------------------------------------------------------------------------------
counts_now=$(q "$url" -c "$(row_counts_sql)")
mig_now=$(q "$url" -c "$(migration_version_sql)")
diff=$(q "$url" -v m="$manifest" -v now="$counts_now" -v same="$([[ "$mig_now" == "$mig_backup" ]] && echo t || echo f)" <<'SQL'
SELECT coalesce(string_agg(format('%s: backup %s, restored %s', b.key, b.value,
                coalesce((:'now')::jsonb->>b.key, 'missing')), '; ' ORDER BY b.key), '')
  FROM jsonb_each_text((:'m')::jsonb->'rowCounts') b
 WHERE ((:'now')::jsonb->>b.key) IS DISTINCT FROM b.value
   AND NOT (b.key = 'public._sqlx_migrations' AND :'same' = 'f');
SQL
)
[[ -z "$diff" ]] || die "row counts differ: $diff"
ntables=$(q "$url" -v m="$manifest" <<'SQL'
SELECT count(*) FROM jsonb_object_keys((:'m')::jsonb->'rowCounts');
SQL
)
ok "row counts ($ntables tables equal the manifest)"

if [[ $(q "$url" -c "SELECT to_regclass('platform.audit_chain_head') IS NOT NULL;") == t ]]; then
  heads_now=$(q "$url" -c "$(audit_heads_sql)")
else
  heads_now='[]'
fi
same=$(q "$url" -v m="$manifest" -v now="$heads_now" <<'SQL'
SELECT ((:'m')::jsonb->'auditHeads') = (:'now')::jsonb;
SQL
)
[[ "$same" == t ]] || die "audit heads differ: restored $heads_now"
ok "audit heads ($(q "$url" -v now="$heads_now" <<'SQL'
SELECT jsonb_array_length((:'now')::jsonb);
SQL
) tenants equal the manifest)"
check_audit_grants "$url"

# --- 6. search rebuild -----------------------------------------------------------------------
if $rebuild && [[ $(q "$url" -c "SELECT to_regclass('platform.search_checkpoint') IS NOT NULL;") == t ]]; then
  n=$(q "$url" -c "WITH u AS (UPDATE platform.search_checkpoint SET rebuild_requested_at = now(), updated_at = now() RETURNING 1) SELECT count(*) FROM u;")
  ok "search rebuild requested ($n projections; projections without a checkpoint rebuild anyway)"
fi
echo "restored $dir"
