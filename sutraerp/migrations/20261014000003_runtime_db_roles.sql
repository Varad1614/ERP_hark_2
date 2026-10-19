-- Batch 1D — per-schema runtime database roles (ADR-L5-0005 / ADR-L5-0036 §3.1).
--
-- Migrations run as the schema OWNER (the role in DATABASE_URL / SUTRA_MIGRATION_DATABASE_URL
-- of `sutra-api migrate`). The API serves as a separate LOGIN role that is a member of
-- `sutra_app` and therefore owns nothing and can only do what is granted here:
--
--   sutra_<schema>  NOLOGIN  DML on the tables of its own schema (+ future tables via default
--                            privileges) and INSERT on the shared publication/audit tables in
--                            `platform` (outbox, audit_log) used inside owner-local transactions.
--   sutra_app       NOLOGIN  member of every module role; the runtime login role is granted it
--                            (`sutra-api migrate` with SUTRA_DB_RUNTIME_USER/_PASSWORD, or
--                            provisioning outside the application).
--
-- Not granted to any runtime role: CREATE on any schema, ownership, TRUNCATE, REFERENCES,
-- TRIGGER, and UPDATE/DELETE on platform.audit_log (append-only at the privilege level as well
-- as by trigger). Roles are cluster-wide; every statement here is idempotent so the migration is
-- safe on clusters where deploy/postgres/init.sql (or an operator) created the roles first.
-- Creating roles needs CREATEROLE on the migrating role; where that is not available the roles
-- must be provisioned beforehand (the DO block then only checks they exist).

DO $$
DECLARE
    s text;
    schemas text[] := ARRAY['platform', 'institution', 'student', 'academic', 'people',
                            'finance', 'campus', 'services', 'exams', 'resources'];
BEGIN
    FOREACH s IN ARRAY schemas || ARRAY['app'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_' || s) THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN', 'sutra_' || s);
            EXCEPTION WHEN duplicate_object OR unique_violation THEN
                NULL; -- created concurrently by another migrating database in the cluster
            END;
        END IF;
    END LOOP;

    FOREACH s IN ARRAY schemas LOOP
        -- Membership: the runtime app role acts with every module's privileges.
        EXECUTE format('GRANT %I TO sutra_app', 'sutra_' || s);
        -- Own schema: usage + DML on existing and future tables, nothing else.
        EXECUTE format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', s);
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', s, 'sutra_' || s);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO %I', s, 'sutra_' || s);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', s, 'sutra_' || s);
        EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA %I TO %I', s, 'sutra_' || s);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT USAGE, SELECT ON SEQUENCES TO %I', s, 'sutra_' || s);
        -- Shared platform services inside owner-local transactions (ADR-L5-0007 outbox, audit).
        IF s <> 'platform' THEN
            EXECUTE format('GRANT USAGE ON SCHEMA platform TO %I', 'sutra_' || s);
            EXECUTE format('GRANT INSERT ON platform.outbox, platform.audit_log TO %I', 'sutra_' || s);
        END IF;
    END LOOP;
END $$;

-- Append-only audit evidence: no UPDATE/DELETE privilege for anyone at runtime.
REVOKE UPDATE, DELETE, TRUNCATE ON platform.audit_log FROM sutra_platform;
GRANT SELECT, INSERT ON platform.audit_log TO sutra_platform;
