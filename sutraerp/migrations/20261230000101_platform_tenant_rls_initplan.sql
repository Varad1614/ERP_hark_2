-- Part 1 S3-int (DEF-X-003 follow-up): same tenant-isolation policy, evaluated once per query.
--
-- 20261230000100 wrote the policy predicate with bare calls to platform.rls_tenant() /
-- rls_institution() / rls_system(). Those inline to current_setting(...)::uuid and were
-- re-evaluated for every row a query touched, which cost ~35 % p95 in the capacity gate
-- (quality run 38856685913). Wrapping each call in a scalar sub-select makes PostgreSQL compute
-- it once per statement as an InitPlan (and lets `tenant_id = $initplan` drive index scans).
-- The predicate is logically identical: same columns, same settings, same fail-closed NULL
-- handling, still FORCE ROW LEVEL SECURITY, still a single permissive policy per table.
-- platform.tenant_rls_gaps keeps matching (`rls_tenant()` still appears in both expressions).

CREATE OR REPLACE FUNCTION platform.apply_tenant_rls() RETURNS integer
    LANGUAGE plpgsql
    AS $fn$
DECLARE
    r record;
    inst_col text;
    expr text;
    check_expr text;
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
        expr := '((SELECT platform.rls_system()) OR (tenant_id = (SELECT platform.rls_tenant())';
        IF inst_col IS NOT NULL THEN
            expr := expr || format(
                ' AND ((SELECT platform.rls_institution()) IS NULL OR %1$I IS NULL OR %1$I = (SELECT platform.rls_institution()))',
                inst_col);
        END IF;
        expr := expr || '))';
        check_expr := expr;
        IF r.schema_name = 'student' AND r.table_name = 'student' THEN
            -- The student persona is tenant-level (one person, a context per institution): a
            -- session pinned to an institution sees the personas that have a context there.
            -- Creating a persona (its context follows in the same transaction) only needs the
            -- tenant, so WITH CHECK stays tenant-only.
            expr := '((SELECT platform.rls_system()) OR (tenant_id = (SELECT platform.rls_tenant()) AND '
                 || '((SELECT platform.rls_institution()) IS NULL OR EXISTS (SELECT 1 FROM student.student_context sc '
                 || 'WHERE sc.tenant_id = student.student.tenant_id AND sc.student_id = student.student.id '
                 || 'AND sc.institution_id = (SELECT platform.rls_institution())))))';
        END IF;
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', r.tbl);
        EXECUTE format('ALTER TABLE %s FORCE ROW LEVEL SECURITY', r.tbl);
        EXECUTE format('DROP POLICY IF EXISTS sutra_tenant_isolation ON %s', r.tbl);
        EXECUTE format(
            'CREATE POLICY sutra_tenant_isolation ON %s AS PERMISSIVE FOR ALL TO PUBLIC USING %s WITH CHECK %s',
            r.tbl, expr, check_expr);
        n := n + 1;
    END LOOP;
    RETURN n;
END
$fn$;

SELECT platform.apply_tenant_rls();
