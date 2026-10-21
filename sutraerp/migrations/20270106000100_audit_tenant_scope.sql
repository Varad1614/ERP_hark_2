-- Audit C3 / M9 (SEC-P1-TENANT-AR3): the last two institution child tables without `tenant_id`
-- were protected only through their RLS-protected parents. Give them `tenant_id` +
-- `institution_id` (backfilled from the parent), so `platform.apply_tenant_rls()` enables and
-- forces RLS with the `sutra_tenant_isolation` policy on them like every other tenant table.
-- Append-only; inserts now copy both columns from the parent row (`INSERT … SELECT`).

-- ---------------------------------------------------------------- org_type_blueprint_unit_type
ALTER TABLE institution.org_type_blueprint_unit_type
    ADD COLUMN tenant_id      uuid,
    ADD COLUMN institution_id uuid;

UPDATE institution.org_type_blueprint_unit_type t
   SET tenant_id = b.tenant_id, institution_id = b.institution_id
  FROM institution.org_type_blueprint b
 WHERE b.id = t.blueprint_id;

ALTER TABLE institution.org_type_blueprint_unit_type
    ALTER COLUMN tenant_id SET NOT NULL,
    ALTER COLUMN institution_id SET NOT NULL,
    ADD CONSTRAINT org_type_blueprint_unit_type_institution_fk
        FOREIGN KEY (institution_id) REFERENCES institution.institution(id);

CREATE INDEX org_type_blueprint_unit_type_tenant_idx
    ON institution.org_type_blueprint_unit_type (tenant_id, institution_id);

-- ---------------------------------------------------------------- org_restructure_move
ALTER TABLE institution.org_restructure_move
    ADD COLUMN tenant_id      uuid,
    ADD COLUMN institution_id uuid;

UPDATE institution.org_restructure_move m
   SET tenant_id = r.tenant_id, institution_id = r.institution_id
  FROM institution.org_restructure r
 WHERE r.id = m.restructure_id;

ALTER TABLE institution.org_restructure_move
    ALTER COLUMN tenant_id SET NOT NULL,
    ALTER COLUMN institution_id SET NOT NULL,
    ADD CONSTRAINT org_restructure_move_institution_fk
        FOREIGN KEY (institution_id) REFERENCES institution.institution(id);

CREATE INDEX org_restructure_move_tenant_idx
    ON institution.org_restructure_move (tenant_id, institution_id);

COMMENT ON COLUMN institution.org_type_blueprint_unit_type.tenant_id IS
    'Audit C3/M9: copied from org_type_blueprint (RLS key).';
COMMENT ON COLUMN institution.org_restructure_move.tenant_id IS
    'Audit C3/M9: copied from org_restructure (RLS key).';

-- Forced row-level security on the two tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
