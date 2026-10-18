-- SutraERP monitoring role for postgres_exporter (Part 2 S5-monitor). Run once as a superuser:
--   psql -v monitor_password="'<random>'" -f deploy/monitoring/postgres/monitor-role.sql sutra
-- pg_monitor gives read access to the statistics views only (no table data). pg_stat_statements
-- needs `shared_preload_libraries = 'pg_stat_statements'` in postgresql.conf (restart) first.
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_monitor') THEN
    CREATE ROLE sutra_monitor LOGIN;
  END IF;
END $$;
ALTER ROLE sutra_monitor WITH LOGIN PASSWORD :monitor_password CONNECTION LIMIT 5;
GRANT pg_monitor TO sutra_monitor;
GRANT CONNECT ON DATABASE sutra TO sutra_monitor;
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
