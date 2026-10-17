#!/usr/bin/env bash
# First start of an empty PostgreSQL volume (docker-entrypoint-initdb.d): Keycloak's LOGIN role +
# database (password from KEYCLOAK_DB_PASSWORD, never a default) and the per-schema NOLOGIN owner
# roles of ADR-L5-0036 (the same list as deploy/postgres/init.sql). Flowable: 20-flowable.sh.
set -euo pipefail
: "${KEYCLOAK_DB_PASSWORD:?KEYCLOAK_DB_PASSWORD is required}"
psql -v ON_ERROR_STOP=1 -qAtX --username "$POSTGRES_USER" --dbname postgres -v p="$KEYCLOAK_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE keycloak LOGIN PASSWORD %L', :'p')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'keycloak') \gexec
SELECT 'CREATE DATABASE keycloak OWNER keycloak'
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'keycloak') \gexec
REVOKE ALL ON DATABASE keycloak FROM PUBLIC;
DO $$
DECLARE s text;
BEGIN
  FOREACH s IN ARRAY ARRAY['platform','institution','student','academic','people','finance','campus','services','exams','resources'] LOOP
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_' || s) THEN
      EXECUTE format('CREATE ROLE %I NOLOGIN', 'sutra_' || s);
    END IF;
  END LOOP;
END $$;
SQL
echo "ok keycloak database + schema owner roles"
