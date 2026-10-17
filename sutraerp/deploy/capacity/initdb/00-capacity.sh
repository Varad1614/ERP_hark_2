#!/usr/bin/env bash
# Part 2 · S5-capacity: first-start bootstrap of the capacity PostgreSQL (runs once on an empty
# volume, as the image's superuser). Same objects as deploy/postgres/init.sql — Keycloak's own
# login role + database, the per-owner NOLOGIN roles — with the per-run random Keycloak password
# instead of the development constant, and pg_stat_statements for the capacity report.
set -euo pipefail
: "${KEYCLOAK_DB_PASSWORD:?}"
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v kcpw="$KEYCLOAK_DB_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE keycloak LOGIN PASSWORD %L', :'kcpw') \gexec
CREATE DATABASE keycloak OWNER keycloak;
DO $$
DECLARE s text;
BEGIN
  FOREACH s IN ARRAY ARRAY['platform','institution','student','academic','people','finance','campus','services','exams','resources'] LOOP
    EXECUTE format('CREATE ROLE %I NOLOGIN', 'sutra_' || s);
  END LOOP;
END $$;
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
SQL
echo "ok capacity bootstrap (keycloak database, owner roles, pg_stat_statements)"
