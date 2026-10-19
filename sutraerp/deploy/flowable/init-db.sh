#!/usr/bin/env bash
# Flowable database bootstrap (slice 9C, docs/batch9-contracts.md §7): LOGIN role `flowable` and
# database `flowable` owned by it, on the SutraERP PostgreSQL server. Idempotent (re-running only
# resets the password). Used as
#   * /docker-entrypoint-initdb.d/10-flowable.sh in a postgres:18 container (POSTGRES_USER set), and
#   * the chart's `sutraerp-flowable-init-db` Job / the CI workflow (libpq env: PGHOST, PGPORT,
#     PGUSER, PGPASSWORD, or SUTRA_FLOWABLE_ADMIN_URL).
# Env: FLOWABLE_DB_PASSWORD (required), FLOWABLE_DB_USER (default flowable),
#      FLOWABLE_DB_NAME (default flowable).
set -euo pipefail
: "${FLOWABLE_DB_PASSWORD:?set FLOWABLE_DB_PASSWORD}"
user=${FLOWABLE_DB_USER:-flowable}
db=${FLOWABLE_DB_NAME:-flowable}
admin_user=${POSTGRES_USER:-postgres}
export PGUSER=${PGUSER:-$admin_user}
export PGDATABASE=${PGDATABASE:-postgres}
conn=()
[[ -n "${SUTRA_FLOWABLE_ADMIN_URL:-}" ]] && conn=("$SUTRA_FLOWABLE_ADMIN_URL")

for i in $(seq 1 60); do
  if psql "${conn[@]}" -qAtX -c 'SELECT 1' >/dev/null 2>&1; then break; fi
  [[ $i -eq 60 ]] && { echo "init-db: PostgreSQL not reachable" >&2; exit 1; }
  sleep 2
done

psql "${conn[@]}" -qAtX -v ON_ERROR_STOP=1 -v u="$user" -v p="$FLOWABLE_DB_PASSWORD" -v d="$db" <<'SQL'
SELECT format(CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'u')
                   THEN 'ALTER ROLE %I WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L'
                   ELSE 'CREATE ROLE %I WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD %L' END,
              :'u', :'p') \gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'d', :'u')
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'d') \gexec
SELECT format('ALTER DATABASE %I OWNER TO %I', :'d', :'u') \gexec
SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', :'d') \gexec
SQL
echo "ok flowable database ($db, role $user)"
