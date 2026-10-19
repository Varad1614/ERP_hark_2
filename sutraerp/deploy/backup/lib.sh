#!/usr/bin/env bash
# Shared helpers for sutra-backup.sh / sutra-restore.sh (slice 9C, docs/batch9-contracts.md §11).
# Needs only bash, coreutils (sha256sum, stat) and the PostgreSQL 18 client tools (psql, pg_dump,
# pg_restore), so the stock `postgres:18` image is a valid backup image (no jq / python): JSON is
# built and compared inside PostgreSQL.
#
# Connection: SUTRA_BACKUP_DATABASE_URL (the `sutra` database, as the schema owner / a superuser),
# else SUTRA_MIGRATION_DATABASE_URL (owner), else DATABASE_URL. Flowable: SUTRA_BACKUP_FLOWABLE_DATABASE_URL (optional; empty = no Flowable dump).
# Tool overrides: PSQL / PG_DUMP / PG_RESTORE (default: from PATH; PG_BIN=/usr/lib/postgresql/18/bin
# prepends that directory).

set -euo pipefail

if [[ -n "${PG_BIN:-}" ]]; then
  PATH="$PG_BIN:$PATH"
fi
PSQL=${PSQL:-psql}
PG_DUMP=${PG_DUMP:-pg_dump}
PG_RESTORE=${PG_RESTORE:-pg_restore}

# The application schemas (the same list as migrations/20261014000003_runtime_db_roles.sql).
SUTRA_SCHEMAS="platform institution student academic people finance campus services exams resources"

log() { printf '%s %s\n' "$(date -u +%H:%M:%SZ)" "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }
ok() { printf 'ok %s\n' "$*"; }

need() {
  local t
  for t in "$@"; do command -v "$t" >/dev/null 2>&1 || die "missing tool: $t"; done
}

sutra_url() {
  local u=${SUTRA_BACKUP_DATABASE_URL:-}
  [[ -n "$u" ]] || u=${SUTRA_MIGRATION_DATABASE_URL:-}
  [[ -n "$u" ]] || u=${DATABASE_URL:-}
  [[ -n "$u" ]] || die "set SUTRA_BACKUP_DATABASE_URL (or SUTRA_MIGRATION_DATABASE_URL / DATABASE_URL)"
  printf '%s' "$u"
}

flowable_url() { printf '%s' "${SUTRA_BACKUP_FLOWABLE_DATABASE_URL:-}"; }

# Same server, another database: replaces the path of a postgres:// URL (keeps the query string).
url_with_db() {
  local url=$1 db=$2 base query=""
  if [[ "$url" == *\?* ]]; then query="?${url#*\?}"; url=${url%%\?*}; fi
  base=${url%/*}
  printf '%s/%s%s' "$base" "$db" "$query"
}

db_name_of() {
  local url=${1%%\?*}
  printf '%s' "${url##*/}"
}

# psql -qAtX with ON_ERROR_STOP; extra args are passed through.
q() {
  local url=$1; shift
  "$PSQL" "$url" -qAtX -v ON_ERROR_STOP=1 "$@"
}

# SQL (one row, one column): JSON object {"<schema>.<table>": <exact count>} of every base table in
# the application schemas + public._sqlx_migrations. query_to_xml keeps it read-only and inside the
# caller's snapshot (no functions are created).
row_counts_sql() {
  local list="" s
  for s in $SUTRA_SCHEMAS; do list+="'$s',"; done
  list=${list%,}
  cat <<SQL
SELECT coalesce(jsonb_object_agg(t.k, t.n), '{}'::jsonb)::text FROM (
  SELECT c.table_schema || '.' || c.table_name AS k,
         (xpath('/row/n/text()', query_to_xml(
            format('SELECT count(*) AS n FROM %I.%I', c.table_schema, c.table_name),
            false, true, '')))[1]::text::bigint AS n
    FROM information_schema.tables c
   WHERE c.table_type = 'BASE TABLE'
     AND (c.table_schema IN ($list)
          OR (c.table_schema = 'public' AND c.table_name = '_sqlx_migrations'))
) t;
SQL
}

# SQL: JSON array of the audit chain heads [{tenantId, seq, headHash}] (call only when
# platform.audit_chain_head exists: `SELECT to_regclass('platform.audit_chain_head') IS NOT NULL`).
audit_heads_sql() {
  cat <<'SQL'
SELECT coalesce(jsonb_agg(jsonb_build_object('tenantId', tenant_id, 'seq', seq,
         'headHash', encode(head_hash, 'hex')) ORDER BY tenant_id), '[]'::jsonb)::text
  FROM platform.audit_chain_head;
SQL
}

# SQL: latest applied migration version (call only when public._sqlx_migrations exists).
migration_version_sql() {
  echo "SELECT coalesce(max(version), 0) FROM public._sqlx_migrations WHERE success;"
}

# SQL: the append-only privilege facts of the audit tables, one line each "<role> <table> <priv> <t|f>".
# Expected: INSERT/SELECT t, UPDATE/DELETE/TRUNCATE f for every runtime role (UPDATE t on the head).
audit_grants_sql() {
  cat <<'SQL'
SELECT r.role || ' ' || t.tbl || ' ' || p.priv || ' ' ||
       CASE WHEN has_table_privilege(r.role, t.tbl, p.priv) THEN 't' ELSE 'f' END
  FROM (VALUES ('sutra_platform'), ('sutra_app')) r(role)
  CROSS JOIN (VALUES ('platform.audit_log'), ('platform.audit_chain_entry'),
                     ('platform.audit_anchor'), ('platform.audit_chain_head')) t(tbl)
  CROSS JOIN (VALUES ('INSERT'), ('UPDATE'), ('DELETE'), ('TRUNCATE')) p(priv)
 WHERE EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r.role)
   AND to_regclass(t.tbl) IS NOT NULL
 ORDER BY 1;
SQL
}

# Checks the audit-table grants survived (append-only); prints "ok audit grants" or dies.
check_audit_grants() {
  local url=$1 line role tbl priv has bad=0 n=0
  while read -r role tbl priv has; do
    [[ -n "$role" ]] || continue
    n=$((n + 1))
    case "$priv" in
      INSERT) [[ "$has" == t ]] || { log "missing INSERT for $role on $tbl"; bad=1; } ;;
      UPDATE) if [[ "$tbl" == platform.audit_chain_head ]]; then
                [[ "$has" == t ]] || { log "missing UPDATE for $role on $tbl"; bad=1; }
              else
                [[ "$has" == f ]] || { log "$role may UPDATE append-only $tbl"; bad=1; }
              fi ;;
      DELETE|TRUNCATE) [[ "$has" == f ]] || { log "$role may $priv append-only $tbl"; bad=1; } ;;
    esac
  done < <(q "$url" -c "$(audit_grants_sql)")
  [[ $n -gt 0 ]] || die "audit grants: no runtime roles / audit tables found"
  [[ $bad -eq 0 ]] || die "audit grants changed (append-only privileges lost)"
  line="audit grants (append-only, $n privilege checks)"
  ok "$line"
}

file_bytes() { stat -c %s "$1"; }
file_sha256() { sha256sum "$1" | cut -d' ' -f1; }
