-- SutraERP cluster roles, re-created BEFORE pg_restore (slice 9C, docs/runbooks/backup-dr.md).
--
-- Roles are cluster-wide, so a database dump never contains them, but the dumps keep their ACLs
-- (no --no-privileges): the GRANT / REVOKE lines that make platform.audit_log and the audit chain
-- tables append-only name these roles and fail to restore (silently losing the append-only
-- privileges) when the roles do not exist. Idempotent; run as a role with CREATEROLE:
--
--   psql "$SUTRA_BACKUP_DATABASE_URL" -v ON_ERROR_STOP=1 -f roles.sql
--
-- Same list as deploy/postgres/init.sql + migrations/20261014000003_runtime_db_roles.sql. The
-- runtime LOGIN role (SUTRA_DB_RUNTIME_USER, e.g. sutra_api) is provisioned with its password by
-- `sutra-api migrate` after the restore; Flowable's login role by deploy/flowable/init-db.sh.
DO $$
DECLARE
    s text;
BEGIN
    FOREACH s IN ARRAY ARRAY['platform', 'institution', 'student', 'academic', 'people', 'finance',
                             'campus', 'services', 'exams', 'resources', 'app'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_' || s) THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN', 'sutra_' || s);
            EXCEPTION WHEN duplicate_object OR unique_violation THEN
                NULL;
            END;
        END IF;
    END LOOP;
    -- sutra_app acts with every module role's privileges (as the 1D migration grants it).
    FOREACH s IN ARRAY ARRAY['platform', 'institution', 'student', 'academic', 'people', 'finance',
                             'campus', 'services', 'exams', 'resources'] LOOP
        EXECUTE format('GRANT %I TO sutra_app', 'sutra_' || s);
    END LOOP;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'flowable') THEN
        BEGIN
            CREATE ROLE flowable NOLOGIN;  -- init-db.sh sets LOGIN + password
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;
        END;
    END IF;
END $$;
