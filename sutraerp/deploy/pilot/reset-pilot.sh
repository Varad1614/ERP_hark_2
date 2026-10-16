#!/usr/bin/env bash
# Part 2 · Stage 6 (S6-pilot) — reset the SYNTHETIC pilot tenant so deploy/pilot/seed-pilot.mjs can
# rebuild it from nothing (rehearsals, UAT dry runs, demo resets).
#
# DESTRUCTIVE and deliberately narrow. It deletes every row of ONE tenant (every table that has a
# `tenant_id` column, all schemas, audit chain included — the chain is per tenant) plus the child
# rows without a tenant_id that referenced those rows, in ONE transaction, and verifies that
# nothing of the tenant is left before it commits. It never touches another tenant.
#
#   bash deploy/pilot/reset-pilot.sh --dry-run --confirm <tenant-id>   # counts only, rolled back
#   bash deploy/pilot/reset-pilot.sh --confirm <tenant-id>             # reset
#
# Refuses (exit 2) when:
#   * SUTRA_ENV=production;
#   * --confirm is missing or differs from the tenant (PILOT_TENANT_ID, default the fixed pilot
#     tenant 0192f5a0-7c3e-7000-8000-000000009110);
#   * the tenant holds an institution whose code is not in PILOT_RESET_INSTITUTION_CODES (comma
#     list, default PILOT) — i.e. anything that is not the synthetic pilot institution;
#   * the database role is not a superuser (the reset needs session_replication_role=replica to
#     bypass append-only / FK triggers for this tenant's rows, and superuser also guarantees RLS
#     cannot hide rows of other tenants from the orphan check).
#
# Environment: DATABASE_URL (default postgres://sutra:sutra@localhost:5432/sutra), PILOT_TENANT_ID,
# PILOT_RESET_INSTITUTION_CODES, SUTRA_ENV. Output: one "ok <check>" line per step.
# The API needs no restart: its advisory cache is keyed by record id and re-seeding creates new ids.
# This is NOT a backup/restore tool and must never be pointed at a real institution's tenant.
set -euo pipefail

DRY=0
CONFIRM=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --confirm) CONFIRM="${2:-}"; shift 2 ;;
    *) echo "reset-pilot: unknown argument $1" >&2; exit 2 ;;
  esac
done

DB="${DATABASE_URL:-postgres://sutra:sutra@localhost:5432/sutra}"
TENANT="${PILOT_TENANT_ID:-0192f5a0-7c3e-7000-8000-000000009110}"
CODES="${PILOT_RESET_INSTITUTION_CODES:-PILOT}"

if [[ "${SUTRA_ENV:-}" == "production" ]]; then
  echo "reset-pilot: refused — SUTRA_ENV=production" >&2
  exit 2
fi
if ! [[ "$TENANT" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
  echo "reset-pilot: refused — PILOT_TENANT_ID is not a uuid ($TENANT)" >&2
  exit 2
fi
if [[ "$CONFIRM" != "$TENANT" ]]; then
  echo "reset-pilot: refused — pass --confirm $TENANT to reset that tenant" >&2
  exit 2
fi
command -v psql >/dev/null || { echo "reset-pilot: psql is required" >&2; exit 1; }
q() { psql "$DB" -qtAX -v ON_ERROR_STOP=1 "$@"; }

SUPER="$(q -c "SELECT rolsuper FROM pg_roles WHERE rolname = current_user")"
if [[ "$SUPER" != "t" ]]; then
  echo "reset-pilot: refused — role $(q -c 'SELECT current_user') is not a superuser" >&2
  exit 2
fi
echo "ok reset-guard-role superuser"

FOREIGN="$(q -v tenant="$TENANT" -v codes="$CODES" <<'SQL'
SELECT coalesce(string_agg(code, ',' ORDER BY code), '')
  FROM institution.institution
 WHERE tenant_id = :'tenant'::uuid
   AND upper(code) <> ALL (string_to_array(upper(:'codes'), ','));
SQL
)"
if [[ -n "$FOREIGN" ]]; then
  echo "reset-pilot: refused — tenant $TENANT holds non-synthetic institution(s): $FOREIGN" >&2
  exit 2
fi
echo "ok reset-guard-institutions only [$CODES] in tenant $TENANT"

END="COMMIT"
[[ "$DRY" == "1" ]] && END="ROLLBACK"
OUT="$(q -v tenant="$TENANT" 2>&1 <<SQL
BEGIN;
SET LOCAL session_replication_role = replica;
SET LOCAL row_security = off;
SELECT set_config('sutra_reset.tenant', :'tenant', true) IS NOT NULL;
DO \$reset\$
DECLARE
    t uuid := current_setting('sutra_reset.tenant')::uuid;
    r record;
    n bigint;
    o bigint;
    total bigint := 0;
    tables integer := 0;
    pass integer := 0;
BEGIN
    FOR r IN
        SELECT c.oid::regclass AS tbl
          FROM pg_class c
          JOIN pg_namespace ns ON ns.oid = c.relnamespace
          JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id'
                             AND a.attnum > 0 AND NOT a.attisdropped
         WHERE c.relkind IN ('r', 'p')
           AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
           AND ns.nspname NOT LIKE 'pg\_%'
         ORDER BY 1
    LOOP
        EXECUTE format('DELETE FROM %s WHERE tenant_id = \$1', r.tbl) USING t;
        GET DIAGNOSTICS n = ROW_COUNT;
        IF n > 0 THEN
            RAISE NOTICE 'reset: % row(s) in %', n, r.tbl;
            tables := tables + 1;
            total := total + n;
        END IF;
    END LOOP;
    -- Child rows of tables WITHOUT a tenant_id that referenced the deleted rows (FK triggers were
    -- off). Superuser + row_security=off: the parent lookup sees every tenant's rows, so only real
    -- orphans go.
    LOOP
        pass := pass + 1;
        o := 0;
        FOR r IN
            SELECT con.conrelid::regclass AS child, con.confrelid::regclass AS parent,
                   (SELECT string_agg(format('c.%I', a.attname), ', ' ORDER BY k.i)
                      FROM unnest(con.conkey) WITH ORDINALITY k(num, i)
                      JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.num) AS ccols,
                   (SELECT string_agg(format('p.%I', a.attname), ', ' ORDER BY k.i)
                      FROM unnest(con.confkey) WITH ORDINALITY k(num, i)
                      JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.num) AS pcols,
                   (SELECT string_agg(format('c.%I IS NOT NULL', a.attname), ' AND ' ORDER BY k.i)
                      FROM unnest(con.conkey) WITH ORDINALITY k(num, i)
                      JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.num) AS nn
              FROM pg_constraint con
             WHERE con.contype = 'f'
               AND NOT EXISTS (SELECT 1 FROM pg_attribute x
                                WHERE x.attrelid = con.conrelid AND x.attname = 'tenant_id'
                                  AND x.attnum > 0 AND NOT x.attisdropped)
        LOOP
            EXECUTE format('DELETE FROM %s c WHERE %s AND NOT EXISTS (SELECT 1 FROM %s p WHERE (%s) = (%s))',
                           r.child, r.nn, r.parent, r.pcols, r.ccols);
            GET DIAGNOSTICS n = ROW_COUNT;
            IF n > 0 THEN
                RAISE NOTICE 'reset: % orphaned row(s) of % in %', n, r.parent, r.child;
                o := o + n;
            END IF;
        END LOOP;
        total := total + o;
        EXIT WHEN o = 0 OR pass >= 10;
    END LOOP;
    -- Verify: nothing of the tenant is left.
    FOR r IN
        SELECT c.oid::regclass AS tbl
          FROM pg_class c
          JOIN pg_namespace ns ON ns.oid = c.relnamespace
          JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id'
                             AND a.attnum > 0 AND NOT a.attisdropped
         WHERE c.relkind IN ('r', 'p')
           AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
           AND ns.nspname NOT LIKE 'pg\_%'
    LOOP
        EXECUTE format('SELECT count(*) FROM %s WHERE tenant_id = \$1', r.tbl) INTO n USING t;
        IF n > 0 THEN
            RAISE EXCEPTION 'reset: % row(s) of the tenant left in %', n, r.tbl;
        END IF;
    END LOOP;
    RAISE NOTICE 'reset: total % row(s) from % tenant table(s), orphan passes %', total, tables, pass;
END
\$reset\$;
$END;
SQL
)" || { echo "$OUT" >&2; echo "reset-pilot: failed (rolled back)" >&2; exit 1; }
echo "$OUT" | sed -n 's/^.*NOTICE:  reset: /  reset: /p'
TOTAL="$(echo "$OUT" | sed -n 's/^.*reset: total \([0-9]*\) row.*/\1/p' | tail -1)"
if [[ "$DRY" == "1" ]]; then
  echo "ok reset-dry-run would delete ${TOTAL:-0} row(s) (rolled back)"
  exit 0
fi
LEFT="$(q -v tenant="$TENANT" <<'SQL'
SELECT (SELECT count(*) FROM institution.institution WHERE tenant_id = :'tenant'::uuid)
     + (SELECT count(*) FROM student.student WHERE tenant_id = :'tenant'::uuid)
     + (SELECT count(*) FROM platform.audit_log WHERE tenant_id = :'tenant'::uuid);
SQL
)"
[[ "$LEFT" == "0" ]] || { echo "reset-pilot: $LEFT row(s) still visible after commit" >&2; exit 1; }
echo "ok reset-committed ${TOTAL:-0} row(s) deleted; tenant $TENANT is empty"
