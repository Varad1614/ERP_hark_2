-- Part 1 Stage 3 (DEF-X-003 / SEC-9D-004): PostgreSQL row-level security on every tenant table.
--
-- Until now tenant isolation rested on the `tenant_id = $n` predicate of each application query;
-- a by-id statement that forgot it (DEF-C-011, E-008, F-010, G-009, H-006, I-015, J-009) could
-- read or change another tenant's row. From this migration on the database enforces it:
--
-- * every table that has a `tenant_id` column gets ROW LEVEL SECURITY **enabled and forced**
--   (forced = it applies to the table owner too; only superusers / BYPASSRLS roles skip it, and
--   the API refuses to serve as one in production — apps/api/src/main.rs check_runtime_role);
-- * one policy `sutra_tenant_isolation` FOR ALL (USING = WITH CHECK): a row is visible and
--   writable only when its tenant is the session's tenant (`sutra.tenant_id`) and, for tables
--   with an `institution_id`, when the session is not pinned to another institution
--   (`sutra.institution_id`, DEF-C-010 / B-004); or when the connection runs server-internal,
--   cross-tenant work (`sutra.rls_scope = 'system'`: session lookup and sign-in before a tenant is
--   known, outbox relay, sweepers, migrations).
-- * Nothing set (a code path that lost its context) = no rows, every write refused: fail closed.
--
-- The settings are written by the API's connection pool from the authenticated server-side
-- session (crates/kernel/src/db_context.rs, crates/persistence/src/pool.rs), never from a
-- request header, path or body, and are cleared whenever a connection returns to the pool.
--
-- `platform.apply_tenant_rls()` is idempotent: a later migration that creates a tenant table
-- ends with `SELECT platform.apply_tenant_rls();`. deploy/ci/db-integrity.sql and the DB test
-- db_p1_s3_tenant.rs fail when a table with `tenant_id` lacks forced RLS or the policy
-- (`platform.tenant_rls_gaps`).

CREATE OR REPLACE FUNCTION platform.rls_tenant() RETURNS uuid
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT NULLIF(current_setting('sutra.tenant_id', true), '')::uuid $$;

CREATE OR REPLACE FUNCTION platform.rls_institution() RETURNS uuid
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT NULLIF(current_setting('sutra.institution_id', true), '')::uuid $$;

CREATE OR REPLACE FUNCTION platform.rls_system() RETURNS boolean
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT COALESCE(current_setting('sutra.rls_scope', true), '') = 'system' $$;

COMMENT ON FUNCTION platform.rls_tenant() IS
    'Part 1 S3 (DEF-X-003): tenant of the database session, from the authenticated API session.';
COMMENT ON FUNCTION platform.rls_institution() IS
    'Part 1 S3 (DEF-C-010): institution the API session is pinned to (NULL = tenant-wide).';
COMMENT ON FUNCTION platform.rls_system() IS
    'Part 1 S3 (DEF-X-003): true for server-internal cross-tenant work (relay, sweepers, sign-in).';

-- (Re)applies forced RLS + the isolation policy to every table with a tenant_id column. Returns
-- the number of tables covered.
CREATE OR REPLACE FUNCTION platform.apply_tenant_rls() RETURNS integer
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    r record;
    inst_col text;
    expr text;
    n integer := 0;
BEGIN
    FOR r IN
        SELECT c.oid::regclass AS tbl, ns.nspname AS schema_name, c.relname AS table_name
          FROM pg_class c
          JOIN pg_namespace ns ON ns.oid = c.relnamespace
          JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id'
                             AND a.attnum > 0 AND NOT a.attisdropped
         WHERE c.relkind IN ('r', 'p')
           AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
           AND ns.nspname NOT LIKE 'pg\_%'
         ORDER BY 2, 3
    LOOP
        inst_col := NULL;
        IF r.schema_name = 'institution' AND r.table_name = 'institution' THEN
            inst_col := 'id'; -- the institution row itself
        ELSIF EXISTS (SELECT 1 FROM pg_attribute
                       WHERE attrelid = r.tbl AND attname = 'institution_id'
                         AND attnum > 0 AND NOT attisdropped) THEN
            inst_col := 'institution_id';
        END IF;
        expr := '(platform.rls_system() OR (tenant_id = platform.rls_tenant()';
        IF inst_col IS NOT NULL THEN
            expr := expr || format(
                ' AND (platform.rls_institution() IS NULL OR %1$I IS NULL OR %1$I = platform.rls_institution())',
                inst_col);
        END IF;
        expr := expr || '))';
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', r.tbl);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', r.tbl);
        EXECUTE format('DROP POLICY IF EXISTS sutra_tenant_isolation ON %s', r.tbl);
        EXECUTE format(
            'CREATE POLICY sutra_tenant_isolation ON %s AS PERMISSIVE FOR ALL TO PUBLIC USING %s WITH CHECK %s',
            r.tbl, expr, expr);
        n := n + 1;
    END LOOP;
    RETURN n;
END
$fn$;

COMMENT ON FUNCTION platform.apply_tenant_rls() IS
    'Part 1 S3 (DEF-X-003): enable + force RLS and (re)create sutra_tenant_isolation on every tenant table.';

-- Tenant tables lacking forced RLS or the isolation policy, or carrying any other policy that
-- could widen it (empty = healthy). Used by deploy/ci/db-integrity.sql and db_p1_s3_tenant.rs.
CREATE OR REPLACE VIEW platform.tenant_rls_gaps AS
SELECT t.schema_name, t.table_name, t.rls_enabled, t.rls_forced, t.has_policy, t.other_policies
  FROM (
    SELECT ns.nspname::text AS schema_name,
           c.relname::text AS table_name,
           c.relrowsecurity AS rls_enabled,
           c.relforcerowsecurity AS rls_forced,
           EXISTS (SELECT 1 FROM pg_policy p
                    WHERE p.polrelid = c.oid AND p.polname = 'sutra_tenant_isolation'
                      AND p.polcmd = '*' AND p.polpermissive
                      AND pg_get_expr(p.polqual, p.polrelid) LIKE '%rls_tenant()%'
                      AND pg_get_expr(p.polwithcheck, p.polrelid) LIKE '%rls_tenant()%') AS has_policy,
           (SELECT count(*) FROM pg_policy p
             WHERE p.polrelid = c.oid AND p.polname <> 'sutra_tenant_isolation')::int AS other_policies
      FROM pg_class c
      JOIN pg_namespace ns ON ns.oid = c.relnamespace
      JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id'
                         AND a.attnum > 0 AND NOT a.attisdropped
     WHERE c.relkind IN ('r', 'p')
       AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
       AND ns.nspname NOT LIKE 'pg\_%'
  ) t
 WHERE NOT (t.rls_enabled AND t.rls_forced AND t.has_policy AND t.other_policies = 0);

COMMENT ON VIEW platform.tenant_rls_gaps IS
    'Part 1 S3 (DEF-X-003): tenant tables lacking forced RLS / sutra_tenant_isolation, or with extra policies.';

SELECT platform.apply_tenant_rls();
